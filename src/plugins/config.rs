//! Configuration commands: `sm config list`, `get`, `show`, `set`, `reset`.
//! Implements the layered configuration hierarchy (defaults -> project -> project-local)
//! and validates against `project-config.schema.json`.

use crate::cli::{Cli, CliError};
use crate::scope::Scope;
use clap::{Args, Subcommand};
use serde_json::{json, Map, Value};
use std::collections::HashMap;

pub const PROJECT_LOCAL_ONLY_KEYS: &[&str] = &[
    "allowEditSmFiles",
    "allowNetworkActions",
    "logLevel",
    "tutorialReminderStep",
    "scan.referencePaths",
    "scan.followExternalSymlinks",
    "activity.captureConversations",
    "activity.captureLevel",
    "activity.shellCapture",
    "ui.liveUpdates",
    "ui.realtimeActivity",
    "ui.showRuntimeAgents",
    "ui.changeSpark",
    "ui.confirmIgnore",
    "ui.confirmViewSwitch",
    "mcp.server.enabled",
    "skillActions.enabled",
    "plugins.github.extensions.enrichment.settings.apiBaseUrl",
    "plugins.github.extensions.enrichment.settings.rawBaseUrl",
];

pub fn is_project_local_only_key(key: &str) -> bool {
    PROJECT_LOCAL_ONLY_KEYS.contains(&key)
        || key.ends_with(".apiBaseUrl")
        || key.ends_with(".rawBaseUrl")
}

pub fn defaults_json() -> Value {
    json!({
        "schemaVersion": 1,
        "allowEditSmFiles": false,
        "allowNetworkActions": false,
        "allowSidecarWriters": true,
        "tutorialReminderStep": 0,
        "tokenizer": "cl100k_base",
        "server": {
            "port": 4242,
            "host": "127.0.0.1"
        },
        "roots": [],
        "ignore": [],
        "scan": {
            "tokenize": true,
            "strict": false,
            "maxFileSizeBytes": 1048576,
            "maxScan": 5000,
            "maxNodes": 256,
            "watch": {
                "debounceMs": 300,
                "backend": "chokidar"
            },
            "referencePaths": [],
            "followExternalSymlinks": false,
            "respectGitignore": false
        },
        "plugins": {},
        "skillActions": {
            "enabled": false
        },
        "jobs": {
            "claimWaitSeconds": 2,
            "perExtensionTtl": {},
            "perExtensionPriority": {},
            "retention": {
                "completed": 2592000,
                "failed": null,
                "cancelled": 2592000
            }
        }
    })
}

#[derive(Args, Debug, Clone)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ConfigCommand {
    /// Print the effective config after layered merge.
    List {
        #[arg(long)]
        strict: bool,
    },
    /// Read a single config value by dot-path key.
    Get {
        key: String,
        #[arg(long)]
        strict: bool,
    },
    /// Show a config value with the layer that set it (--source).
    Show {
        key: String,
        #[arg(long)]
        source: bool,
        #[arg(long)]
        strict: bool,
    },
    /// Write a config key.
    Set {
        key: String,
        value: String,
        #[arg(long)]
        yes: bool,
    },
    /// Remove a config key from the target settings file.
    Reset {
        key: String,
    },
}

pub fn get_dot_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = root;
    for seg in path.split('.') {
        if seg.is_empty() {
            continue;
        }
        match cur {
            Value::Object(m) => cur = m.get(seg)?,
            _ => return None,
        }
    }
    Some(cur)
}

pub fn set_dot_path(root: &mut Value, path: &str, value: Value) {
    let segments: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return;
    }
    let mut cur = root;
    for &seg in &segments[..segments.len() - 1] {
        if !cur.is_object() {
            *cur = Value::Object(Map::new());
        }
        let obj = cur.as_object_mut().unwrap();
        if !obj.contains_key(seg) || !obj[seg].is_object() {
            obj.insert(seg.to_string(), Value::Object(Map::new()));
        }
        cur = obj.get_mut(seg).unwrap();
    }
    let last = segments[segments.len() - 1];
    if !cur.is_object() {
        *cur = Value::Object(Map::new());
    }
    cur.as_object_mut().unwrap().insert(last.to_string(), value);
}

