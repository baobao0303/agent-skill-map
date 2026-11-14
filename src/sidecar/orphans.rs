//! `sm orphans [reconcile | undo-rename]`: act on the rename heuristic's
//! orphan / auto-rename issues.

use crate::cli::{Cli, CliError};
use crate::model::Issue;
use crate::scope::Scope;
use crate::{format, jsjson, store};
use serde_json::{Value, json};
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

const RULES: [&str; 3] = ["orphan", "auto-rename-medium", "auto-rename-ambiguous"];

fn open(db: &Path) -> Result<rusqlite::Connection, CliError> {
    if !db.exists() {
        return Err(CliError::NotFound(format!("DB not found at {}\n   Run `sm scan` first.", db.display())));
    }
    store::open_existing(db).map_err(|e| CliError::Operational(e.to_string()))
}

fn active(conn: &rusqlite::Connection, pred: impl Fn(&Issue) -> bool) -> Result<Vec<(i64, Issue)>, CliError> {
    Ok(store::issues_with_ids(conn)
        .map_err(|e| CliError::Operational(e.to_string()))?
        .into_iter()
        .filter(|(_, i)| RULES.contains(&i.analyzer_id.as_str()) && pred(i))
        .collect())
}

fn data_str<'a>(i: &'a Issue, k: &str) -> Option<&'a str> {
    i.get("data").and_then(|d| d.get(k)).and_then(Value::as_str)
}

pub fn list(cli: &Cli, db: &Path, kind: Option<&str>) -> Result<u8, CliError> {
    let filter = match kind {
        None => None,
        Some("orphan") => Some("orphan"),
        Some("medium") => Some("auto-rename-medium"),
        Some("ambiguous") => Some("auto-rename-ambiguous"),
        Some(k) => return Err(CliError::Usage(format!("--kind: invalid value \"{k}\".\n   Allowed: orphan, medium, ambiguous."))),
    };
    let conn = open(db)?;
    let found: Vec<Issue> = active(&conn, |i| filter.is_none_or(|f| i.analyzer_id == f))?.into_iter().map(|(_, i)| i).collect();
    if cli.global.json {
        println!("{}", jsjson::to_string(&found));
    } else if found.is_empty() {
        println!("✓  No orphan / auto-rename issues.");
    } else {
        let rows: Vec<(String, String, String)> = found
            .iter()
            .map(|i| {
                let subject = i.node_ids.first().map_or_else(|| "(no node)".to_string(), |s| format::sanitize(s));
                (format::sanitize(&i.analyzer_id), subject, format::sanitize(&i.message))
            })
            .collect();
        let aw = rows.iter().map(|r| r.0.encode_utf16().count()).max().unwrap_or(0);
        let sw = rows.iter().map(|r| r.1.encode_utf16().count()).max().unwrap_or(0);
        let pad = |s: &str, w: usize| format!("{s}{}", " ".repeat(w.saturating_sub(s.encode_utf16().count())));
        let mut out = format!("sm orphans: {} {}\n\n", found.len(), if found.len() == 1 { "issue" } else { "issues" });
        for (a, s, m) in &rows {
            out += &format!("  ⚠  {}  {}  {m}\n", pad(a, aw), pad(s, sw));
        }
        out += "\nTip: `sm orphans reconcile <path>` to reattach, `sm orphans undo-rename` to revert.\n";
        print!("{out}");
    }
    Ok(0)
}

fn breakdown(r: &store::MigrateReport) -> String {
    format!(
        "{} rows · jobs {} · execs {} · summaries {} · findings {} · enrichments {} · kv {} · favorites {}",
        r.total(),
        r.jobs,
        r.executions,
        r.summaries,
        r.findings,
        r.enrichments,
        r.plugin_kvs,
        r.node_favorites
    )
}

pub fn reconcile(cli: &Cli, scope: &Scope, db: &Path, orphan: &str, to: &str, dry_run: bool) -> Result<u8, CliError> {
    let mut conn = open(db)?;
    let live = store::load(&conn).map_err(|e| CliError::Operational(e.to_string()))?.is_some_and(|s| s.nodes.iter().any(|n| n.path == to));
    if !live {
        return Err(CliError::NotFound(format!(
            "sm orphans reconcile: target node \"{to}\" not found in scan_nodes.\n   Run `sm scan` first, then retry with the path as it appears in `sm list`."
        )));
    }
    let candidates = active(&conn, |i| i.analyzer_id == "orphan" && data_str(i, "path") == Some(orphan))?;
    if candidates.is_empty() {
        return Err(CliError::NotFound(format!(
            "sm orphans reconcile: no active orphan issue found for \"{orphan}\".\n   Listing first: run `sm orphans` to see the open issues."
        )));
    }
    let tx = conn.transaction().map_err(|e| CliError::Operational(e.to_string()))?;
    let report = store::migrate_node_fks(&tx, orphan, to).map_err(|e| CliError::Operational(e.to_string()))?;
    if dry_run {
        drop(tx); // rollback
        println!("⋯  Would reconcile {orphan} → {to}  (dry-run)");
    } else {
        for (id, _) in &candidates {
            tx.execute("DELETE FROM scan_issues WHERE id = ?1", [id]).map_err(|e| CliError::Operational(e.to_string()))?;
        }
        tx.commit().map_err(|e| CliError::Operational(e.to_string()))?;
        scope.log_entry(json!({ "op": "orphans.reconcile", "target": orphan, "channel": "cli", "outcome": "ok", "detail": format!("to={to}") }));
        println!("✓  Reconciled {orphan} → {to}");
    }
    println!("   {}", breakdown(&report));
    if report.collisions > 0 && !cli.global.json && !cli.global.quiet {
        let n = report.collisions;
        let plural = if n == 1 { "" } else { "s" };
        if dry_run {
            eprintln!("⚠  {n} composite-PK collision{plural} would be skipped; destination rows preserved.");
        } else {
            eprintln!("⚠  {n} composite-PK collision{plural}; destination rows preserved.");
        }
    }
    Ok(0)
}

