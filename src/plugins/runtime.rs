//! Project-local plugin runtime: load the trusted drop-in plugins under
//! `.skill-map/plugins/`, validate each extension the way skill-map's
//! loader does, and run their analyzers.
//!
//! Extension modules are JavaScript, so they are imported and called
//! through `bridge.mjs` in a `node` child process. Everything else stays
//! here: the trust and enable gates (no code is imported before both
//! pass), manifest validation against the spec schemas, `ui` payload
//! validation, the confidence fold and the error messages.

use crate::model::{Issue, Link, Node};
use crate::plugins::schema;
use crate::scope::Scope;
use serde_json::{Value, json};
use std::collections::HashMap;
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

pub struct Analyzer {
    pub plugin_id: String,
    pub id: String,
    pub phase: String,
    key: String,
    /// `ui` contribution id → slot.
    contributions: HashMap<String, String>,
}

impl Analyzer {
    fn qualified(&self) -> String {
        format!("{}/{}", self.plugin_id, self.id)
    }
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
    pub analyzers: Vec<Analyzer>,
}

/// Results of one phase of plugin analyzers.
#[derive(Default)]
pub struct PhaseOutput {
    /// (qualified analyzer id, issue).
    pub issues: Vec<(String, Issue)>,
    /// Per link index: (pluginId, extensionId, op kind, value).
    pub ops: Vec<Vec<(String, String, String, f64)>>,
}

fn plugin_failure(plugin_id: &str, status: &str, reason: &str) {
    eprintln!("plugin {plugin_id} ({status}), all extensions skipped: {reason}");
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
    let mut rt = Runtime { bridge: None, analyzers: Vec::new() };
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
        let mut loaded = Vec::new();
        let mut failed = false;
        for (entry, res) in entries.iter().zip(results.iter()) {
            match validate_entry(entry, res) {
                Ok(Some(a)) => loaded.push(a),
                Ok(None) => {}
                Err((status, reason)) => {
                    plugin_failure(&plugin_id, status, &reason);
                    failed = true;
                    break;
                }
            }
        }
        if !failed {
            rt.analyzers.extend(loaded);
        }
    }
    rt.bridge = Some(bridge);
    rt
}

/// The loader's per-entry checks, in order. `Ok(None)`: valid but not an
/// analyzer this kernel runs.
fn validate_entry(entry: &Entry, res: &Value) -> Result<Option<Analyzer>, (&'static str, String)> {
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
    let manifest = res.get("manifest").cloned().unwrap_or_else(|| json!({}));
    // Provider manifests carry a much larger contract this kernel does not
    // run; they are imported (proving they load) but not shape-checked.
    if entry.kind != "provider" {
        let errors = schema::validate(&schema::extension_ref(entry.kind), &manifest);
        if !errors.is_empty() {
            let slot_error = errors.iter().any(|e| e.instance_path.ends_with("/slot"));
            let doc = if slot_error { format!("{SPEC_BASE}/view-slots.md") } else { format!("{SPEC_BASE}/schemas/extensions/{}.schema.json", entry.kind) };
            return Err(("invalid-manifest", format!("{rel}: {}. See {doc}.", schema::format_errors(&errors))));
        }
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
    Ok(Some(Analyzer {
        plugin_id: entry.plugin_id.clone(),
        id: entry.id.clone(),
        phase: manifest.get("phase").and_then(Value::as_str).unwrap_or("detect").to_string(),
        key: entry.key.clone(),
        contributions,
    }))
}

impl Runtime {
    pub fn is_empty(&self) -> bool {
        self.analyzers.is_empty()
    }

    /// Run the plugin analyzers of the given phases (in phase, then
    /// discovery, order) over the graph.
    pub fn run_phase(&mut self, phases: &[&str], nodes: &[Node], links: &[Link], cwd: &Path) -> PhaseOutput {
        let mut out = PhaseOutput { issues: Vec::new(), ops: vec![Vec::new(); links.len()] };
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
                    "settings": {},
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
                    check_contribution(a, c);
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
fn check_contribution(a: &Analyzer, c: &Value) {
    let node_path = c.get("nodePath").and_then(Value::as_str).unwrap_or("");
    let qualified = a.qualified();
    let Some((id, slot)) = c.get("id").and_then(Value::as_str).and_then(|id| a.contributions.get_key_value(id)) else {
        eprintln!(
            "⚠  Extension \"{qualified}\" emitted a view contribution on {node_path} whose object is not one declared in its `ui` map (pass the declared const by reference, do not spread or inline it). Contribution dropped."
        );
        return;
    };
    let payload = c.get("payload").cloned().unwrap_or(Value::Null);
    let errors = match schema::slot_payload_ref(slot) {
        Some(r) => schema::format_errors(&schema::validate(&r, &payload)),
        None => "unknown-slot".to_string(),
    };
    if !errors.is_empty() {
        eprintln!(
            "⚠  Extractor \"{qualified}\" emitted contribution \"{id}\" on {node_path}; payload failed the \"{slot}\" schema: {errors}. Contribution dropped."
        );
    }
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
