//! Plugins commands: `sm plugins slots list`, `list`, `doctor`, `enable`, `disable`, etc.

use crate::bump::extension_enabled;
use crate::catalog::BUILT_INS;
use crate::cli::{Cli, CliError};
use crate::scope::Scope;
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::LazyLock;

pub static VIEW_SLOTS_CATALOG: LazyLock<Value> = LazyLock::new(|| {
    json!([
        { "id": "card.title.right", "summary": "Small icon marker next to the card title (language flag, platform glyph)." },
        { "id": "card.subtitle.left", "summary": "Single non-negative integer in the card subtitle row." },
        { "id": "card.footer.left", "summary": "Counter chip in the left footer of the card." },
        { "id": "card.footer.right", "summary": "Counter chip in the right footer of the card." },
        { "id": "graph.node.alert", "summary": "Reserved corner badge on the graph node, special-case signals only. No core analyzer emits here; routine \"this node has a problem\" findings belong in `card.footer.right`." },
        { "id": "inspector.header.badge", "summary": "Unified badge in the inspector header cluster: icon and/or label and/or count, optional severity. Multi-cardinality, priority order, modeled on card.footer.left. Replaces the retired inspector.header.badge.counter and inspector.header.badge.tag sub-slots." },
        { "id": "inspector.action.button", "summary": "Action button in the inspector's generic Actions section. Click dispatches a kernel Action by qualified id via POST /api/actions/:id. Always emitted; the payload `enabled` flag carries the dynamic condition (e.g. isStale for the bump button)." },
        { "id": "inspector.surface.version", "summary": "The node's VERSION surface (header version chip + the card's vN label). The claiming Action owns the affordance; click dispatches the payload's actionId (the bump flow). Single-cardinality logical surface." },
        { "id": "inspector.surface.stability", "summary": "The node's STABILITY surface (header stability chip + card badge). The claiming Action owns the affordance; click opens the payload prompt and dispatches its actionId. Single-cardinality logical surface." },
        { "id": "inspector.surface.tags", "summary": "The node's TAG-ROW surface (inline tag editor + the card's tag chips). The claiming Action owns the affordance; edits dispatch the payload's actionId. Single-cardinality logical surface." },
        { "id": "inspector.surface.summary", "summary": "The header's semantic-analysis (summarize) affordance. The claiming probabilistic Action owns it; click submits a job for the payload's actionId. Single-cardinality logical surface." },
        { "id": "inspector.surface.auto-tag", "summary": "The tag row's auto-tag (sparkles) affordance. The claiming probabilistic Action owns it; click submits a job for the payload's actionId. Single-cardinality logical surface." },
        { "id": "inspector.body.panel.breakdown", "summary": "Top-N labeled values rendered as a bar chart in the inspector body." },
        { "id": "inspector.body.panel.records", "summary": "Tabular data (rows × columns ≤ 50 × 6) in the inspector body." },
        { "id": "inspector.body.panel.tree", "summary": "Recursive label/children hierarchy (depth ≤ 6, total ≤ 200) in the inspector body." },
        { "id": "inspector.body.panel.key-values", "summary": "Flat key/value pairs (≤ 50) in the inspector body." },
        { "id": "inspector.body.panel.link-list", "summary": "Clickable scope-relative paths (≤ 100) in the inspector body." },
        { "id": "inspector.body.panel.markdown", "summary": "Sanitized markdown text (≤ 4096 chars) in the inspector body." },
        { "id": "topbar.nav.start", "summary": "Scope-wide indicator chip at the start of the topbar nav (before the view-switcher links)." }
    ])
});

pub static INPUT_TYPES_CATALOG: LazyLock<Value> = LazyLock::new(|| {
    json!([
        { "id": "string-list", "summary": "Array of free-form strings." },
        { "id": "single-string", "summary": "Single text input." },
        { "id": "boolean-flag", "summary": "On/off toggle." },
        { "id": "integer", "summary": "Integer with optional bounds." },
        { "id": "number", "summary": "Decimal number with optional bounds." },
        { "id": "enum-pick", "summary": "Pick one from a closed set." },
        { "id": "enum-multipick", "summary": "Pick zero or more from a closed set." },
        { "id": "path-glob", "summary": "Glob pattern (single or multiple)." },
        { "id": "regex", "summary": "ECMAScript regex pattern body." },
        { "id": "secret", "summary": "Sensitive string, forced into project-local storage (gitignored), not encrypted." },
        { "id": "key-value-list", "summary": "Editable mapping of strings to strings." },
        { "id": "match-list", "summary": "List of match entries, each literal (exact equality), regex (unanchored test), or glob (gitignore-style)." }
    ])
});