pub struct UndoArgs<'a> {
    pub new_path: &'a str,
    pub from: Option<&'a str>,
    pub force: bool,
    pub dry_run: bool,
}

pub fn undo_rename(cli: &Cli, scope: &Scope, db: &Path, a: UndoArgs) -> Result<u8, CliError> {
    let mut conn = open(db)?;
    let candidates = active(&conn, |i| i.analyzer_id != "orphan" && i.node_ids.iter().any(|n| n == a.new_path))?;
    let new_path = a.new_path;
    if candidates.is_empty() {
        return Err(CliError::NotFound(format!(
            "sm orphans undo-rename: no active auto-rename issue targets \"{new_path}\".\n   Run `sm orphans` to list the active auto-rename issues."
        )));
    }
    if candidates.len() > 1 {
        return Err(CliError::Usage(format!(
            "sm orphans undo-rename: {} active auto-rename issues target \"{new_path}\".\n   The rename heuristic should produce at most one. Run `sm scan` and retry.",
            candidates.len()
        )));
    }
    let (issue_id, issue) = &candidates[0];
    let from = if issue.analyzer_id == "auto-rename-medium" {
        let Some(data_from) = data_str(issue, "from") else {
            return Err(CliError::Usage(
                "sm orphans undo-rename: auto-rename-medium issue is missing data.from.\n   Cannot revert without --from. Pass --from <old.path> explicitly.".into(),
            ));
        };
        if let Some(f) = a.from.filter(|f| *f != data_from) {
            return Err(CliError::Usage(format!(
                "sm orphans undo-rename: --from \"{f}\" does not match auto-rename-medium data.from \"{}\".",
                format::sanitize(data_from)
            )));
        }
        data_from.to_string()
    } else {
        let Some(f) = a.from else {
            return Err(CliError::NotFound(
                "sm orphans undo-rename: --from <old.path> is REQUIRED for auto-rename-ambiguous.\n   Pick one of data.candidates and pass it as --from.".into(),
            ));
        };
        let listed = issue.get("data").and_then(|d| d.get("candidates")).and_then(Value::as_array).is_some_and(|c| c.iter().any(|x| x == f));
        if !listed {
            return Err(CliError::NotFound(format!("sm orphans undo-rename: --from \"{f}\" not in auto-rename-ambiguous candidates.")));
        }
        f.to_string()
    };
    let safe_from = format::sanitize(&from);
    if !a.force && !a.dry_run {
        let q = format!("Undo auto-rename: migrate state_* FKs from {new_path} back to {safe_from}? [y/N] ");
        eprint!("{q}");
        let _ = std::io::stderr().flush();
        if !std::io::stdin().is_terminal() {
            return Ok(0); // a closed stdin never answers (the original's readline)
        }
        let mut answer = String::new();
        let _ = std::io::stdin().lock().read_line(&mut answer);
        let ans = answer.trim().to_ascii_lowercase();
        if ans != "y" && ans != "yes" {
            if !cli.global.quiet && !cli.global.json {
                eprintln!("ℹ  sm orphans undo-rename: aborted by user. Nothing moved.");
            }
            return Ok(0);
        }
    }
    let tx = conn.transaction().map_err(|e| CliError::Operational(e.to_string()))?;
    let report = store::migrate_node_fks(&tx, new_path, &from).map_err(|e| CliError::Operational(e.to_string()))?;
    let rows = report.total();
    if a.dry_run {
        drop(tx);
        println!("⋯  Would revert {new_path} → {safe_from}  (dry-run)");
        println!("   {rows} rows would migrate · would emit a new orphan issue on {safe_from}");
        return Ok(0);
    }
    let op = |e: rusqlite::Error| CliError::Operational(e.to_string());
    tx.execute("DELETE FROM scan_issues WHERE id = ?1", [issue_id]).map_err(op)?;
    let orphan = Issue::new(
        "orphan",
        "info",
        vec![from.clone()],
        format!("Orphan history: {from} (was reverted from auto-rename to {new_path})."),
    )
    .data(json!({ "path": from }));
    store::insert_issue(&tx, &orphan).map_err(op)?;
    tx.commit().map_err(op)?;
    scope.log_entry(json!({
        "op": "orphans.undo-rename", "target": new_path, "channel": "cli", "outcome": "ok", "detail": format!("to={from} rows={rows}"),
    }));
    println!("✓  Reverted {new_path} → {safe_from}");
    println!("   {rows} rows migrated · new orphan issue emitted on {safe_from}");
    Ok(0)
}
