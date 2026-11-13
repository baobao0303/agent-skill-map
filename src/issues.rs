//! `sm issues dismiss | undismiss | suppressions`: durable per-node issue
//! suppressions kept in `annotations.issueSuppressions` of the `.sm` sidecar.

use crate::bump::ensure_consent;
use crate::cli::{Cli, CliError};
use crate::model::Node;
use crate::scope::Scope;
use crate::{format, jsjson, sidecar, store};
use serde_json::{Map, Value, json};
use std::path::Path;

/// The built-in analyzer catalog (qualified ids), as `sm` reports it.
const ANALYZERS: [&str; 26] = [
    "core/ai-contradiction-analyzer",
    "core/ai-incoherence-analyzer",
    "core/ai-prose-to-rules-analyzer",
    "core/ai-redundancy-analyzer",
    "core/ai-scope-analyzer",
    "core/ai-security-analyzer",
    "core/ai-structure-analyzer",
    "core/ai-suspicion-analyzer",
    "core/ai-trigger-analyzer",
    "core/ai-vagueness-analyzer",
    "core/ai-verbosity-analyzer",
    "core/annotation-field-unknown",
    "core/annotation-orphan",
    "core/annotation-stale",
    "core/extractor-collision",
    "core/issue-counter",
    "core/link-counter",
    "core/link-kind-conflict",
    "core/link-self-loop",
    "core/name-collision",
    "core/name-mismatch",
    "core/name-reserved",
    "core/node-stability",
    "core/reference-broken",
    "core/reference-redundant",
    "core/schema-violation",
];

fn known_analyzer(id: &str) -> bool {
    ANALYZERS.iter().any(|q| *q == id || q.split_once('/').is_some_and(|(_, bare)| bare == id))
}

/// `issueSuppressionAnalyzersEquivalent`: equal, or bare vs qualified.
fn equivalent(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (a.split_once('/'), b.split_once('/')) {
        (None, Some((_, bb))) => bb == a,
        (Some((_, ab)), None) => ab == b,
        _ => false,
    }
}

fn open(db: &Path) -> Result<rusqlite::Connection, CliError> {
    if !db.exists() {
        return Err(CliError::NotFound(format!("DB not found at {}\n   Run `sm scan` first.", db.display())));
    }
    store::open_existing(db).map_err(|e| CliError::Operational(e.to_string()))
}

fn find_node(conn: &rusqlite::Connection, path: &str, hint: &str) -> Result<Node, CliError> {
    store::load(conn)
        .map_err(|e| CliError::Operational(e.to_string()))?
        .and_then(|s| s.nodes.into_iter().find(|n| n.path == path))
        .ok_or_else(|| CliError::NotFound(format!("Node {} is not in the current scan.\n   {hint}", format::sanitize(path))))
}

fn existing(root: Option<&Map<String, Value>>) -> Vec<Value> {
    root.and_then(|r| r.get("annotations"))
        .and_then(|a| a.get("issueSuppressions"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter(|e| e.is_object()).cloned().collect())
        .unwrap_or_default()
}

fn refresh_mirror(conn: &rusqlite::Connection, node: &str, sm: &Path) -> Result<(), CliError> {
    let ann = sidecar::annotations_of(sidecar::read_valid(sm).as_ref());
    store::update_node_annotations(conn, node, ann.as_ref()).map_err(|e| CliError::Operational(e.to_string()))
}

fn sidecar_rel(node: &str) -> String {
    sidecar::path_for(Path::new(node)).display().to_string()
}

pub struct Target<'a> {
    pub analyzer: &'a str,
    pub value: &'a str,
    pub node: &'a str,
    pub yes: bool,
}