pub fn delete_dot_path(root: &mut Value, path: &str) -> bool {
    let segments: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return false;
    }
    let mut cur = root;
    for &seg in &segments[..segments.len() - 1] {
        match cur {
            Value::Object(m) => {
                let Some(next) = m.get_mut(seg) else {
                    return false;
                };
                cur = next;
            }
            _ => return false,
        }
    }
    let last = segments[segments.len() - 1];
    if let Value::Object(m) = cur {
        m.remove(last).is_some()
    } else {
        false
    }
}

pub fn deep_merge(target: &mut Value, source: &Value) {
    match (target, source) {
        (Value::Object(t), Value::Object(s)) => {
            for (k, v) in s {
                if let Some(target_v) = t.get_mut(k) {
                    deep_merge(target_v, v);
                } else {
                    t.insert(k.clone(), v.clone());
                }
            }
        }
        (t, s) => {
            *t = s.clone();
        }
    }
}

pub fn record_sources(prefix: &str, val: &Value, sources: &mut HashMap<String, String>, layer: &str) {
    match val {
        Value::Object(m) => {
            if m.is_empty() && !prefix.is_empty() {
                sources.insert(prefix.to_string(), layer.to_string());
                return;
            }
            for (k, v) in m {
                let next = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                record_sources(&next, v, sources, layer);
            }
        }
        _ => {
            if !prefix.is_empty() {
                sources.insert(prefix.to_string(), layer.to_string());
            }
        }
    }
}

fn strip_local_only(val: &mut Value) {
    for &key in PROJECT_LOCAL_ONLY_KEYS {
        delete_dot_path(val, key);
    }
}

pub fn load_config(scope: &Scope) -> (Value, HashMap<String, String>) {
    let mut effective = defaults_json();
    let mut sources = HashMap::new();
    record_sources("", &effective, &mut sources, "defaults");

    // Project layer: .skill-map/settings.json
    let project_path = scope.dir().join("settings.json");
    if let Ok(text) = std::fs::read_to_string(&project_path) {
        if let Ok(mut parsed) = serde_json::from_str::<Value>(&text) {
            strip_local_only(&mut parsed);
            deep_merge(&mut effective, &parsed);
            record_sources("", &parsed, &mut sources, "project");
        }
    }

    // Project-local layer: .skill-map/settings.local.json
    let local_path = scope.dir().join("settings.local.json");
    if let Ok(text) = std::fs::read_to_string(&local_path) {
        if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
            deep_merge(&mut effective, &parsed);
            record_sources("", &parsed, &mut sources, "project-local");
        }
    }

    (effective, sources)
}

pub fn parse_cli_value(raw: &str) -> Value {
    if raw == "true" {
        json!(true)
    } else if raw == "false" {
        json!(false)
    } else if raw == "null" {
        json!(null)
    } else if let Ok(i) = raw.parse::<i64>() {
        json!(i)
    } else if let Ok(f) = raw.parse::<f64>() {
        json!(f)
    } else if let Ok(v) = serde_json::from_str::<Value>(raw) {
        if v.is_array() || v.is_object() {
            v
        } else {
            json!(raw)
        }
    } else {
        json!(raw)
    }
}

