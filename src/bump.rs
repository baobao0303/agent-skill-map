//! `sm bump` (the `core/node-bump` deterministic action) and the `.sm`
//! consent gate shared by every verb that writes a sidecar.

use crate::cli::{Cli, CliError};
use crate::model::Node;
use crate::scope::{self, Scope};
use crate::{format, jsjson, sidecar, store};
use serde_json::{Map, Value, json};
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

pub struct Args<'a> {
    pub node: Option<&'a str>,
    pub pending: bool,
    pub staged: bool,
    pub force: bool,
    pub yes: bool,
}

fn usage(head: &str, hint: &str) -> CliError {
    CliError::Usage(format!("{head}\n   {hint}"))
}

/// Built-in extension enablement: per-extension, then per-plugin, then the
/// shipped default (`plugins.<plugin>.extensions.<id>.enabled`).
pub fn extension_enabled(scope: &Scope, plugin: &str, id: &str, default: bool) -> bool {
    for layer in [scope.settings_local(), scope.settings()] {
        let p = layer.get("plugins").and_then(|p| p.get(plugin));
        if let Some(v) = p.and_then(|p| p.get("extensions")).and_then(|e| e.get(id)).and_then(|e| e.get("enabled")).and_then(Value::as_bool) {
            return v;
        }
        if let Some(v) = p.and_then(|p| p.get("enabled")).and_then(Value::as_bool) {
            return v;
        }
    }
    default
}

/// `allowSidecarWriters: false` is a team policy no flag overrides.
pub fn assert_writers_allowed(scope: &Scope) -> Result<(), CliError> {
    if scope.sidecar_writers_allowed() {
        return Ok(());
    }
    // Printed as `✕ <message>` (one space), unlike the two-space blocks.
    Err(CliError::Raw(
        "✕ Sidecar-writing extensions are disabled in this project ('allowSidecarWriters' is false in .skill-map/settings.json). This is a team-level project policy and cannot be overridden.".into(),
        2,
    ))
}

/// Consent to write `.sm` files: an existing grant, `--yes` (persisted as a
/// grant), or an interactive yes on a TTY. `verb` prefixes the refusals.
pub fn ensure_consent(scope: &Scope, yes: bool, verb: &str) -> Result<(), CliError> {
    if scope.local_key("allowEditSmFiles") == Some(Value::Bool(true)) {
        return Ok(());
    }
    let grant = |s: &Scope| s.grant_local_key("allowEditSmFiles", Value::Bool(true)).map_err(|e| CliError::Operational(e.to_string()));
    if yes {
        return grant(scope);
    }
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Err(usage(
            &format!("{verb}: consent required to write .sm sidecar files in this project."),
            "Pass --yes to grant (writes to .skill-map/settings.local.json, gitignored).",
        ));
    }
    eprint!(
        "ℹ  skill-map needs consent to create .sm sidecar files next to your\n   .md sources. Your choice is saved to .skill-map/settings.local.json\n   (gitignored) and this prompt will not appear again.\n\nAllow .sm sidecar writes in this project? [Y/n] "
    );
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    let _ = stdin.lock().read_line(&mut answer);
    let a = answer.trim().to_ascii_lowercase();
    if a.is_empty() || a == "y" || a == "yes" || !(a == "n" || a == "no") {
        return grant(scope);
    }
    Err(CliError::Raw(format!("ℹ  {verb}: aborted by user. No .sm sidecar files were written."), 2))
}