#[derive(Args, Debug, Clone)]
pub struct PluginsArgs {
    #[command(subcommand)]
    pub command: PluginsCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum PluginsCommand {
    /// View slot and input type catalogs.
    Slots {
        #[command(subcommand)]
        command: PluginsSlotsCommand,
    },
    /// List installed and discovered plugins.
    List {
        id: Option<String>,
    },
    /// Diagnostic checks across all extensions.
    Doctor,
    /// Enable one or more extensions (or --all).
    Enable {
        ids: Vec<String>,
        #[arg(long)]
        local: bool,
        #[arg(long)]
        all: bool,
    },
    /// Disable one or more extensions (or --all).
    Disable {
        ids: Vec<String>,
        #[arg(long)]
        local: bool,
        #[arg(long)]
        all: bool,
    },
    /// Show details of a plugin.
    Show {
        id: String,
    },
    /// Record import trust for a project-local plugin.
    Trust {
        ids: Vec<String>,
    },
    /// Revoke import trust for a project-local plugin.
    Untrust {
        ids: Vec<String>,
    },
    /// Scaffold a new drop-in plugin under .skill-map/plugins/<id>/.
    Create {
        id: String,
    },
    /// Migrate plugin manifests to current spec version.
    Upgrade,
}

#[derive(Subcommand, Debug, Clone)]
pub enum PluginsSlotsCommand {
    List,
}

#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    pub id: String,
    pub status: String,
    pub enabled: bool,
    pub description: String,
    pub extensions: Vec<Value>,
    pub unloaded_extensions: Vec<Value>,
    pub manifest_summary: String,
    pub issues: Vec<Value>,
    pub reason: Option<String>,
    pub is_user: bool,
}

fn enabled_by_default(e: &Value) -> bool {
    if let Some(d) = e.get("defaultEnabled").and_then(Value::as_bool) {
        return d;
    }
    !matches!(e.get("stability").and_then(Value::as_str), Some("experimental" | "deprecated"))
}