pub fn run(cli: &Cli, scope: &Scope, args: &ConfigArgs) -> Result<u8, CliError> {
    match &args.command {
        ConfigCommand::List { strict: _ } => {
            let (effective, _) = load_config(scope);
            if cli.global.json {
                println!("{}", serde_json::to_string_pretty(&effective).unwrap());
                return Ok(0);
            }
            print_human_sections(&effective);
            Ok(0)
        }
        ConfigCommand::Get { key, strict: _ } => {
            let (effective, _) = load_config(scope);
            let Some(val) = get_dot_path(&effective, key) else {
                return Err(CliError::Raw(format!("✕  Unknown config key: {key}"), 5));
            };
            if cli.global.json {
                println!("{}", serde_json::to_string(val).unwrap());
            } else {
                println!("{}", format_human_value(val));
            }
            Ok(0)
        }
        ConfigCommand::Show { key, source, strict: _ } => {
            let (effective, sources) = load_config(scope);
            let Some(val) = get_dot_path(&effective, key) else {
                return Err(CliError::Raw(format!("✕  Unknown config key: {key}"), 5));
            };
            let layer = resolve_source(key, val, &sources);
            if cli.global.json {
                if *source {
                    println!("{}", json!({ "value": val, "source": layer }));
                } else {
                    println!("{}", serde_json::to_string(val).unwrap());
                }
            } else if *source {
                println!("{}  (from {})", format_human_value(val), layer);
            } else {
                println!("{}", format_human_value(val));
            }
            Ok(0)
        }
        ConfigCommand::Set { key, value, yes } => {
            let parsed_val = parse_cli_value(value);
            if key == "scan.followExternalSymlinks" && parsed_val == json!(true) && !yes {
                return Err(CliError::Raw(
                    "✕  Setting scan.followExternalSymlinks expands disk access outside the project. Pass --yes to confirm.".into(),
                    2,
                ));
            }
            scope.provision().map_err(|e| CliError::Operational(e.to_string()))?;
            let is_local = is_project_local_only_key(key);
            let filename = if is_local { "settings.local.json" } else { "settings.json" };
            let target_path = scope.dir().join(filename);
            let rel_target = format!(".skill-map/{filename}");

            let mut root = std::fs::read_to_string(&target_path)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .unwrap_or_else(|| json!({}));

            set_dot_path(&mut root, key, parsed_val.clone());
            crate::scope::write_json(&target_path, &root).map_err(|e| CliError::Operational(e.to_string()))?;

            if is_local {
                grant_local_scope_key(scope, key, &parsed_val)?;
            }

            scope.log_operation("config.set", "cli", "ok", &format!("key={key}"));
            println!("✓  {} = {}  (wrote {})", key, format_human_value(&parsed_val), rel_target);
            Ok(0)
        }
        ConfigCommand::Reset { key } => {
            let is_local = is_project_local_only_key(key);
            let filename = if is_local { "settings.local.json" } else { "settings.json" };
            let target_path = scope.dir().join(filename);
            let rel_target = format!(".skill-map/{filename}");

            if !target_path.exists() {
                println!("✓  No override at {rel_target} for {key}");
                return Ok(0);
            }

            let mut root = std::fs::read_to_string(&target_path)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .unwrap_or_else(|| json!({}));

            let removed = delete_dot_path(&mut root, key);
            if removed {
                crate::scope::write_json(&target_path, &root).map_err(|e| CliError::Operational(e.to_string()))?;
                if is_local {
                    revoke_local_scope_key(scope, key).ok();
                }
                scope.log_operation("config.reset", "cli", "ok", &format!("key={key}"));
                println!("✓  Removed {key} from {rel_target}");
            } else {
                println!("✓  No override at {rel_target} for {key}");
            }
            Ok(0)
        }
    }
}

