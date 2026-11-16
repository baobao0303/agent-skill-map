//! Project-local plugin runtime: load the trusted drop-in plugins under
//! `.skill-map/plugins/`, validate each extension the way skill-map's
//! loader does, and run their analyzers.
//!
//! Extension modules are JavaScript, so they are imported and called
//! through `bridge.mjs` in a `node` child process. Everything else stays
//! here: the trust and enable gates (no code is imported before both
//! pass), manifest validation against the spec schemas, `ui` payload
//! validation, the confidence fold and the error messages.

use crate::model::{Contribution, ContributionError, Issue, Link, Node};
use crate::plugins::schema;
use crate::scope::Scope;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const BRIDGE: &str = include_str!("bridge.mjs");
const SPEC_BASE: &str = "https://github.com/crystian/skill-map/blob/main/spec";

/// Folder → kind, in the loader's discovery order.
const KIND_DIRS: &[(&str, &str)] = &[
    ("providers", "provider"),
    ("extractors", "extractor"),
    ("analyzers", "analyzer"),
    ("actions", "action"),
    ("formatters", "formatter"),
    ("hooks", "hook"),
];
const DERIVED_MANIFEST_KEYS: &[&str] = &["id", "kind", "kinds", "formatId"];
const EXTENSION_JSON_KEYS: &[&str] = &["version", "description", "stability", "defaultEnabled"];

#[derive(Clone, Debug)]
pub struct Extractor {
    pub plugin_id: String,
    pub id: String,
    pub scope: String,
    pub precondition_kinds: Option<Vec<String>>,
    pub precondition_providers: Option<Vec<String>>,
    pub settings_declarations: HashMap<String, Value>,
    pub resolved_settings: Value,
    pub key: String,
    /// `ui` contribution id → slot.
    pub contributions: HashMap<String, String>,
}

impl Extractor {
    pub fn qualified(&self) -> String {
        format!("{}/{}", self.plugin_id, self.id)
    }

    pub fn matches_kind(&self, kind: &str) -> bool {
        let Some(kinds) = &self.precondition_kinds else { return true; };
        if kinds.is_empty() { return true; }
        kinds.iter().any(|qualified| {
            let slash_idx = qualified.find('/');
            let kind_only = match slash_idx {
                Some(idx) => &qualified[idx + 1..],
                None => qualified.as_str(),
            };
            kind_only == kind
        })
    }

    pub fn matches_provider(&self, provider: &str) -> bool {
        let Some(providers) = &self.precondition_providers else { return true; };
        if providers.is_empty() { return true; }
        providers.iter().any(|p| p == provider)
    }
}

pub struct Analyzer {
    pub plugin_id: String,
    pub id: String,
    pub phase: String,
    pub key: String,
    pub settings_declarations: HashMap<String, Value>,
    pub resolved_settings: Value,
    /// `ui` contribution id → slot.
    pub contributions: HashMap<String, String>,
}

impl Analyzer {
    pub fn qualified(&self) -> String {
        format!("{}/{}", self.plugin_id, self.id)
    }
}

pub enum LoadedExtension {
    Extractor(Extractor),
    Analyzer(Analyzer),
}

struct Bridge {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Bridge {
    fn spawn() -> Option<Bridge> {
        let mut child = Command::new("node")
            .arg("--input-type=module")
            .arg("-e")
            .arg(BRIDGE)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .ok()?;
        let stdin = child.stdin.take()?;
        let stdout = BufReader::new(child.stdout.take()?);
        Some(Bridge { child, stdin, stdout })
    }

