//! `sm sidecars refresh | prune | annotate`: filesystem-layer sidecar verbs.

use crate::bump::ensure_consent;
use crate::cli::{Cli, CliError};
use crate::model::Node;
use crate::scope::Scope;
use crate::{format, jsjson, scan, sidecar, store};
use serde_json::{Map, Value, json};
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

/// Nodes of the persisted scan; `None` when there is no DB at all.
fn persisted_nodes(db: &Path) -> Result<Option<Vec<Node>>, CliError> {
    if !db.exists() {
        return Ok(None);
    }
    let conn = store::open_existing(db).map_err(|e| CliError::Operational(e.to_string()))?;
    Ok(Some(store::load(&conn).map_err(|e| CliError::Operational(e.to_string()))?.map(|s| s.nodes).unwrap_or_default()))
}

fn find_node(db: &Path, path: &str) -> Result<Node, CliError> {
    persisted_nodes(db)?.and_then(|ns| ns.into_iter().find(|n| n.path == path)).ok_or_else(|| {
        CliError::NotFound(format!(
            "Node not found in the persisted scan: {path}\n   Run `sm scan` first, then retry with the path as it appears in `sm list`."
        ))
    })
}

/// The consent gate inside the store: the writers policy first (reported
/// through the verb's own failure line), then the `.sm` consent.
fn gate(scope: &Scope, yes: bool, failed: &str) -> Result<(), CliError> {
    if !scope.sidecar_writers_allowed() {
        return Err(CliError::Operational(format!(
            "{failed}: Sidecar-writing extensions are disabled in this project ('allowSidecarWriters' is false in .skill-map/settings.json). This is a team-level project policy and cannot be overridden."
        )));
    }
    ensure_consent(scope, yes, "sm sidecars")
}

fn identity(node: &Node) -> Value {
    json!({ "path": node.path, "bodyHash": node.body_hash, "frontmatterHash": node.frontmatter_hash })
}

pub fn refresh(cli: &Cli, scope: &Scope, db: &Path, node_path: &str, yes: bool) -> Result<u8, CliError> {
    let node = find_node(db, node_path)?;
    let sm = sidecar::path_for(&scope.root.join(&node.path));
    let overlay = node.sidecar.as_ref();
    if overlay.and_then(|o| o.get("present")).and_then(Value::as_bool) != Some(true) {
        return Err(CliError::NotFound(format!(
            "No sidecar at {}\n   Run `sm sidecars annotate` to scaffold one, or `sm bump` to create it via the Action.",
            sm.display()
        )));
    }
    if overlay.and_then(|o| o.get("status")).and_then(Value::as_str) == Some("fresh") {
        // Info channel: silent under --json and -q.
        if !cli.global.quiet && !cli.global.json {
            eprintln!("✓  {} is fresh (hashes match the live node). Nothing to do.", format::sanitize(&node.path));
        }
        return Ok(0);
    }
    gate(scope, yes, "sm sidecars refresh")?;
    let mut changes = Map::new();
    changes.insert("identity".into(), identity(&node));
    sidecar::apply_patch(&sm, &changes).map_err(|m| CliError::Operational(format!("sm sidecars refresh: {m}")))?;
    if cli.global.json {
        println!(
            "{}",
            jsjson::to_string(&json!({ "nodePath": node.path, "sidecarPath": sm.display().to_string(), "status": "refreshed" }))
        );
    } else {
        println!(
            "✓  Refreshed {} (bodyHash + frontmatterHash sync'd to live node, version unchanged).",
            format::sanitize(&sm.display().to_string())
        );
    }
    Ok(0)
}

pub fn annotate(cli: &Cli, scope: &Scope, db: &Path, node_path: &str, force: bool, yes: bool) -> Result<u8, CliError> {
    let node = find_node(db, node_path)?;
    let sm = sidecar::path_for(&scope.root.join(&node.path));
    if sm.exists() {
        if !force {
            return Err(CliError::Usage(format!("{} already exists\n   Pass --force to overwrite.", sm.display())));
        }
        std::fs::remove_file(&sm).map_err(|e| CliError::Operational(format!("sm sidecars annotate: {e}")))?;
    }
    gate(scope, yes, "sm sidecars annotate")?;
    let mut changes = Map::new();
    changes.insert("identity".into(), identity(&node));
    changes.insert("annotations".into(), json!({}));
    sidecar::apply_patch(&sm, &changes).map_err(|m| CliError::Operational(format!("sm sidecars annotate: {m}")))?;
    if cli.global.json {
        println!("{}", jsjson::to_string(&json!({ "nodePath": node.path, "sidecarPath": sm.display().to_string(), "created": true })));
    } else {
        println!("✓  Created {}. Edit it, then run `sm bump {}` to commit the version.", sm.display(), node.path);
    }
    Ok(0)
}