pub fn dismiss(cli: &Cli, scope: &Scope, db: &Path, t: Target, note: Option<&str>) -> Result<u8, CliError> {
    let conn = open(db)?;
    if !known_analyzer(t.analyzer) {
        let known: Vec<String> = ANALYZERS.iter().map(|id| format!("     {id}")).collect();
        return Err(CliError::Usage(format!(
            "Unknown analyzer \"{}\", nothing was written.\n   Valid ids (qualified or bare form accepted):\n{}\n   A suppression naming an analyzer that does not exist would sit in the committed .sm sidecar forever without ever matching an issue.",
            format::sanitize(t.analyzer),
            known.join("\n")
        )));
    }
    let node = find_node(&conn, t.node, "Run `sm scan` to index the node, then dismiss again.")?;
    if !scope.sidecar_writers_allowed() {
        return Err(crate::bump::assert_writers_allowed(scope).unwrap_err());
    }
    ensure_consent(scope, t.yes, "sm issues dismiss")?;
    let mut entry = Map::new();
    entry.insert("analyzer".into(), json!(t.analyzer));
    entry.insert("value".into(), json!(t.value));
    if let Some(n) = note.filter(|n| !n.is_empty()) {
        entry.insert("note".into(), json!(n));
    }
    let sm = sidecar::path_for(&scope.root.join(t.node));
    let parsed = sidecar::read_valid(&sm);
    let mut merged = existing(parsed.as_ref());
    let dup = merged.iter().any(|e| {
        e.get("analyzer").and_then(Value::as_str).is_some_and(|a| equivalent(a, t.analyzer))
            && e.get("value").and_then(Value::as_str) == Some(t.value)
    });
    if !dup {
        merged.push(Value::Object(entry.clone()));
    }
    let mut changes = Map::new();
    changes.insert("annotations".into(), json!({ "issueSuppressions": merged }));
    if parsed.is_none() {
        changes.insert(
            "identity".into(),
            json!({ "path": node.path, "bodyHash": node.body_hash, "frontmatterHash": node.frontmatter_hash }),
        );
    }
    sidecar::apply_patch(&sm, &changes).map_err(CliError::Operational)?;
    refresh_mirror(&conn, t.node, &sm)?;
    let deleted = store::delete_issues_for_suppression(&conn, t.node, t.analyzer, t.value)
        .map_err(|e| CliError::Operational(e.to_string()))?;
    scope.log_entry(json!({
        "op": "issues.dismiss", "target": t.node, "extension": t.analyzer, "channel": "cli", "outcome": "ok",
        "detail": format!("value={} deleted={deleted}", t.value),
    }));
    if cli.global.json {
        println!(
            "{}",
            jsjson::to_string(&json!({
                "ok": true, "kind": "issue-suppression", "suppression": entry, "node": t.node, "deletedIssues": deleted,
            }))
        );
    } else {
        println!(
            "✓  Dismissed: {} \"{}\" on {} (recorded in {}; {deleted} stored issue row{} deleted).\n   The analyzer skips this exact value on every future scan; `sm issues undismiss` lifts it (the issue returns at the next scan).",
            format::sanitize(t.analyzer),
            format::sanitize(t.value),
            format::sanitize(t.node),
            format::sanitize(&sidecar_rel(t.node)),
            if deleted == 1 { "" } else { "s" }
        );
    }
    Ok(0)
}