    fn call(&mut self, req: &Value) -> Option<Value> {
        let mut line = serde_json::to_string(req).ok()?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).ok()?;
        self.stdin.flush().ok()?;
        let mut resp = String::new();
        self.stdout.read_line(&mut resp).ok()?;
        serde_json::from_str(&resp).ok()
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct Runtime {
    bridge: Option<Bridge>,
    /// Ids of the trusted project-local plugins whose code is loaded.
    pub executed: Vec<String>,
    pub extractors: Vec<Extractor>,
    pub analyzers: Vec<Analyzer>,
    pub registered_contribution_keys: HashSet<String>,
}

/// Results of one phase of plugin analyzers.
#[derive(Default)]
pub struct PhaseOutput {
    /// (qualified analyzer id, issue).
    pub issues: Vec<(String, Issue)>,
    /// Per link index: (pluginId, extensionId, op kind, value).
    pub ops: Vec<Vec<(String, String, String, f64)>>,
    pub contributions: Vec<Contribution>,
    pub contribution_errors: Vec<ContributionError>,
}

#[derive(Default)]
pub struct ExtractorOutput {
    pub links: Vec<Link>,
    pub enrichments: Vec<(String, Value)>,
    pub contributions: Vec<Contribution>,
    pub contribution_errors: Vec<ContributionError>,
    pub signals: Vec<Value>,
    pub virtual_nodes: Vec<Node>,
}

fn plugin_failure(plugin_id: &str, status: &str, reason: &str) {
    eprintln!("plugin {plugin_id} ({status}), all extensions skipped: {reason}");
}

/// `kernel/util/logger.ts` at its default `warn` level: a local-time
/// `HH:MM:SS  ⚠ WARN   message` line on stderr.
fn warn_log(message: &str) {
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    eprintln!(
        "{:02}:{:02}:{:02}  \u{26A0} WARN   {}",
        now.hour(),
        now.minute(),
        now.second(),
        crate::cli::format::sanitize(message)
    );
}

/// Resolved settings of a bundled (core) extension: its declarations from the
/// built-in catalog, read through the same override layers as project plugins.
pub fn resolve_builtin_settings(scope: &Scope, plugin_id: &str, ext_id: &str) -> Value {
    let decls = crate::jobs::catalog::BUILT_INS
        .iter()
        .find(|m| m.get("pluginId").and_then(Value::as_str) == Some(plugin_id) && m.get("id").and_then(Value::as_str) == Some(ext_id))
        .and_then(|m| m.get("settings"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let overrides = read_extension_settings_overrides(scope, plugin_id, ext_id);
    resolve_extension_settings(plugin_id, ext_id, &decls, &overrides)
}

/// `kinds/<k>/kind.json` against the provider-kind shape (no code runs).
fn check_provider_kinds(path: &Path, plugin_id: &str) -> Option<String> {
    let kinds_dir = path.join("kinds");
    let mut entries: Vec<_> = std::fs::read_dir(&kinds_dir).ok()?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for k in entries {
        let k_name = k.file_name().to_string_lossy().to_string();
        let Ok(txt) = std::fs::read_to_string(k.path().join("kind.json")) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&txt) else { continue };
        let mut errors = Vec::new();
        match v.as_object() {
            None => errors.push("(root) must be object"),
            Some(obj) => {
                if !obj.contains_key("ui") {
                    errors.push("(root) must have required property 'ui'");
                }
                if obj.keys().any(|key| !matches!(key.as_str(), "ui" | "identifiers" | "identifierMismatch")) {
                    errors.push("(root) must NOT have additional properties");
                }
            }
        }
        if !errors.is_empty() {
            let provider = std::fs::read_dir(path.join("providers"))
                .ok()
                .and_then(|mut d| d.next())
                .and_then(Result::ok)
                .map(|p| p.file_name().to_string_lossy().to_string())
                .unwrap_or_else(|| plugin_id.to_string());
            return Some(format!(
                "Provider kind `{k_name}` (declared at `providers/{provider}/index.js`) failed validation in `kinds/{k_name}/kind.json`: {}. See {SPEC_BASE}/schemas/extensions/provider-kind.schema.json.",
                errors.join("; ")
            ));
        }
    }
    None
}

fn strip_activity_runtime_fields(manifest: &mut Value) {
    if let Some(act) = manifest.get_mut("activity").and_then(Value::as_object_mut) {
        act.remove("pluginHooksSource");
        act.remove("mapEvent");
    }
}

fn fail(reason: impl Into<String>) -> Result<(), String> {
    Err(reason.into())
}

fn validate_setting_value(declaration: &Value, value: &Value) -> Result<(), String> {
    let decl_type = declaration.get("type").and_then(Value::as_str).unwrap_or("");
    match decl_type {
        "string-list" => {
            let Some(arr) = value.as_array() else {
                return fail("expected an array of strings");
            };
            if !arr.iter().all(Value::is_string) {
                return fail("expected an array of strings");
            }
            if let Some(min) = declaration.get("min").and_then(Value::as_u64) {
                if (arr.len() as u64) < min {
                    return fail(format!("expected at least {min} item(s)"));
                }
            }
            if let Some(max) = declaration.get("max").and_then(Value::as_u64) {
                if (arr.len() as u64) > max {
                    return fail(format!("expected at most {max} item(s)"));
                }
            }
            let cap = declaration.get("itemMaxLength").and_then(Value::as_u64).unwrap_or(256) as usize;
            if arr.iter().any(|item| item.as_str().map_or(0, |s| s.len()) > cap) {
                return fail(format!("item exceeds {cap} characters"));
            }
            Ok(())
        }
        "single-string" => {
            let Some(s) = value.as_str() else {
                return fail("expected a string");
            };
            if let Some(min) = declaration.get("minLength").and_then(Value::as_u64) {
                if (s.len() as u64) < min {
                    return fail(format!("expected at least {min} characters"));
                }
            }
            if let Some(max) = declaration.get("maxLength").and_then(Value::as_u64) {
                if (s.len() as u64) > max {
                    return fail(format!("expected at most {max} characters"));
                }
            }
            if let Some(pat) = declaration.get("pattern").and_then(Value::as_str) {
                if let Ok(re) = fancy_regex::Regex::new(pat) {
                    if re.is_match(s).unwrap_or(false) == false {
                        return fail(format!("does not match pattern {pat}"));
                    }
                }
            }
            Ok(())
        }
        "boolean-flag" => {
            if value.is_boolean() {
                Ok(())
            } else {
                fail("expected a boolean")
            }
        }
        "integer" => {
            let Some(n) = value.as_i64() else {
                return fail("expected an integer");
            };
            if let Some(min) = declaration.get("min").and_then(Value::as_i64) {
                if n < min {
                    return fail(format!("expected >= {min}"));
                }
            }
            if let Some(max) = declaration.get("max").and_then(Value::as_i64) {
                if n > max {
                    return fail(format!("expected <= {max}"));
                }
            }
            Ok(())
        }
        "number" => {
            let Some(n) = value.as_f64() else {
                return fail("expected a finite number");
            };
            if !n.is_finite() {
                return fail("expected a finite number");
            }
            if let Some(min) = declaration.get("min").and_then(Value::as_f64) {
                if n < min {
                    return fail(format!("expected >= {min}"));
                }
            }
            if let Some(max) = declaration.get("max").and_then(Value::as_f64) {
                if n > max {
                    return fail(format!("expected <= {max}"));
                }
            }
            Ok(())
        }
        "enum-pick" => {
            let Some(s) = value.as_str() else {
                return fail("expected a string");
            };
            let options: Vec<&str> = declaration
                .get("options")
                .and_then(Value::as_array)
                .map(|opts| opts.iter().filter_map(|o| o.get("value").and_then(Value::as_str)).collect())
                .unwrap_or_default();
            if !options.contains(&s) {
                return fail(format!("expected one of: {}", options.join(", ")));
            }
            Ok(())
        }
        "enum-multipick" => {
            let Some(arr) = value.as_array() else {
                return fail("expected an array of strings");
            };
            if !arr.iter().all(Value::is_string) {
                return fail("expected an array of strings");
            }
            let options: Vec<&str> = declaration
                .get("options")
                .and_then(Value::as_array)
                .map(|opts| opts.iter().filter_map(|o| o.get("value").and_then(Value::as_str)).collect())
                .unwrap_or_default();
            let opt_set: HashSet<&str> = options.iter().copied().collect();
            for item in arr {
                let s = item.as_str().unwrap_or("");
                if !opt_set.contains(s) {
                    return fail(format!("every entry must be one of: {}", options.join(", ")));
                }
            }
            if let Some(min) = declaration.get("min").and_then(Value::as_u64) {
                if (arr.len() as u64) < min {
                    return fail(format!("expected at least {min} selection(s)"));
                }
            }
            if let Some(max) = declaration.get("max").and_then(Value::as_u64) {
                if (arr.len() as u64) > max {
                    return fail(format!("expected at most {max} selection(s)"));
                }
            }
            Ok(())
        }
        "path-glob" => {
            let multiple = declaration.get("multiple").and_then(Value::as_bool).unwrap_or(false);
            if multiple {
                let Some(arr) = value.as_array() else {
                    return fail("expected an array of glob strings");
                };
                if !arr.iter().all(Value::is_string) {
                    return fail("expected an array of glob strings");
                }
                Ok(())
            } else if value.is_string() {
                Ok(())
            } else {
                fail("expected a glob string")
            }
        }
        "regex" => {
            let Some(s) = value.as_str() else {
                return fail("expected a string");
            };
            if let Err(err) = fancy_regex::Regex::new(s) {
                return fail(format!("is not a compilable regex ({err})"));
            }
            Ok(())
        }
        "secret" => {
            if value.is_string() {
                Ok(())
            } else {
                fail("expected a string")
            }
        }
        "key-value-list" => {
            let Some(arr) = value.as_array() else {
                return fail("expected an array of { key, value } entries");
            };
            let well_shaped = arr.iter().all(|entry| {
                entry.is_object()
                    && entry.get("key").is_some_and(Value::is_string)
                    && entry.get("value").is_some_and(Value::is_string)
            });
            if !well_shaped {
                return fail("every entry must be { key: string, value: string }");
            }
            if let Some(min) = declaration.get("min").and_then(Value::as_u64) {
                if (arr.len() as u64) < min {
                    return fail(format!("expected at least {min} entr(y/ies)"));
                }
            }
            if let Some(max) = declaration.get("max").and_then(Value::as_u64) {
                if (arr.len() as u64) > max {
                    return fail(format!("expected at most {max} entr(y/ies)"));
                }
            }
            Ok(())
        }
        "match-list" => {
            let Some(arr) = value.as_array() else {
                return fail("expected an array of { type, value } entries");
            };
            for entry in arr {
                let Some(obj) = entry.as_object() else {
                    return fail("every entry must be { type: string, value: string }");
                };
                let (Some(t), Some(expr)) = (obj.get("type").and_then(Value::as_str), obj.get("value").and_then(Value::as_str)) else {
                    return fail("every entry must be { type: string, value: string }");
                };
                if !matches!(t, "literal" | "regex" | "glob") {
                    return fail(format!("entry type must be one of: literal, regex, glob (got {t})"));
                }
                if expr.is_empty() {
                    return fail("entry value must not be empty");
                }
                if expr.len() > 256 {
                    return fail("entry value exceeds 256 characters");
                }
                if expr.chars().any(|c| c.is_ascii_control() || c == '\n' || c == '\r') {
                    return fail("entry value must be a single line without control characters");
                }
                if t == "regex" {
                    if let Err(err) = fancy_regex::Regex::new(expr) {
                        return fail(format!("is not a compilable regex ({err})"));
                    }
                }
            }
            Ok(())
        }
        other => fail(format!("unknown input-type: {other}")),
    }
}

fn resolve_extension_settings(
    plugin_id: &str,
    ext_id: &str,
    declarations: &Value,
    overrides: &HashMap<String, Value>,
) -> Value {
    let Some(decls) = declarations.as_object() else {
        return json!({});
    };
    if decls.is_empty() {
        return json!({});
    }
    let mut resolved = serde_json::Map::new();
    for (setting_id, decl) in decls {
        let decl_type = decl.get("type").and_then(Value::as_str).unwrap_or("");
        if decl_type == "secret" {
            if let Some(env_var) = decl.get("envVar").and_then(Value::as_str) {
                if let Ok(from_env) = std::env::var(env_var) {
                    if !from_env.is_empty() {
                        resolved.insert(setting_id.clone(), Value::String(from_env));
                        continue;
                    }
                }
            }
        }
        let fallback = decl.get("default").cloned();
        if let Some(candidate) = overrides.get(setting_id) {
            match validate_setting_value(decl, candidate) {
                Ok(()) => {
                    resolved.insert(setting_id.clone(), candidate.clone());
                }
                Err(reason) => {
                    warn_log(&format!(
                        "Setting '{setting_id}' for extension '{plugin_id}/{ext_id}' is invalid ({reason}); falling back to the declared default."
                    ));
                    if let Some(fb) = fallback {
                        resolved.insert(setting_id.clone(), fb);
                    }
                }
            }
        } else if let Some(fb) = fallback {
            resolved.insert(setting_id.clone(), fb);
        }
    }
    Value::Object(resolved)
}

fn read_extension_settings_overrides(scope: &Scope, plugin_id: &str, ext_id: &str) -> HashMap<String, Value> {
    let mut overrides = HashMap::new();
    let local = scope.settings_local();
    let committed = scope.settings();
    for layer in [&committed, &local] {
        if let Some(settings) = layer
            .get("plugins")
            .and_then(|p| p.get(plugin_id))
            .and_then(|p| p.get("extensions"))
            .and_then(|e| e.get(ext_id))
            .and_then(|e| e.get("settings"))
            .and_then(Value::as_object)
        {
            for (k, v) in settings {
                overrides.insert(k.clone(), v.clone());
            }
        }
    }
    overrides
}

struct Entry {
    key: String,
    plugin_id: String,
    kind: &'static str,
    id: String,
    rel: String,
    file: PathBuf,
}

/// Load every trusted, enabled project-local plugin under `root`.
/// Plugin-level failures are reported on stderr exactly as skill-map does.
pub fn load(root: &Path) -> Runtime {
    let mut rt = Runtime {
        bridge: None,
        executed: Vec::new(),
        extractors: Vec::new(),
        analyzers: Vec::new(),
        registered_contribution_keys: HashSet::new(),
    };
    let plugins_dir = root.join(".skill-map").join("plugins");
    let Ok(dirs) = std::fs::read_dir(&plugins_dir) else { return rt };
    let scope = Scope::new(root.to_path_buf());
    let trusted = scope.load_plugin_trust();
    let mut dirs: Vec<_> = dirs.flatten().filter(|d| d.path().join("plugin.json").is_file()).collect();
    dirs.sort_by_key(|d| d.file_name());

    let mut per_plugin: Vec<(String, Vec<Entry>)> = Vec::new();
    for dir in dirs {
        let plugin_id = dir.file_name().to_string_lossy().to_string();
        // Trust is the security boundary: nothing of an untrusted plugin runs.
        if !trusted.contains(&plugin_id) {
            continue;
        }
        let path = dir.path();
        if let Some(reason) = check_provider_kinds(&path, &plugin_id) {
            plugin_failure(&plugin_id, "invalid-manifest", &reason);
            continue;
        }
        rt.executed.push(plugin_id.clone());
        let mut entries = Vec::new();
        for (folder, kind) in KIND_DIRS {
            let Ok(list) = std::fs::read_dir(path.join(folder)) else { continue };
            let mut list: Vec<_> = list.flatten().filter(|e| e.path().is_dir()).collect();
            list.sort_by_key(|e| e.file_name());
            for e in list {
                let id = e.file_name().to_string_lossy().to_string();
                let file = e.path().join("index.js");
                if !file.is_file() {
                    continue;
                }
                let ext_manifest: Value = std::fs::read_to_string(e.path().join("extension.json"))
                    .ok()
                    .and_then(|t| serde_json::from_str(&t).ok())
                    .unwrap_or_else(|| json!({}));
                let stability = ext_manifest.get("stability").and_then(Value::as_str);
                let default_on = ext_manifest
                    .get("defaultEnabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(!matches!(stability, Some("experimental" | "deprecated")));
                // Enable gate: a disabled extension is never imported.
                if !crate::bump::extension_enabled(&scope, &plugin_id, &id, default_on) {
                    continue;
                }
                entries.push(Entry {
                    key: format!("{plugin_id}/{folder}/{id}"),
                    plugin_id: plugin_id.clone(),
                    kind,
                    rel: format!("{folder}/{id}/index.js"),
                    id,
                    file,
                });
            }
        }
        if !entries.is_empty() {
            per_plugin.push((plugin_id, entries));
        }
    }
    if per_plugin.is_empty() {
        return rt;
    }

    let Some(mut bridge) = Bridge::spawn() else {
        for (plugin_id, _) in &per_plugin {
            plugin_failure(plugin_id, "load-error", "the `node` runtime was not found on PATH; project-local plugins are JavaScript and need it to run");
        }
        return rt;
    };

    for (plugin_id, entries) in per_plugin {
        let req = json!({
            "op": "load",
            "extensions": entries.iter().map(|e| json!({ "key": e.key, "file": e.file })).collect::<Vec<_>>(),
        });
        let Some(resp) = bridge.call(&req) else {
            plugin_failure(&plugin_id, "load-error", "the plugin runtime bridge stopped responding");
            continue;
        };
        let results = resp.get("results").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut loaded_extractors = Vec::new();
        let mut loaded_analyzers = Vec::new();
        let mut failed = false;
        for (entry, res) in entries.iter().zip(results.iter()) {
            let overrides = read_extension_settings_overrides(&scope, &entry.plugin_id, &entry.id);
            match validate_entry(entry, res, &overrides) {
                Ok(Some(LoadedExtension::Extractor(ex))) => loaded_extractors.push(ex),
                Ok(Some(LoadedExtension::Analyzer(a))) => loaded_analyzers.push(a),
                Ok(None) => {}
                Err((status, reason)) => {
                    plugin_failure(&plugin_id, status, &reason);
                    failed = true;
                    break;
                }
            }
        }
        if !failed {
            for ex in &loaded_extractors {
                for (cid, _) in &ex.contributions {
                    rt.registered_contribution_keys.insert(format!("{}/{}/{}", ex.plugin_id, ex.id, cid));
                }
            }
            for a in &loaded_analyzers {
                for (cid, _) in &a.contributions {
                    rt.registered_contribution_keys.insert(format!("{}/{}/{}", a.plugin_id, a.id, cid));
                }
            }
            rt.extractors.extend(loaded_extractors);
            rt.analyzers.extend(loaded_analyzers);
        }
    }
    rt.bridge = Some(bridge);
    rt
}

/// The loader's per-entry checks, in order. `Ok(None)`: valid but not an
/// analyzer or extractor this kernel runs.
fn validate_entry(
    entry: &Entry,
    res: &Value,
    settings_overrides: &HashMap<String, Value>,
) -> Result<Option<LoadedExtension>, (&'static str, String)> {
    let rel = &entry.rel;
    if res.get("ok") != Some(&Value::Bool(true)) {
        let err = res.get("error").and_then(Value::as_str).unwrap_or("");
        if err == "missing-kind" {
            return Err((
                "load-error",
                format!("{rel}: default export missing a string `kind` field. Expected one of: provider, extractor, analyzer, action, formatter, hook."),
            ));
        }
        return Err(("load-error", format!("{rel}: import failed: {err}")));
    }
    if let Some(declared) = res.get("pluginId").and_then(Value::as_str) {
        if declared != entry.plugin_id {
            return Err((
                "invalid-manifest",
                format!(
                    "{rel}: extension declares pluginId '{declared}' but its plugin.json declares id '{}'. Remove the explicit pluginId from the extension; the loader injects it from plugin.json#/id.",
                    entry.plugin_id
                ),
            ));
        }
    }
    let keys: Vec<&str> = res.get("keys").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let fields = |list: &[&str]| list.iter().filter(|f| keys.contains(f)).map(|f| format!("`{f}`")).collect::<Vec<_>>().join(", ");
    let redeclared = fields(DERIVED_MANIFEST_KEYS);
    if !redeclared.is_empty() {
        return Err((
            "invalid-manifest",
            format!("{rel}: extension manifest declares {redeclared}, derived from the folder layout (structure-as-truth) and not a manifest field. Remove it: id is the leaf folder, kind the parent folder, provider kinds the `kinds/` catalog, formatter formatId the formatter folder name."),
        ));
    }
    let relocated = fields(EXTENSION_JSON_KEYS);
    if !relocated.is_empty() {
        return Err((
            "invalid-manifest",
            format!("{rel}: the extension module declares {relocated}, which now live in `extension.json` beside it. Delete them from the module. They moved because the loader has to know whether an extension is enabled BEFORE importing it, and it cannot read a field out of a module without running that module first."),
        ));
    }
    let mut manifest = res.get("manifest").cloned().unwrap_or_else(|| json!({}));
    if entry.kind == "provider" {
        strip_activity_runtime_fields(&mut manifest);
        let errors = schema::validate(&schema::extension_ref("provider"), &manifest);
        if !errors.is_empty() {
            let slot_error = errors.iter().any(|e| e.instance_path.ends_with("/slot"));
            let doc = if slot_error { format!("{SPEC_BASE}/view-slots.md") } else { format!("{SPEC_BASE}/schemas/extensions/provider.schema.json") };
            return Err(("invalid-manifest", format!("{rel}: {}. See {doc}.", schema::format_errors(&errors))));
        }
        return Ok(None);
    } else {
        let errors = schema::validate(&schema::extension_ref(entry.kind), &manifest);
        if !errors.is_empty() {
            let slot_error = errors.iter().any(|e| e.instance_path.ends_with("/slot"));
            let doc = if slot_error { format!("{SPEC_BASE}/view-slots.md") } else { format!("{SPEC_BASE}/schemas/extensions/{}.schema.json", entry.kind) };
            return Err(("invalid-manifest", format!("{rel}: {}. See {doc}.", schema::format_errors(&errors))));
        }
    }

    if entry.kind == "extractor" {
        let contributions = manifest
            .get("ui")
            .and_then(Value::as_object)
            .map(|ui| {
                ui.iter()
                    .filter_map(|(id, v)| Some((id.clone(), v.get("slot")?.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let decls = manifest.get("settings").cloned().unwrap_or(json!({}));
        let resolved = resolve_extension_settings(&entry.plugin_id, &entry.id, &decls, settings_overrides);
        let precondition = manifest.get("precondition");
        let precondition_kinds = precondition
            .and_then(|p| p.get("kind"))
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Value::as_str).map(String::from).collect());
        let precondition_providers = precondition
            .and_then(|p| p.get("provider"))
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Value::as_str).map(String::from).collect());
        return Ok(Some(LoadedExtension::Extractor(Extractor {
            plugin_id: entry.plugin_id.clone(),
            id: entry.id.clone(),
            scope: manifest.get("scope").and_then(Value::as_str).unwrap_or("both").to_string(),
            precondition_kinds,
            precondition_providers,
            settings_declarations: decls.as_object().cloned().unwrap_or_default().into_iter().collect(),
            resolved_settings: resolved,
            key: entry.key.clone(),
            contributions,
        })));
    }

    if entry.kind != "analyzer" {
        return Ok(None);
    }
    let mode = manifest.get("mode").and_then(Value::as_str).unwrap_or("deterministic");
    if mode == "probabilistic" {
        return Ok(None);
    }
    let contributions = manifest
        .get("ui")
        .and_then(Value::as_object)
        .map(|ui| {
            ui.iter()
                .filter_map(|(id, v)| Some((id.clone(), v.get("slot")?.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let decls = manifest.get("settings").cloned().unwrap_or(json!({}));
    let resolved = resolve_extension_settings(&entry.plugin_id, &entry.id, &decls, settings_overrides);
    Ok(Some(LoadedExtension::Analyzer(Analyzer {
        plugin_id: entry.plugin_id.clone(),
        id: entry.id.clone(),
        phase: manifest.get("phase").and_then(Value::as_str).unwrap_or("detect").to_string(),
        key: entry.key.clone(),
        settings_declarations: decls.as_object().cloned().unwrap_or_default().into_iter().collect(),
        resolved_settings: resolved,
        contributions,
    })))
}

/// `validateLink`: a kind outside the spec's four, or a confidence outside
/// [0..1], drops the link with skill-map's warning.
fn check_link(qualified: &str, raw: &Value) -> Result<(), String> {
    const KINDS: [&str; 4] = ["invokes", "references", "mentions", "points"];
    let text = |v: &Value| match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let kind = raw.get("kind").map_or_else(|| "undefined".to_string(), text);
    if !raw.get("kind").and_then(Value::as_str).is_some_and(|k| KINDS.contains(&k)) {
        return Err(format!(
            "Extractor \"{qualified}\" emitted a link of kind \"{kind}\", which is not one of the link kinds the spec defines [{}]. Link dropped.",
            KINDS.join(", ")
        ));
    }
    if let Some(c) = raw.get("confidence") {
        let valid = c.as_f64().is_some_and(|n| (0.0..=1.0).contains(&n));
        if !valid {
            return Err(format!("Extractor {qualified} emitted a Link with confidence {} outside [0..1]; dropped.", text(c)));
        }
    }
    Ok(())
}

fn parse_link(raw: &Value) -> Option<Link> {
    let source = raw.get("source")?.as_str()?.to_string();
    let target = raw.get("target")?.as_str()?.to_string();
    let kind_str = raw.get("kind")?.as_str()?;
    let kind: &'static str = match kind_str {
        "invokes" => "invokes",
        "references" => "references",
        "mentions" => "mentions",
        "points" => "points",
        _ => return None,
    };
    // A missing confidence is `ConfidenceTier.MEDIUM`.
    let confidence = raw.get("confidence").and_then(Value::as_f64).unwrap_or(0.6);
    if !(0.0..=1.0).contains(&confidence) {
        return None;
    }
    let sources: Vec<String> = raw
        .get("sources")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
        .unwrap_or_default();
    let raw_text = raw.get("raw").and_then(Value::as_str).unwrap_or("").to_string();
    let trigger = raw.get("trigger").and_then(|t| {
        Some(crate::model::Trigger {
            original_trigger: t.get("originalTrigger")?.as_str()?.to_string(),
            normalized_trigger: t.get("normalizedTrigger")?.as_str()?.to_string(),
        })
    });
    let location = raw.get("location").and_then(|l| {
        Some(crate::model::Location {
            line: l.get("line")?.as_u64()?,
            offset: l.get("offset")?.as_u64()?,
        })
    });
    let occurrences: Vec<crate::model::Occurrence> = raw
        .get("occurrences")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|o| {
                    Some(crate::model::Occurrence {
                        extractor: o.get("extractor")?.as_str()?.to_string(),
                        original_trigger: o.get("originalTrigger")?.as_str()?.to_string(),
                        location: o.get("location").and_then(|loc| Some(crate::model::OccLocation { line: loc.get("line")?.as_u64()? })),
                        context: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let resolved_target = raw.get("resolvedTarget").and_then(Value::as_str).map(String::from);
    Some(Link {
        source,
        target,
        kind,
        confidence,
        sources,
        raw: raw_text,
        trigger,
        location,
        occurrences,
        resolved_target,
        loaded: None,
    })
}

impl Runtime {
    pub fn is_empty(&self) -> bool {
        self.analyzers.is_empty() && self.extractors.is_empty()
    }

    pub fn run_extractor_idx(
        &mut self,
        idx: usize,
        node: &Node,
        body: &str,
        frontmatter: &Value,
    ) -> ExtractorOutput {
        let extractor = self.extractors[idx].clone();
        self.run_extractor(&extractor, node, body, frontmatter)
    }

    pub fn run_extractor(
        &mut self,
        extractor: &Extractor,
        node: &Node,
        body: &str,
        frontmatter: &Value,
    ) -> ExtractorOutput {
        let mut out = ExtractorOutput::default();
        let Some(bridge) = self.bridge.as_mut() else { return out };
        let node_json = serde_json::to_value(node).unwrap_or(Value::Null);
        let req = json!({
            "op": "extract",
            "key": extractor.key,
            "node": node_json,
            "body": body,
            "frontmatter": frontmatter,
            "settings": extractor.resolved_settings,
        });
        let Some(resp) = bridge.call(&req) else { return out };
        if let Some(e) = resp.get("error").and_then(Value::as_str) {
            eprintln!("⚠  Extractor \"{}\" threw during extract: {e}", extractor.qualified());
        }
        if let Some(links) = resp.get("links").and_then(Value::as_array) {
            for raw_link in links {
                if let Err(message) = check_link(&extractor.qualified(), raw_link) {
                    eprintln!("⚠  {message}");
                    continue;
                }
                if let Some(mut link) = parse_link(raw_link) {
                    // An explicit empty `sources` is kept (schema-violation reports it).
                    // Without the key, sm crashes; fall back to the extractor id.
                    if raw_link.get("sources").is_none() {
                        link.sources = vec![extractor.id.clone()];
                    }
                    out.links.push(link);
                }
            }
        }
        if let Some(enrichments) = resp.get("enrichments").and_then(Value::as_array) {
            for enr in enrichments {
                out.enrichments.push((extractor.qualified(), enr.clone()));
            }
        }
        if let Some(contribs) = resp.get("contributions").and_then(Value::as_array) {
            for c in contribs {
                match check_contribution_internal(&extractor.plugin_id, &extractor.id, &extractor.contributions, c) {
                    Ok(contrib) => out.contributions.push(contrib),
                    Err(err) => out.contribution_errors.push(err),
                }
            }
        }
        out
    }

    /// Run the plugin analyzers of the given phases (in phase, then
    /// discovery, order) over the graph.
    pub fn run_phase(&mut self, phases: &[&str], nodes: &[Node], links: &[Link], cwd: &Path) -> PhaseOutput {
        let mut out = PhaseOutput {
            issues: Vec::new(),
            ops: vec![Vec::new(); links.len()],
            contributions: Vec::new(),
            contribution_errors: Vec::new(),
        };
        let Some(bridge) = self.bridge.as_mut() else { return out };
        let nodes_json = serde_json::to_value(nodes).unwrap_or(Value::Null);
        let links_json = serde_json::to_value(links).unwrap_or(Value::Null);
        for phase in phases {
            for a in self.analyzers.iter().filter(|a| a.phase == *phase) {
                let req = json!({
                    "op": "evaluate",
                    "key": a.key,
                    "nodes": nodes_json,
                    "links": links_json,
                    "settings": a.resolved_settings,
                    "cwd": cwd,
                });
                let Some(resp) = bridge.call(&req) else { continue };
                if let Some(e) = resp.get("error").and_then(Value::as_str) {
                    eprintln!("⚠  Analyzer \"{}\" threw during evaluate: {e}", a.qualified());
                }
                for adj in resp.get("adjustments").and_then(Value::as_array).into_iter().flatten() {
                    let (Some(i), Some(kind), Some(value)) = (
                        adj.get("index").and_then(Value::as_u64),
                        adj.pointer("/op/kind").and_then(Value::as_str),
                        adj.pointer("/op/value").and_then(Value::as_f64),
                    ) else {
                        continue;
                    };
                    if let Some(slot) = out.ops.get_mut(i as usize) {
                        slot.push((a.plugin_id.clone(), a.id.clone(), kind.to_string(), value));
                    }
                }
                for c in resp.get("contributions").and_then(Value::as_array).into_iter().flatten() {
                    match check_contribution_internal(&a.plugin_id, &a.id, &a.contributions, c) {
                        Ok(contrib) => out.contributions.push(contrib),
                        Err(err) => out.contribution_errors.push(err),
                    }
                }
                for issue in resp.get("issues").and_then(Value::as_array).into_iter().flatten() {
                    if let Some(i) = to_issue(a, issue) {
                        out.issues.push((a.qualified(), i));
                    }
                }
            }
        }
        out
    }
}

/// Validate one `emitContribution` call: the ref must be a declared `ui`
/// object (by identity, resolved in the bridge) and the payload must pass
/// its slot's schema. Rejections are reported like skill-map's
/// `extension.error`.
fn check_contribution_internal(
    plugin_id: &str,
    ext_id: &str,
    contributions_map: &HashMap<String, String>,
    c: &Value,
) -> Result<Contribution, ContributionError> {
    let qualified = format!("{plugin_id}/{ext_id}");
    let node_path = c.get("nodePath").and_then(Value::as_str).unwrap_or("");
    let now = crate::scan::now_ms();
    let Some((id, slot)) = c
        .get("id")
        .and_then(Value::as_str)
        .and_then(|id| contributions_map.get_key_value(id))
    else {
        let msg = format!(
            "Extension \"{qualified}\" emitted a view contribution on {node_path} whose object is not one declared in its `ui` map (pass the declared const by reference, do not spread or inline it). Contribution dropped."
        );
        eprintln!("⚠  {msg}");
        return Err(ContributionError {
            plugin_id: plugin_id.to_string(),
            extension_id: ext_id.to_string(),
            node_path: node_path.to_string(),
            reason: "undeclared-contribution-ref".to_string(),
            message: msg,
            contribution_id: None,
            slot: None,
            emitted_at: now,
        });
    };
    let payload = c.get("payload").cloned().unwrap_or(Value::Null);
    let errors = match schema::slot_payload_ref(slot) {
        Some(r) => schema::format_errors(&schema::validate(&r, &payload)),
        None => "unknown-slot".to_string(),
    };
    if !errors.is_empty() {
        let msg = format!(
            "Extractor \"{qualified}\" emitted contribution \"{id}\" on {node_path}; payload failed the \"{slot}\" schema: {errors}. Contribution dropped."
        );
        eprintln!("⚠  {msg}");
        return Err(ContributionError {
            plugin_id: plugin_id.to_string(),
            extension_id: ext_id.to_string(),
            node_path: node_path.to_string(),
            reason: errors,
            message: msg,
            contribution_id: Some(id.clone()),
            slot: Some(slot.clone()),
            emitted_at: now,
        });
    }
    Ok(Contribution {
        plugin_id: plugin_id.to_string(),
        extension_id: ext_id.to_string(),
        node_path: node_path.to_string(),
        contribution_id: id.clone(),
        slot: slot.clone(),
        payload,
        emitted_at: now,
    })
}

/// `validateIssue`: drop an issue with an invalid severity, default its
/// `analyzerId` to the extension id.
fn to_issue(a: &Analyzer, raw: &Value) -> Option<Issue> {
    let obj = raw.as_object()?;
    let severity: &'static str = match obj.get("severity").and_then(Value::as_str) {
        Some("error") => "error",
        Some("warn") => "warn",
        Some("info") => "info",
        _ => {
            let sev = obj.get("severity").map(crate::jsjson::to_string).unwrap_or_else(|| "undefined".into());
            eprintln!(
                "⚠  Rule \"{}\" emitted an issue with invalid severity {sev} (allowed: 'error' | 'warn' | 'info'). Issue dropped.",
                a.qualified()
            );
            return None;
        }
    };
    let analyzer_id = obj.get("analyzerId").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(&a.id);
    let node_ids = obj
        .get("nodeIds")
        .and_then(Value::as_array)
        .map(|v| v.iter().filter_map(Value::as_str).map(String::from).collect())
        .unwrap_or_default();
    let message = obj.get("message").and_then(Value::as_str).unwrap_or("").to_string();
    let mut issue = Issue::new(analyzer_id, severity, node_ids, message);
    for (k, v) in obj {
        if !matches!(k.as_str(), "analyzerId" | "severity" | "nodeIds" | "message") {
            issue.tail.insert(k.clone(), v.clone());
        }
    }
    Some(issue)
}

/// `foldConfidence`: set, then delta, then floor, then ceil, clamped to
/// [0, 1]. `ops` must already be in (pluginId, extensionId) order.
pub fn fold_confidence(base: f64, ops: &[(String, String, String, f64)]) -> f64 {
    let mut running = base;
    for (_, _, k, v) in ops {
        if k == "set" {
            running = *v;
        }
    }
    for (_, _, k, v) in ops {
        if k == "delta" {
            running += *v;
        }
    }
    for (_, _, k, v) in ops {
        if k == "floor" {
            running = running.max(*v);
        }
    }
    for (_, _, k, v) in ops {
        if k == "ceil" {
            running = running.min(*v);
        }
    }
    running.clamp(0.0, 1.0)
}