/// `confirm()` with a `[y/N]` default; a closed / non-TTY stdin reads as "no".
fn confirm_no(question: &str) -> bool {
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().lock().read_line(&mut answer);
    let a = answer.trim().to_ascii_lowercase();
    a == "y" || a == "yes"
}

pub fn prune(cli: &Cli, scope: &Scope, dry_run: bool, yes: bool) -> Result<u8, CliError> {
    let started = std::time::Instant::now();
    let orphans: Vec<(String, String)> = scan::orphan_sidecars(&scope.root, None)
        .into_iter()
        .map(|rel| {
            let abs = scope.root.join(&rel).display().to_string();
            let md = format!("{}.md", &abs[..abs.len() - 3]);
            (abs, md)
        })
        .collect();
    if orphans.is_empty() {
        if cli.global.json {
            println!("{}", jsjson::to_string(&json!({ "deleted": 0, "wouldDelete": 0, "items": [] })));
        } else {
            println!("✓  No orphan .sm files found under the configured roots.");
        }
        return Ok(0);
    }
    if !dry_run && !yes {
        let lines: Vec<String> =
            orphans.iter().map(|(s, m)| format!("  {} (expected {})", format::sanitize(s), format::sanitize(m))).collect();
        let n = orphans.len();
        let q = format!(
            "sm sidecars prune is about to delete {n} orphan .sm file{}:\n{}\nProceed?",
            if n == 1 { "" } else { "s" },
            lines.join("\n")
        );
        if !std::io::stdin().is_terminal() {
            // The original's readline never answers on a closed stdin: the
            // prompt is left hanging and the process ends with exit 0.
            eprint!("{q} [y/N] ");
            return Ok(0);
        }
        if !confirm_no(&q) {
            if !cli.global.quiet {
                eprintln!("ℹ  sm sidecars prune: aborted by user. No files deleted.");
            }
            return Ok(0);
        }
    }
    let mut items: Vec<Value> = Vec::new();
    let mut errors = 0;
    for (sm, md) in &orphans {
        if dry_run {
            items.push(json!({ "sidecarPath": sm, "expectedMd": md, "deleted": false }));
            continue;
        }
        match std::fs::remove_file(sm) {
            Ok(()) => items.push(json!({ "sidecarPath": sm, "expectedMd": md, "deleted": true })),
            Err(e) => {
                errors += 1;
                let message = e.to_string();
                if !cli.global.json {
                    eprintln!(
                        "⚠  Failed to delete {sm}: {message}.\n   Continuing with the remaining files; re-run `sm sidecars prune` after fixing the underlying issue."
                    );
                }
                items.push(json!({ "sidecarPath": sm, "expectedMd": md, "deleted": false, "error": message }));
            }
        }
    }
    let deleted = items.iter().filter(|i| i["deleted"] == json!(true)).count();
    if cli.global.json {
        println!(
            "{}",
            jsjson::to_string(&json!({
                "deleted": deleted,
                "wouldDelete": if dry_run { items.len() } else { 0 },
                "errors": errors,
                "items": items,
                "elapsedMs": started.elapsed().as_millis() as u64,
            }))
        );
    } else {
        for i in &items {
            let action = if dry_run {
                "would delete"
            } else if i["deleted"] == json!(true) {
                "deleted"
            } else {
                "failed"
            };
            println!("  {action} {} (expected {})", i["sidecarPath"].as_str().unwrap_or(""), i["expectedMd"].as_str().unwrap_or(""));
        }
        if !cli.global.quiet && !cli.global.json {
            let n = if dry_run { items.len() } else { deleted };
            let plural = if n == 1 { "" } else { "s" };
            if dry_run {
                eprintln!("⋯  Would delete {n} orphan .sm file{plural}  (no changes made)");
            } else {
                eprintln!("✓  Deleted {n} orphan .sm file{plural}.");
            }
        }
    }
    Ok(if errors > 0 { 1 } else { 0 })
}