pub fn undismiss(cli: &Cli, scope: &Scope, db: &Path, t: Target) -> Result<u8, CliError> {
    let conn = open(db)?;
    find_node(&conn, t.node, "Run `sm scan` to re-index the node, then undismiss again.")?;
    let sm = sidecar::path_for(&scope.root.join(t.node));
    let mut remaining = Vec::new();
    let mut removed: Option<Value> = None;
    for e in existing(sidecar::read_valid(&sm).as_ref()) {
        let hit = e.get("analyzer").and_then(Value::as_str).is_some_and(|a| equivalent(a, t.analyzer))
            && e.get("value").and_then(Value::as_str) == Some(t.value);
        if hit && removed.is_none() {
            removed = Some(e);
        } else {
            remaining.push(e);
        }
    }
    let Some(removed) = removed else {
        refresh_mirror(&conn, t.node, &sm)?;
        return Err(CliError::NotFound(format!(
            "No issue suppression for {} \"{}\" on {}.\n   Run `sm issues suppressions -n <path>` to list the active entries (matching is exact and case-sensitive on the value).",
            format::sanitize(t.analyzer),
            format::sanitize(t.value),
            format::sanitize(t.node)
        )));
    };
    if !scope.sidecar_writers_allowed() {
        return Err(crate::bump::assert_writers_allowed(scope).unwrap_err());
    }
    ensure_consent(scope, t.yes, "sm issues undismiss")?;
    let mut changes = Map::new();
    changes.insert("annotations".into(), json!({ "issueSuppressions": remaining }));
    sidecar::apply_patch(&sm, &changes).map_err(CliError::Operational)?;
    refresh_mirror(&conn, t.node, &sm)?;
    scope.log_entry(json!({
        "op": "issues.undismiss", "target": t.node, "extension": t.analyzer, "channel": "cli", "outcome": "ok",
        "detail": format!("value={}", t.value),
    }));
    if cli.global.json {
        println!("{}", jsjson::to_string(&json!({ "ok": true, "kind": "issue-unsuppression", "removed": removed, "node": t.node })));
    } else {
        let s = |k: &str| removed.get(k).map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string)).unwrap_or_default();
        println!(
            "✓  Un-dismissed: {} \"{}\" on {} (removed from {}).\n   The issue reappears at the NEXT scan (issue suppressions act at emission time); run `sm scan` to re-check the node.",
            format::sanitize(&s("analyzer")),
            format::sanitize(&s("value")),
            format::sanitize(t.node),
            format::sanitize(&sidecar_rel(t.node))
        );
    }
    Ok(0)
}

pub fn suppressions(cli: &Cli, db: &Path, node: Option<&str>) -> Result<u8, CliError> {
    let conn = open(db)?;
    let nodes = store::load(&conn).map_err(|e| CliError::Operational(e.to_string()))?.map(|s| s.nodes).unwrap_or_default();
    let nonempty = |v: Option<&Value>| v.and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    let mut rows: Vec<(String, String, String, Option<String>)> = Vec::new();
    for n in nodes.iter().filter(|n| node.is_none_or(|p| n.path == p)) {
        let list = n.sidecar.as_ref().and_then(|s| s.get("annotations")).and_then(|a| a.get("issueSuppressions")).and_then(Value::as_array);
        for e in list.into_iter().flatten() {
            if let (Some(a), Some(v)) = (nonempty(e.get("analyzer")), nonempty(e.get("value"))) {
                rows.push((n.path.clone(), a, v, nonempty(e.get("note"))));
            }
        }
    }
    rows.sort_by(|x, y| {
        format::locale_compare(&x.0, &y.0).then_with(|| format::locale_compare(&x.1, &y.1)).then_with(|| format::locale_compare(&x.2, &y.2))
    });
    if cli.global.json {
        let list: Vec<Value> = rows
            .iter()
            .map(|(n, a, v, note)| {
                let mut o = json!({ "node": n, "analyzer": a, "value": v });
                if let Some(note) = note {
                    o["note"] = json!(note);
                }
                o
            })
            .collect();
        println!("{}", jsjson::to_string(&json!({ "ok": true, "kind": "issue-suppressions", "suppressions": list })));
        return Ok(0);
    }
    if rows.is_empty() {
        println!("✓  No active issue suppressions.");
        return Ok(0);
    }
    let mut out = format!("sm issues suppressions: {} active\n\n", rows.len());
    for (n, a, v, note) in &rows {
        let suffix = note.as_ref().map(|t| format!("  {}", format::sanitize(t))).unwrap_or_default();
        out += &format!("  {}  {}  \"{}\"{suffix}\n", format::sanitize(n), format::sanitize(a), format::sanitize(v));
    }
    out += "\nTip: `sm issues undismiss <analyzer> <value> -n <path>` removes one; the issue returns at the next scan.\n";
    print!("{out}");
    Ok(0)
}