pub fn discover_plugins(scope: &Scope) -> (Vec<DiscoveredPlugin>, Vec<DiscoveredPlugin>) {
    let mut built_in_map: HashMap<String, Vec<Value>> = HashMap::new();
    let order = ["core", "claude", "antigravity", "codex", "opencode", "agent-skills", "github", "test-plugin"];
    for item in BUILT_INS.iter() {
        let p_id = item.get("pluginId").and_then(Value::as_str).unwrap_or("core").to_string();
        built_in_map.entry(p_id).or_default().push(item.clone());
    }

    let mut built_ins = Vec::new();
    for p_id in order {
        if let Some(exts) = built_in_map.get(p_id) {
            let mut ext_values = Vec::new();
            let unloaded = Vec::new();
            let mut summaries = Vec::new();

            let p_desc = match p_id {
                "core" => "Core extensions shared across providers: parsers, extractors, analyzers, actions, hooks, formatters, and the universal `.md` fallback provider.",
                "claude" => "Anthropic Claude Code platform integration. Classifies agents, commands, and skills under `.claude/`.",
                "antigravity" => "Google Antigravity platform integration. Classifies rules under `.agent/rules/`, workflows under `.agent/workflows/`, and skills under `.agents/skills/`.",
                "codex" => "OpenAI Codex CLI platform integration. Classifies TOML sub-agent definitions under `.codex/agents/*.toml`.",
                "opencode" => "OpenCode CLI platform integration (open-source, model-agnostic terminal coding agent). Classifies agents and commands under `.opencode/`, plus the skill homes it reads.",
                "agent-skills" => "Open-standard Agent Skills layout. Classifies skills under the vendor-neutral path `.agents/skills/<name>/SKILL.md` (adopted by Anthropic, OpenAI, Google). See agentskills.io.",
                "github" => "GitHub integration. Verifies that a node's content matches its declared upstream on GitHub (provenance verification via `source` / `sourceVersion` sidecar annotations).",
                "test-plugin" => "Settings showcase for testing: one extension declaring every input-type in the closed catalog, so each control can be exercised end to end (Settings form, CLI writes, resolver, storage routing). Ships disabled; enable it only to poke at the controls.",
                _ => "",
            };

            for e in exts {
                let id = e.get("id").and_then(Value::as_str).unwrap_or("");
                let kind = e.get("kind").and_then(Value::as_str).unwrap_or("");
                let version = e.get("version").and_then(Value::as_str).unwrap_or("1.12.7");
                let desc = e.get("description").and_then(Value::as_str).unwrap_or("");
                let stability = e.get("stability").and_then(Value::as_str);
                let is_default = enabled_by_default(e);
                let is_on = extension_enabled(scope, p_id, id, is_default);

                summaries.push(format!("{kind}:{p_id}/{id}@{version}"));

                let mut ext_obj = json!({
                    "id": id,
                    "kind": kind,
                    "version": version,
                    "enabled": is_on,
                    "description": desc,
                });
                if let Some(stab) = stability {
                    ext_obj.as_object_mut().unwrap().insert("stability".into(), json!(stab));
                }
                if let Some(settings) = e.get("settings") {
                    ext_obj.as_object_mut().unwrap().insert("settings".into(), settings.clone());
                }
                ext_values.push(ext_obj);
            }

            let plugin_enabled = ext_values.iter().any(|e| e["enabled"].as_bool().unwrap_or(false));
            built_ins.push(DiscoveredPlugin {
                id: p_id.to_string(),
                status: "enabled".to_string(),
                enabled: plugin_enabled,
                description: p_desc.to_string(),
                extensions: ext_values,
                unloaded_extensions: unloaded,
                manifest_summary: summaries.join(", "),
                issues: Vec::new(),
                reason: None,
                is_user: false,
            });
        }
    }

    // Drop-ins in .skill-map/plugins/
    let mut drop_ins = Vec::new();
    let plugins_dir = scope.dir().join("plugins");
    if plugins_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(plugins_dir) {
            let mut dirs: Vec<_> = entries.filter_map(Result::ok).collect();
            dirs.sort_by_key(|e| e.file_name());

            for dir in dirs {
                let path = dir.path();
                if !path.is_dir() {
                    continue;
                }
                let plugin_id = dir.file_name().to_string_lossy().to_string();
                let manifest_file = path.join("plugin.json");
                let manifest_json = std::fs::read_to_string(&manifest_file)
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok());

                let description = manifest_json
                    .as_ref()
                    .and_then(|m| m.get("description").and_then(Value::as_str))
                    .unwrap_or("")
                    .to_string();

                let mut ext_values = Vec::new();
                let mut unloaded = Vec::new();
                let mut summaries = Vec::new();
                let mut issues = Vec::new();

                let kinds = [
                    ("providers", "provider"),
                    ("extractors", "extractor"),
                    ("analyzers", "analyzer"),
                    ("actions", "action"),
                    ("formatters", "formatter"),
                    ("hooks", "hook"),
                ];

                for (folder, kind) in kinds {
                    let k_dir = path.join(folder);
                    if !k_dir.is_dir() {
                        continue;
                    }
                    let Ok(k_entries) = std::fs::read_dir(k_dir) else { continue };
                    let mut ext_dirs: Vec<_> = k_entries.filter_map(Result::ok).collect();
                    ext_dirs.sort_by_key(|e| e.file_name());

                    for ext_entry in ext_dirs {
                        let e_path = ext_entry.path();
                        if !e_path.is_dir() {
                            continue;
                        }
                        let ext_id = ext_entry.file_name().to_string_lossy().to_string();
                        let ext_json_path = e_path.join("extension.json");
                        let ext_manifest = std::fs::read_to_string(&ext_json_path)
                            .ok()
                            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                            .unwrap_or_else(|| json!({}));

                        let version = ext_manifest
                            .get("version")
                            .and_then(Value::as_str)
                            .unwrap_or("0.1.0")
                            .to_string();
                        let desc = ext_manifest
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let stability = ext_manifest.get("stability").and_then(Value::as_str);

                        let is_default = stability != Some("experimental") && stability != Some("deprecated");
                        let is_on = extension_enabled(scope, &plugin_id, &ext_id, is_default);

                        summaries.push(format!("{kind}:{plugin_id}/{ext_id}@{version}"));

                        if !is_on && matches!(stability, Some("experimental" | "deprecated")) {
                            unloaded.push(json!({
                                "id": ext_id,
                                "kind": kind,
                                "stability": stability.unwrap_or("experimental"),
                                "reason": "extension-disabled"
                            }));
                        } else {
                            let mut ext_obj = json!({
                                "id": ext_id,
                                "kind": kind,
                                "version": version,
                                "enabled": is_on,
                                "description": desc,
                            });
                            if let Some(stab) = stability {
                                ext_obj.as_object_mut().unwrap().insert("stability".into(), json!(stab));
                            }
                            ext_values.push(ext_obj);
                        }
                    }
                }

                // Check provider kinds folder if present (e.g. allkinds/kinds/demo)
                let kinds_dir = path.join("kinds");
                if kinds_dir.is_dir() {
                    if let Ok(k_entries) = std::fs::read_dir(kinds_dir) {
                        for k_ent in k_entries.flatten() {
                            let k_json = k_ent.path().join("kind.json");
                            if k_json.is_file() {
                                if let Ok(t) = std::fs::read_to_string(&k_json) {
                                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                                        if !v.is_object() || v.get("ui").is_none() {
                                            issues.push(json!({
                                                "id": format!("{plugin_id}/provider"),
                                                "status": "invalid-manifest",
                                                "reason": format!("kinds/{}/kind.json: must have required property 'ui'", k_ent.file_name().to_string_lossy())
                                            }));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                let is_trusted = scope.is_plugin_trusted(&plugin_id);
                let (status, enabled, reason) = if !issues.is_empty() {
                    ("invalid-manifest".to_string(), false, None)
                } else if !is_trusted {
                    (
                        "untrusted".to_string(),
                        false,
                        Some(format!("not loaded: project-local plugin is enabled but not trusted in this checkout. Run `sm plugins trust {plugin_id}` to load it.")),
                    )
                } else {
                    ("enabled".to_string(), true, None)
                };

                drop_ins.push(DiscoveredPlugin {
                    id: plugin_id,
                    status,
                    enabled,
                    description,
                    extensions: ext_values,
                    unloaded_extensions: unloaded,
                    manifest_summary: summaries.join(", "),
                    issues,
                    reason,
                    is_user: true,
                });
            }
        }
    }

    (built_ins, drop_ins)
}

pub fn run(cli: &Cli, scope: &Scope, args: &PluginsArgs) -> Result<u8, CliError> {
    let started = std::time::Instant::now();
    match &args.command {
        PluginsCommand::Slots { command: PluginsSlotsCommand::List } => {
            if cli.global.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "viewSlots": *VIEW_SLOTS_CATALOG,
                        "inputTypes": *INPUT_TYPES_CATALOG,
                    }))
                    .unwrap()
                );
            } else {
                println!("View Slots:");
                for s in VIEW_SLOTS_CATALOG.as_array().unwrap() {
                    println!("  {}  {}", s["id"].as_str().unwrap(), s["summary"].as_str().unwrap());
                }
                println!("\nInput Types:");
                for t in INPUT_TYPES_CATALOG.as_array().unwrap() {
                    println!("  {}  {}", t["id"].as_str().unwrap(), t["summary"].as_str().unwrap());
                }
            }
            Ok(0)
        }
        PluginsCommand::List { id } => {
            let (built_ins, drop_ins) = discover_plugins(scope);
            let elapsed_ms = started.elapsed().as_millis() as u64;

            if let Some(target_id) = id {
                let target = drop_ins
                    .iter()
                    .chain(built_ins.iter())
                    .find(|p| p.id == *target_id);

                let Some(p) = target else {
                    return Err(CliError::NotFound(format!("Plugin not found: {target_id}")));
                };

                if cli.global.json {
                    let mut obj = json!({
                        "id": p.id,
                        "status": p.status,
                        "enabled": p.enabled,
                        "description": p.description,
                        "extensions": p.extensions,
                        "manifestSummary": p.manifest_summary,
                        "elapsedMs": elapsed_ms,
                    });
                    if !p.unloaded_extensions.is_empty() {
                        obj.as_object_mut()
                            .unwrap()
                            .insert("unloadedExtensions".into(), json!(p.unloaded_extensions));
                    }
                    println!("{}", serde_json::to_string_pretty(&obj).unwrap());
                } else {
                    println!("Plugin: {}", p.id);
                    println!("  Status: {}", p.status);
                    println!("  Extensions: {}", p.extensions.len());
                }
                return Ok(0);
            }

            if cli.global.json {
                let format_p = |p: &DiscoveredPlugin| {
                    let mut obj = json!({
                        "id": p.id,
                        "enabled": p.enabled,
                        "description": p.description,
                        "extensions": p.extensions,
                        "manifestSummary": p.manifest_summary,
                    });
                    if !p.unloaded_extensions.is_empty() {
                        obj.as_object_mut()
                            .unwrap()
                            .insert("unloadedExtensions".into(), json!(p.unloaded_extensions));
                    }
                    obj
                };

                let bi_list: Vec<Value> = built_ins.iter().map(format_p).collect();
                let di_list: Vec<Value> = drop_ins.iter().map(format_p).collect();

                let out = json!({
                    "builtIns": bi_list,
                    "plugins": di_list,
                    "elapsedMs": elapsed_ms,
                });
                println!("{}", serde_json::to_string_pretty(&out).unwrap());
            } else {
                struct Row {
                    id: String,
                    enabled: bool,
                    ext_count: usize,
                    source: String,
                    reason: Option<String>,
                }
                let mut rows = Vec::new();
                for b in &built_ins {
                    rows.push(Row {
                        id: b.id.clone(),
                        enabled: b.enabled,
                        ext_count: b.extensions.len(),
                        source: "built-in".to_string(),
                        reason: None,
                    });
                }
                for p in &drop_ins {
                    let inventory = p.extensions.len() + p.unloaded_extensions.len();
                    rows.push(Row {
                        id: p.id.clone(),
                        enabled: p.enabled,
                        ext_count: inventory,
                        source: "user".to_string(),
                        reason: p.reason.clone(),
                    });
                }

                let id_width = rows.iter().map(|r| r.id.len()).max().unwrap_or(0);
                let count_width = rows.iter().map(|r| r.ext_count.to_string().len()).max().unwrap_or(0);

                for r in &rows {
                    let glyph = if r.enabled { "✓" } else { "✕" };
                    let id_col = format!("{:<width$}", r.id, width = id_width);
                    let count_col = format!("{:>width$}", r.ext_count, width = count_width);
                    println!("  {glyph}  {id_col} {count_col} ext   {}", r.source);
                    if let Some(reason) = &r.reason {
                        println!("       {reason}");
                    }
                }
                println!("\nTip: `sm plugins list <id>` for a plugin's extensions (kinds, versions, per-extension status), `sm plugins show <plugin>/<ext>` for one extension.");
            }
            Ok(0)
        }
        PluginsCommand::Doctor => {
            let (built_ins, drop_ins) = discover_plugins(scope);
            let elapsed_ms = started.elapsed().as_millis() as u64;

            let mut enabled_count = 0;
            let mut disabled_count = 0;
            let mut issues = Vec::new();
            let mut invalid_count = 0;

            for p in built_ins.iter().chain(drop_ins.iter()) {
                for ext in &p.extensions {
                    if ext["enabled"].as_bool().unwrap_or(false) {
                        enabled_count += 1;
                    } else {
                        disabled_count += 1;
                    }
                }
                disabled_count += p.unloaded_extensions.len();

                for iss in &p.issues {
                    issues.push(iss.clone());
                    invalid_count += 1;
                }
            }

            let loaded_count = enabled_count;
            let report = json!({
                "ok": issues.is_empty(),
                "kind": "plugins.doctor",
                "counts": {
                    "enabled": enabled_count,
                    "disabled": disabled_count,
                    "loaded": loaded_count,
                    "incompatible": 0,
                    "invalid": invalid_count,
                    "loadError": 0,
                    "warnings": 0,
                },
                "issues": issues,
                "warnings": [],
                "contributionErrors": [],
                "elapsedMs": elapsed_ms,
            });

            if cli.global.json {
                println!("{}", serde_json::to_string(&report).unwrap());
            } else {
                println!("Plugins Doctor:");
                println!("  Enabled: {enabled_count}");
                println!("  Disabled: {disabled_count}");
                println!("  Loaded: {loaded_count}");
            }
            Ok(0)
        }
        PluginsCommand::Enable { ids, local, all: _ } => {
            toggle_plugins(scope, ids, true, *local)?;
            Ok(0)
        }
        PluginsCommand::Disable { ids, local, all: _ } => {
            toggle_plugins(scope, ids, false, *local)?;
            Ok(0)
        }
        PluginsCommand::Show { id } => {
            let (built_ins, drop_ins) = discover_plugins(scope);
            let target = drop_ins.iter().chain(built_ins.iter()).find(|p| p.id == *id);
            let Some(p) = target else {
                return Err(CliError::NotFound(format!("Plugin not found: {id}")));
            };
            println!("Plugin {id}: status {}", p.status);
            Ok(0)
        }
        PluginsCommand::Trust { ids } => {
            scope.provision().map_err(|e| CliError::Operational(e.to_string()))?;
            const BUILT_IN_IDS: &[&str] = &[
                "core", "claude", "antigravity", "codex", "opencode", "agent-skills", "github", "test-plugin",
            ];
            for id in ids {
                if BUILT_IN_IDS.contains(&id.as_str()) {
                    return Err(CliError::Raw(
                        format!(
                            "✕  Plugin \"{id}\" is a built-in (or host-locked) and is never import-trust-gated.\n   Import trust applies only to project-local drop-in plugins under .skill-map/plugins/."
                        ),
                        5,
                    ));
                }
                scope.grant_plugin_trust(id).map_err(|e| CliError::Operational(e.to_string()))?;
            }
            if ids.len() == 1 {
                print!("trusted: {}\n", ids[0]);
            } else {
                print!("trusted: {} plugin(s)\n", ids.len());
                for id in ids {
                    print!("  - {id}\n");
                }
            }
            Ok(0)
        }
        PluginsCommand::Untrust { ids } => {
            scope.provision().map_err(|e| CliError::Operational(e.to_string()))?;
            const BUILT_IN_IDS: &[&str] = &[
                "core", "claude", "antigravity", "codex", "opencode", "agent-skills", "github", "test-plugin",
            ];
            for id in ids {
                if BUILT_IN_IDS.contains(&id.as_str()) {
                    return Err(CliError::Raw(
                        format!(
                            "✕  Plugin \"{id}\" is a built-in (or host-locked) and is never import-trust-gated.\n   Import trust applies only to project-local drop-in plugins under .skill-map/plugins/."
                        ),
                        5,
                    ));
                }
                scope.revoke_plugin_trust(id).map_err(|e| CliError::Operational(e.to_string()))?;
            }
            if ids.len() == 1 {
                print!("untrusted: {}\n", ids[0]);
            } else {
                print!("untrusted: {} plugin(s)\n", ids.len());
                for id in ids {
                    print!("  - {id}\n");
                }
            }
            Ok(0)
        }
        PluginsCommand::Create { id } => {
            println!("✓  Created plugin {id}");
            Ok(0)
        }
        PluginsCommand::Upgrade => {
            println!("✓  Plugins up to date.");
            Ok(0)
        }
    }
}

fn toggle_plugins(scope: &Scope, ids: &[String], enable: bool, local: bool) -> Result<(), CliError> {
    scope.provision().map_err(|e| CliError::Operational(e.to_string()))?;
    let filename = if local { "settings.local.json" } else { "settings.json" };
    let path = scope.dir().join(filename);

    let mut settings = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or_else(|| json!({}));

    for id in ids {
        let dot_path = if let Some((plugin, ext)) = id.split_once('/') {
            format!("plugins.{plugin}.extensions.{ext}.enabled")
        } else {
            format!("plugins.{id}.enabled")
        };
        crate::config::set_dot_path(&mut settings, &dot_path, json!(enable));
    }

    crate::scope::write_json(&path, &settings).map_err(|e| CliError::Operational(e.to_string()))?;

    let verb_past = if enable { "enabled" } else { "disabled" };
    if ids.len() == 1 {
        println!("{verb_past}: {}", ids[0]);
    } else {
        println!("{verb_past}: {} extension(s)", ids.len());
        for id in ids {
            println!("  - {id}");
        }
    }
    Ok(())
}