fn grant_local_scope_key(scope: &Scope, key: &str, value: &Value) -> Result<(), CliError> {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(scope.dir()).map_err(|e| CliError::Operational(e.to_string()))?;
    let born = meta
        .created()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let parts = [
        "skill-map/scope-grant/v1".to_string(),
        "local-config".to_string(),
        meta.ino().to_string(),
        born.to_string(),
        key.to_string(),
        crate::jsjson::to_string(value),
    ];
    let preimage: String = parts.iter().map(|p| format!("{}:{p}", p.encode_utf16().count())).collect();
    let grant = hex::encode(Sha256::digest(preimage.as_bytes()));

    let lock_path = scope.dir().join("scope.lock.json");
    let mut lock = std::fs::read_to_string(&lock_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or_else(|| json!({ "v": 1, "namespaces": {} }));

    if !lock.is_object() {
        lock = json!({ "v": 1, "namespaces": {} });
    }
    let namespaces = lock.as_object_mut().unwrap().entry("namespaces").or_insert_with(|| json!({}));
    if !namespaces.is_object() {
        *namespaces = json!({});
    }
    let local_cfg = namespaces.as_object_mut().unwrap().entry("local-config").or_insert_with(|| json!({}));
    if !local_cfg.is_object() {
        *local_cfg = json!({});
    }
    local_cfg.as_object_mut().unwrap().insert(
        key.to_string(),
        json!({
            "grant": grant,
            "grantedAt": crate::scan::now_ms()
        }),
    );

    crate::scope::write_json(&lock_path, &lock).map_err(|e| CliError::Operational(e.to_string()))?;
    Ok(())
}

fn revoke_local_scope_key(scope: &Scope, key: &str) -> Result<(), CliError> {
    let lock_path = scope.dir().join("scope.lock.json");
    if !lock_path.exists() {
        return Ok(());
    }
    let mut lock = std::fs::read_to_string(&lock_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or_else(|| json!({ "v": 1, "namespaces": {} }));

    if let Some(namespaces) = lock.get_mut("namespaces").and_then(Value::as_object_mut) {
        if let Some(local_cfg) = namespaces.get_mut("local-config").and_then(Value::as_object_mut) {
            local_cfg.remove(key);
        }
    }
    crate::scope::write_json(&lock_path, &lock).map_err(|e| CliError::Operational(e.to_string()))?;
    Ok(())
}

fn resolve_source(key: &str, value: &Value, sources: &HashMap<String, String>) -> String {
    if let Some(s) = sources.get(key) {
        return s.clone();
    }
    if value.is_object() {
        let prefix = format!("{key}.");
        let mut best = "defaults";
        let rank = |r: &str| match r {
            "override" => 3,
            "project-local" => 2,
            "project" => 1,
            _ => 0,
        };
        let mut best_rank = 0;
        for (k, layer) in sources {
            if k.starts_with(&prefix) {
                let r = rank(layer);
                if r > best_rank {
                    best_rank = r;
                    best = layer;
                }
            }
        }
        return best.to_string();
    }
    "defaults".to_string()
}

fn format_human_value(val: &Value) -> String {
    match val {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        _ => serde_json::to_string(val).unwrap_or_default(),
    }
}

fn is_empty_config_value(val: &Value) -> bool {
    match val {
        Value::Null => true,
        Value::Array(a) => a.is_empty(),
        Value::Object(m) => m.is_empty(),
        _ => false,
    }
}

fn collect_leaf_paths(prefix: &str, val: &Value, out: &mut Vec<(String, Value)>) {
    match val {
        Value::Object(m) => {
            if m.is_empty() {
                out.push((prefix.to_string(), val.clone()));
                return;
            }
            for (k, v) in m {
                let next = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                collect_leaf_paths(&next, v, out);
            }
        }
        _ => {
            out.push((prefix.to_string(), val.clone()));
        }
    }
}

fn print_human_sections(effective: &Value) {
    let mut leaves = Vec::new();
    collect_leaf_paths("", effective, &mut leaves);

    struct SecDef {
        title: &'static str,
        exact: Option<&'static [&'static str]>,
        prefix: Option<&'static str>,
    }

    let defs = [
        SecDef { title: "General", exact: Some(&["schemaVersion", "tokenizer"]), prefix: None },
        SecDef { title: "Scan", exact: None, prefix: Some("scan.") },
        SecDef { title: "Jobs", exact: None, prefix: Some("jobs.") },
        SecDef { title: "Roots & plugins", exact: Some(&["roots", "plugins", "ignore"]), prefix: None },
    ];

    let mut unmatched = leaves;
    let mut sections: Vec<(String, Vec<(String, Value)>)> = Vec::new();

    for def in &defs {
        let mut matched = Vec::new();
        let mut remaining = Vec::new();
        for (k, v) in unmatched {
            let is_match = if let Some(exact) = def.exact {
                exact.contains(&k.as_str())
            } else if let Some(pfx) = def.prefix {
                k.starts_with(pfx)
            } else {
                false
            };
            if is_match {
                let display_k = if let Some(pfx) = def.prefix {
                    k.strip_prefix(pfx).unwrap_or(&k).to_string()
                } else {
                    k
                };
                matched.push((display_k, v));
            } else {
                remaining.push((k, v));
            }
        }
        unmatched = remaining;
        if !matched.is_empty() {
            matched.sort_by(|a, b| a.0.cmp(&b.0));
            sections.push((def.title.to_string(), matched));
        }
    }

    if !unmatched.is_empty() {
        unmatched.sort_by(|a, b| a.0.cmp(&b.0));
        sections.push(("Other".to_string(), unmatched));
    }

    for (i, (title, rows)) in sections.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!("  {title}");
        let width = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
        for (k, v) in rows {
            let display_val = if is_empty_config_value(v) {
                "-".to_string()
            } else {
                format_human_value(v)
            };
            println!("    {:width$}  {}", k, display_val, width = width);
        }
    }
}
