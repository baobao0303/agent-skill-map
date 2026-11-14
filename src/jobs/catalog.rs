//! Action / analyzer catalog: the original's built-in extensions (data
//! extracted to `schema/catalog.json` by `tools/extract-catalog.mjs`) plus
//! drop-in project plugins under `.skill-map/plugins/<plugin>/`.
//!
//! Drop-in plugins are read by convention, never executed: an extension
//! folder carrying `prompt.md` + `report.schema.json` is a probabilistic
//! extension; anything else is listed as deterministic.

use crate::bump::extension_enabled;
use crate::scope::Scope;
use serde_json::Value;
use std::sync::LazyLock;

pub static BUILT_INS: LazyLock<Vec<Value>> =
    LazyLock::new(|| serde_json::from_str(include_str!("../schema/catalog.json")).expect("bundled catalog"));

#[derive(Clone)]
pub struct Ext {
    pub plugin: String,
    pub id: String,
    /// `action` | `analyzer`
    pub kind: String,
    pub version: String,
    pub mode: String,
    pub precondition: Option<Value>,
    pub prompt: Option<String>,
    /// The report schema text exactly as it is inlined into the job.
    pub report_text: Option<String>,
    pub report_schema: Option<Value>,
    pub nodeless: bool,
    pub default_priority: Option<i64>,
}

impl Ext {
    pub fn qualified(&self) -> String {
        format!("{}/{}", self.plugin, self.id)
    }
    pub fn probabilistic(&self) -> bool {
        self.mode == "probabilistic"
    }
}

fn built_in(e: &Value) -> Ext {
    let s = |k: &str| e.get(k).and_then(Value::as_str).map(str::to_string);
    let report_schema = e.get("reportSchema").filter(|v| v.is_object()).cloned();
    Ext {
        plugin: s("pluginId").unwrap_or_default(),
        id: s("id").unwrap_or_default(),
        kind: s("kind").unwrap_or_default(),
        version: s("version").unwrap_or_default(),
        mode: s("mode").unwrap_or_else(|| "deterministic".into()),
        precondition: e.get("precondition").cloned(),
        prompt: s("promptTemplate"),
        report_text: report_schema.as_ref().map(crate::jsjson::to_string_pretty),
        report_schema,
        nodeless: e.get("probNodeless").and_then(Value::as_bool) == Some(true),
        default_priority: e.get("defaultPriority").and_then(Value::as_i64),
    }
}

fn enabled_by_default(e: &Value) -> bool {
    if let Some(d) = e.get("defaultEnabled").and_then(Value::as_bool) {
        return d;
    }
    !matches!(e.get("stability").and_then(Value::as_str), Some("experimental" | "deprecated"))
}

fn drop_ins(scope: &Scope) -> Vec<Ext> {
    let mut out = Vec::new();
    let Ok(plugins) = std::fs::read_dir(scope.dir().join("plugins")) else { return out };
    let mut plugins: Vec<_> = plugins.filter_map(Result::ok).filter(|e| e.path().join("plugin.json").is_file()).collect();
    plugins.sort_by_key(|e| e.file_name());
    for p in plugins {
        let plugin = p.file_name().to_string_lossy().to_string();
        for (folder, kind) in [("actions", "action"), ("analyzers", "analyzer")] {
            let Ok(exts) = std::fs::read_dir(p.path().join(folder)) else { continue };
            let mut exts: Vec<_> = exts.filter_map(Result::ok).filter(|e| e.path().is_dir()).collect();
            exts.sort_by_key(|e| e.file_name());
            for e in exts {
                let dir = e.path();
                let manifest: Value = std::fs::read_to_string(dir.join("extension.json"))
                    .ok()
                    .and_then(|t| serde_json::from_str(&t).ok())
                    .unwrap_or(Value::Null);
                let prompt = std::fs::read_to_string(dir.join("prompt.md")).ok();
                let report_text = std::fs::read_to_string(dir.join("report.schema.json")).ok();
                let report_schema = report_text.as_deref().and_then(|t| serde_json::from_str(t).ok());
                let probabilistic = prompt.is_some() && report_text.is_some();
                out.push(Ext {
                    plugin: plugin.clone(),
                    id: e.file_name().to_string_lossy().to_string(),
                    kind: kind.into(),
                    version: manifest.get("version").and_then(Value::as_str).unwrap_or("0.0.0").into(),
                    mode: if probabilistic { "probabilistic" } else { "deterministic" }.into(),
                    precondition: manifest.get("precondition").cloned(),
                    prompt,
                    report_text,
                    report_schema,
                    nodeless: manifest.get("probNodeless").and_then(Value::as_bool) == Some(true),
                    default_priority: manifest.get("defaultPriority").and_then(Value::as_i64),
                });
            }
        }
    }
    out
}

/// Enabled actions and analyzers, built-ins first.
pub fn load(scope: &Scope) -> Vec<Ext> {
    let mut out: Vec<Ext> = BUILT_INS
        .iter()
        .filter(|e| matches!(e.get("kind").and_then(Value::as_str), Some("action" | "analyzer")))
        .filter(|e| {
            let plugin = e.get("pluginId").and_then(Value::as_str).unwrap_or("");
            let id = e.get("id").and_then(Value::as_str).unwrap_or("");
            extension_enabled(scope, plugin, id, enabled_by_default(e))
        })
        .map(built_in)
        .collect();
    out.extend(drop_ins(scope));
    out
}

fn find<'a>(exts: &[&'a Ext], id: &str) -> Option<&'a Ext> {
    exts.iter().find(|e| e.qualified() == id).or_else(|| exts.iter().find(|e| e.id == id)).copied()
}

pub enum Target<'a> {
    Found(&'a Ext),
    NotFound,
    Deterministic(String),
    Ambiguous(String, String),
}

/// `resolveSubmitTarget`: `action:` / `analyzer:` prefixes, else the
/// probabilistic action and analyzer namespaces (both → ambiguous).
pub fn resolve<'a>(catalog: &'a [Ext], target: &str) -> Target<'a> {
    let of = |kind: &str| catalog.iter().filter(|e| e.kind == kind).collect::<Vec<_>>();
    let (actions, analyzers) = (of("action"), of("analyzer"));
    let prefixed = |list: &[&'a Ext], id: &str| match find(list, id) {
        None => Target::NotFound,
        Some(e) if !e.probabilistic() => Target::Deterministic(e.mode.clone()),
        Some(e) => Target::Found(e),
    };
    if let Some(rest) = target.strip_prefix("action:") {
        return prefixed(&actions, rest);
    }
    if let Some(rest) = target.strip_prefix("analyzer:") {
        return prefixed(&analyzers, rest);
    }
    let prob = |l: &[&'a Ext]| l.iter().copied().filter(|e| e.probabilistic()).collect::<Vec<_>>();
    match (find(&prob(&actions), target), find(&prob(&analyzers), target)) {
        (Some(a), Some(b)) => Target::Ambiguous(a.qualified(), b.qualified()),
        (Some(a), None) | (None, Some(a)) => Target::Found(a),
        (None, None) => match find(&actions, target).or_else(|| find(&analyzers, target)) {
            Some(e) => Target::Deterministic(e.mode.clone()),
            None => Target::NotFound,
        },
    }
}