/// `resolveGitAuthorName`: `git config user.name` inside a work tree, else `cli`.
fn invoker(root: &Path) -> String {
    let in_repo = root.ancestors().any(|d| d.join(".git").exists());
    if !in_repo {
        return "cli".into();
    }
    std::process::Command::new("git")
        .args(["config", "user.name"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "cli".into())
}

enum Plan {
    Refused,
    Skipped,
    Bumped { version: i64, created: bool, changes: Map<String, Value> },
}

fn overlay_version(node: &Node) -> Option<f64> {
    node.sidecar.as_ref()?.get("annotations")?.get("version")?.as_f64().filter(|v| v.is_finite())
}

fn plan(node: &Node, force: bool, invoker: &str) -> Plan {
    let overlay = node.sidecar.as_ref();
    let present = overlay.and_then(|o| o.get("present")).and_then(Value::as_bool) == Some(true);
    let status = overlay.and_then(|o| o.get("status")).and_then(Value::as_str);
    if present && status == Some("fresh") && overlay_version(node).is_some() {
        return if force { Plan::Skipped } else { Plan::Refused };
    }
    let version = overlay_version(node).map_or(0, |v| v as i64) + 1;
    let now = scope::iso_now();
    let mut audit = Map::new();
    audit.insert("lastBumpedAt".into(), json!(now));
    audit.insert("lastBumpedBy".into(), json!(invoker));
    if !present {
        audit.insert("createdAt".into(), json!(now));
        audit.insert("createdBy".into(), json!(invoker));
    }
    let changes = json!({
        "identity": { "path": node.path, "bodyHash": node.body_hash, "frontmatterHash": node.frontmatter_hash },
        "annotations": { "version": version },
        "audit": audit,
    });
    Plan::Bumped { version, created: !present, changes: changes.as_object().cloned().unwrap_or_default() }
}

pub fn run(cli: &Cli, scope: &Scope, db: &Path, a: Args) -> Result<u8, CliError> {
    if a.pending && a.node.is_some() {
        return Err(usage(
            "--pending cannot be combined with a positional <node.path>.",
            "Run `sm bump <node.path>` for a single bump, or `sm bump --pending` to batch every stale node.",
        ));
    }
    if !a.pending && a.node.is_none() {
        return Err(usage(
            "Pass <node.path> for a single-node bump, or --pending for batch mode.",
            "Examples: `sm bump path/to/node.md` (single), `sm bump --pending` (every stale sidecar).",
        ));
    }
    if a.staged && !a.pending {
        return Err(usage(
            "--staged is only valid together with --pending.",
            "Pass `--pending --staged` to bump every stale sidecar and `git add` each successful write.",
        ));
    }
    if !extension_enabled(scope, "core", "node-bump", false) {
        return Err(usage(
            "The core/node-bump extension is disabled in this project.",
            "Enable it in the UI (Settings > Plugins) or set `plugins.core.extensions.node-bump.enabled: true` in .skill-map/settings.json, then retry.",
        ));
    }
    assert_writers_allowed(scope)?;
    let not_found = |path: &str| {
        CliError::NotFound(format!(
            "Node not found in the persisted scan: {path}\n   Run `sm scan` first, then retry with the path as it appears in `sm list`."
        ))
    };
    if !db.exists() {
        return Err(not_found(a.node.unwrap_or("<pending>")));
    }
    let conn = store::open_existing(db).map_err(|e| CliError::Operational(e.to_string()))?;
    let nodes = store::load(&conn).map_err(|e| CliError::Operational(e.to_string()))?.map(|s| s.nodes).unwrap_or_default();
    let who = invoker(&scope.root);
    let write = |node: &Node, changes: &Map<String, Value>| -> Result<std::path::PathBuf, String> {
        let path = sidecar::path_for(&scope.root.join(&node.path));
        sidecar::apply_patch(&path, changes).map(|_| path.clone()).map_err(|e| format!("sidecar write failed for {}: {e}", path.display()))
    };

    if let Some(target) = a.node {
        let node = nodes.iter().find(|n| n.path == target).ok_or_else(|| not_found(target))?;
        return match plan(node, a.force, &who) {
            Plan::Refused => Err(usage(
                &format!("{} is fresh (no drift versus its sidecar)", format::sanitize(&node.path)),
                "Pass --force to bump anyway.",
            )),
            Plan::Skipped => Ok(0),
            Plan::Bumped { version, created, changes } => {
                ensure_consent(scope, a.yes, "sm bump")?;
                let path = write(node, &changes).map_err(|m| CliError::Operational(format!("sm bump: {m}")))?;
                if cli.global.json {
                    let mut report = json!({ "ok": true, "version": version });
                    if created {
                        report["createdSidecar"] = json!(true);
                    }
                    println!("{}", jsjson::to_string(&report));
                } else if created {
                    println!(
                        "✓  Created {} and bumped {} to annotations.version={version}.",
                        format::sanitize(&path.display().to_string()),
                        format::sanitize(&node.path)
                    );
                } else {
                    println!("✓  Bumped {} to annotations.version={version}.", format::sanitize(&node.path));
                }
                Ok(0)
            }
        };
    }

    // --pending: every node whose persisted overlay reports drift.
    if a.staged {
        if !scope.root.ancestors().any(|d| d.join(".git").exists()) {
            return Err(CliError::NotFound(format!(
                "--staged: not inside a git repository (no .git/ found at or above {}).\n   Drop `--staged` to bump without staging, or run `sm bump` from inside a git checkout.",
                scope.root.display()
            )));
        }
        if std::process::Command::new("git").arg("--version").output().is_err() {
            return Err(usage("--staged: `git` binary not found on PATH.", "Install git or run without --staged."));
        }
    }
    let started = std::time::Instant::now();
    let mut stale: Vec<&Node> = nodes
        .iter()
        .filter(|n| {
            let o = n.sidecar.as_ref();
            o.and_then(|o| o.get("present")).and_then(Value::as_bool) == Some(true)
                && o.and_then(|o| o.get("status")).and_then(Value::as_str).is_some_and(|s| s != "fresh")
        })
        .collect();
    stale.sort_by(|x, y| format::locale_compare(&x.path, &y.path));
    if stale.is_empty() {
        if cli.global.json {
            println!(
                "{}",
                jsjson::to_string(&json!({ "bumped": 0, "refused": 0, "skipped": 0, "errors": [], "elapsedMs": started.elapsed().as_millis() as u64 }))
            );
        } else {
            println!("sm bump --pending: no stale sidecars in the persisted scan. Nothing to do.");
        }
        return Ok(0);
    }
    if !cli.global.json && !cli.global.quiet {
        eprintln!("sm bump --pending: scanning {} stale node(s).", stale.len());
    }
    let (mut bumped, mut refused, mut skipped) = (0, 0, 0);
    let mut errors: Vec<(String, String)> = Vec::new();
    let mut lines = Vec::new();
    let mut consented = false;
    for node in stale {
        match plan(node, a.force, &who) {
            Plan::Refused => {
                refused += 1;
                lines.push(format!("  refused {} (fresh, would need --force)", node.path));
            }
            Plan::Skipped => {
                skipped += 1;
                lines.push(format!("  skipped {} (noop)", node.path));
            }
            Plan::Bumped { version, created, changes } => {
                if !consented {
                    ensure_consent(scope, a.yes, "sm bump")?;
                    consented = true;
                }
                match write(node, &changes) {
                    Ok(path) => {
                        bumped += 1;
                        lines.push(format!("  bumped {} -> v{version}{}", node.path, if created { " (new sidecar)" } else { "" }));
                        if a.staged {
                            stage(scope, &path, cli.global.json);
                        }
                    }
                    Err(m) => {
                        lines.push(format!("  error  {}: {m}", node.path));
                        errors.push((node.path.clone(), m));
                    }
                }
            }
        }
    }
    if cli.global.json {
        let errs: Vec<Value> = errors.iter().map(|(p, m)| json!({ "nodePath": p, "message": m })).collect();
        println!(
            "{}",
            jsjson::to_string(&json!({
                "bumped": bumped, "refused": refused, "skipped": skipped, "errors": errs,
                "elapsedMs": started.elapsed().as_millis() as u64,
            }))
        );
    } else {
        for l in &lines {
            println!("{l}");
        }
        if !cli.global.quiet {
            eprintln!(
                "sm bump --pending: bumped {bumped}, refused {refused}, skipped {skipped}, errors {}.",
                errors.len()
            );
        }
    }
    Ok(if errors.is_empty() { 0 } else { 1 })
}

/// `git add` one bumped sidecar; a failure is a warning, the batch goes on.
fn stage(scope: &Scope, path: &Path, json_mode: bool) {
    let out = std::process::Command::new("git").arg("add").arg("--").arg(path).current_dir(&scope.root).output();
    let err = match out {
        Err(e) => e.to_string(),
        Ok(o) if o.status.success() => return,
        Ok(o) => {
            let e = String::from_utf8_lossy(&o.stderr).trim().to_string();
            if e.is_empty() { format!("git add exited with code {}", o.status.code().unwrap_or(-1)) } else { e }
        }
    };
    if !json_mode {
        let p = path.display();
        eprintln!(
            "⚠  --staged: git add failed for {p}: {err}.\n   Continuing batch; stage the sidecar manually with `git add {p}` afterwards."
        );
    }
}
