//! SQLite persistence of the `scan_*` zone using skill-map's own DDL
//! (`schema/001_initial.sql`, verbatim) so the original UI/BFF can read it.

use crate::model::{
    ExternalRef, Issue, Link, Location, Node, OccLocation, Occurrence, OversizedFile, ScanResult, ScannedBy,
    Stats, Trigger, Triple,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

const DDL: &str = include_str!("schema/001_initial.sql");

/// sha256 over `version \0 description \0 byteLength \0` + sql, per migration.
pub fn schema_fingerprint() -> String {
    let mut h = Sha256::new();
    h.update(format!("1\0initial\0{}\0", DDL.len()).as_bytes());
    h.update(DDL.as_bytes());
    hex::encode(h.finalize())
}

/// Open (creating and migrating if needed). `backups_dir` receives a copy of
/// the DB file taken right before a migration runs.
pub fn open(db_path: &Path, backups_dir: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(db_path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version == 0 {
        let _ = std::fs::create_dir_all(backups_dir);
        let _ = std::fs::copy(db_path, backups_dir.join("skill-map-pre-migrate-v1.db"));
        conn.execute_batch(DDL)?;
        conn.execute(
            "INSERT INTO config_schema_versions (scope, owner_id, version, description, applied_at) VALUES ('kernel','kernel',1,'initial',?1)",
            params![now_ms()],
        )?;
        conn.pragma_update(None, "user_version", 1)?;
    }
    Ok(conn)
}

pub fn open_existing(db_path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn json<T: serde::Serialize + ?Sized>(v: &T) -> String {
    crate::jsjson::to_string(v)
}

fn opt_json<T: serde::Serialize>(v: &Option<T>) -> Option<String> {
    v.as_ref().map(|x| json(x))
}

/// Re-point every `state_*` reference of a renamed node (`migrateNodeFks`).
/// On a primary-key collision at the new path the existing row wins and the
/// migrating one is dropped.
fn migrate_node_fks(tx: &rusqlite::Transaction, from: &str, to: &str) -> rusqlite::Result<()> {
    if from == to {
        return Ok(());
    }
    tx.execute("UPDATE state_jobs SET node_id = ?2 WHERE node_id = ?1", params![from, to])?;
    let rows: Vec<(i64, String)> = {
        let mut st = tx.prepare(
            "SELECT id, node_ids_json FROM state_executions
             WHERE EXISTS (SELECT 1 FROM json_each(state_executions.node_ids_json) je WHERE je.value = ?1)",
        )?;
        st.query_map([from], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?
    };
    for (id, ids) in rows {
        let ids: Vec<String> = serde_json::from_str(&ids).unwrap_or_default();
        let ids: Vec<&str> = ids.iter().map(|p| if p == from { to } else { p.as_str() }).collect();
        tx.execute("UPDATE state_executions SET node_ids_json = ?2 WHERE id = ?1", params![id, serde_json::to_string(&ids).unwrap()])?;
    }
    tx.execute("UPDATE state_findings SET node_id = ?2 WHERE node_id = ?1", params![from, to])?;
    for (table, col) in [("state_summaries", "node_id"), ("state_enrichments", "node_id")] {
        tx.execute(&format!("UPDATE OR IGNORE {table} SET {col} = ?2 WHERE {col} = ?1"), params![from, to])?;
        tx.execute(&format!("DELETE FROM {table} WHERE {col} = ?1"), [from])?;
    }
    if !from.is_empty() {
        tx.execute("UPDATE OR IGNORE state_plugin_kvs SET node_id = ?2 WHERE node_id = ?1", params![from, to])?;
        tx.execute("DELETE FROM state_plugin_kvs WHERE node_id = ?1", [from])?;
    }
    for table in ["state_node_favorites", "state_activity_stats"] {
        tx.execute(&format!("UPDATE OR IGNORE {table} SET node_path = ?2 WHERE node_path = ?1"), params![from, to])?;
        tx.execute(&format!("DELETE FROM {table} WHERE node_path = ?1"), [from])?;
    }
    tx.execute("UPDATE OR IGNORE state_activity_pairs SET parent = ?2 WHERE parent = ?1", params![from, to])?;
    tx.execute("DELETE FROM state_activity_pairs WHERE parent = ?1", [from])?;
    tx.execute("UPDATE OR IGNORE state_activity_pairs SET child_node_path = ?2 WHERE child_node_path = ?1", params![from, to])?;
    tx.execute("DELETE FROM state_activity_pairs WHERE child_node_path = ?1", [from])?;
    tx.execute("UPDATE node_enrichments SET node_path = ?2 WHERE node_path = ?1", params![from, to])?;
    Ok(())
}

/// `state_*` rows whose node left the scan without a rename match surface
/// as `orphan` info issues (unless the rename pass already reported them).
fn append_stranded_orphans(tx: &rusqlite::Transaction, scan: &mut ScanResult) -> rusqlite::Result<()> {
    let live: std::collections::HashSet<&str> = scan.nodes.iter().map(|n| n.path.as_str()).collect();
    let known: std::collections::HashSet<String> = scan
        .issues
        .iter()
        .filter(|i| i.analyzer_id == "orphan")
        .filter_map(|i| i.get("data").and_then(|d| d.get("path")).and_then(Value::as_str).map(str::to_string))
        .collect();
    let mut st = tx.prepare(
        "SELECT node_id AS value FROM state_jobs
         UNION SELECT je.value FROM state_executions, json_each(state_executions.node_ids_json) je
         UNION SELECT node_id FROM state_summaries
         UNION SELECT node_id FROM state_findings
         UNION SELECT node_id FROM state_enrichments
         UNION SELECT node_id FROM state_plugin_kvs WHERE node_id != ''
         UNION SELECT node_path FROM state_node_favorites",
    )?;
    let mut stranded: Vec<String> = st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|p| !live.contains(p.as_str()))
        .collect();
    drop(st);
    stranded.sort();
    stranded.dedup();
    for path in stranded {
        if known.contains(&path) || path.starts_with("sm://") {
            continue;
        }
        scan.issues.push(
            crate::model::Issue::new(
                "orphan",
                "info",
                vec![path.clone()],
                format!("Orphan history: {path} has stranded state_* references but no live node."),
            )
            .data(serde_json::json!({ "path": path })),
        );
    }
    scan.stats.issues_count = scan.issues.len() as u64;
    Ok(())
}

/// The persisted `scan_extractor_runs`, keyed for the cache decision.
pub fn load_extractor_runs(conn: &Connection) -> rusqlite::Result<crate::scan::PriorRuns> {
    let mut out = crate::scan::PriorRuns::new();
    let mut st = conn.prepare(
        "SELECT node_path, extractor_id, body_hash_at_run, sidecar_annotations_hash_at_run, settings_hash_at_run FROM scan_extractor_runs",
    )?;
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, (r.get(2)?, r.get(3)?, r.get(4)?))))?;
    for row in rows {
        let (path, id, key) = row?;
        out.entry(path).or_default().insert(id, key);
    }
    Ok(out)
}

/// Write-through mirror of a node's sidecar annotations.
pub fn update_node_annotations(conn: &Connection, path: &str, annotations: Option<&Value>) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE scan_nodes SET annotations_json = ?1 WHERE path = ?2",
        params![annotations.map(crate::jsjson::to_string), path],
    )?;
    Ok(())
}

/// Delete the persisted issues an issue suppression silences: same analyzer
/// (qualified filter or bare), `data.target == value`, flagged on `node`.
pub fn delete_issues_for_suppression(conn: &Connection, node: &str, analyzer: &str, value: &str) -> rusqlite::Result<usize> {
    let mut st = conn.prepare("SELECT id, analyzer_id, node_ids_json, data_json FROM scan_issues ORDER BY id")?;
    let rows: Vec<(i64, String, String, Option<String>)> =
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<_, _>>()?;
    let bare = analyzer.split_once('/').map_or(analyzer, |(_, b)| b);
    let mut deleted = 0;
    for (id, aid, ids, data) in rows {
        if aid != analyzer && aid != bare {
            continue;
        }
        let data: Value = data.and_then(|d| serde_json::from_str(&d).ok()).unwrap_or(Value::Null);
        if data.get("target").and_then(Value::as_str) != Some(value) {
            continue;
        }
        let ids: Vec<String> = serde_json::from_str(&ids).unwrap_or_default();
        if !ids.iter().any(|i| i == node) {
            continue;
        }
        conn.execute("DELETE FROM scan_issues WHERE id = ?1", [id])?;
        deleted += 1;
    }
    Ok(deleted)
}

/// Rows currently in the scan zone (the zero-result wipe guard).
pub fn count_scan_rows(conn: &Connection) -> rusqlite::Result<u64> {
    let mut total = 0u64;
    for t in ["scan_nodes", "scan_links", "scan_issues"] {
        total += conn.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get::<_, i64>(0))? as u64;
    }
    Ok(total)
}

/// Replace-all persist of one scan in a single transaction.
pub fn persist(
    conn: &mut Connection,
    scan: &mut ScanResult,
    renames: &[crate::scan::Rename],
    runs: &[crate::scan::ExtractorRun],
) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    for r in renames {
        migrate_node_fks(&tx, &r.from, &r.to)?;
    }
    append_stranded_orphans(&tx, scan)?;
    tx.execute("DELETE FROM scan_extractor_runs", [])?;
    {
        let mut st = tx.prepare(
            "INSERT OR REPLACE INTO scan_extractor_runs (node_path, extractor_id, body_hash_at_run, sidecar_annotations_hash_at_run,
             settings_hash_at_run, ran_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for r in runs {
            st.execute(params![r.node_path, r.extractor_id, r.body_hash, r.sidecar_annotations_hash, r.settings_hash, r.ran_at])?;
        }
    }
    for t in ["scan_nodes", "scan_links", "scan_issues", "scan_meta", "scan_node_tags", "scan_link_scores"] {
        tx.execute(&format!("DELETE FROM {t}"), [])?;
    }
    {
        let mut ins = tx.prepare(
            "INSERT INTO scan_nodes (path, kind, provider, title, description, stability, version, frontmatter_json,
             body_hash, frontmatter_hash, bytes_frontmatter, bytes_body, bytes_total, tokens_frontmatter, tokens_body,
             tokens_total, links_out_count, links_in_count, external_refs_count, external_refs_json, scanned_at,
             modified_at_ms, virtual, derived_from_json, sidecar_present, sidecar_status, annotations_json, sidecar_root_json)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28)",
        )?;
        for n in &scan.nodes {
            let fm = n.frontmatter.clone().unwrap_or(Value::Object(Default::default()));
            let fm_str = |k: &str| fm.get(k).and_then(Value::as_str).map(str::to_string);
            let sc = n.sidecar.as_ref();
            let ann = sc.and_then(|s| s.get("annotations")).filter(|a| !a.is_null());
            ins.execute(params![
                n.path,
                n.kind,
                n.provider,
                fm_str("name"),
                fm_str("description"),
                ann.and_then(|a| a.get("stability")).and_then(Value::as_str),
                ann.and_then(|a| a.get("version")).and_then(Value::as_i64),
                json(&fm),
                n.body_hash,
                n.frontmatter_hash,
                n.bytes.frontmatter,
                n.bytes.body,
                n.bytes.total,
                n.tokens.map(|t| t.frontmatter),
                n.tokens.map(|t| t.body),
                n.tokens.map(|t| t.total),
                n.links_out_count,
                n.links_in_count,
                n.external_refs_count,
                opt_json(&n.external_refs),
                scan.scanned_at,
                n.modified_at_ms,
                n.r#virtual.unwrap_or(false),
                opt_json(&n.derived_from),
                sc.and_then(|s| s.get("present")).and_then(Value::as_bool).unwrap_or(false),
                sc.and_then(|s| s.get("status")).and_then(Value::as_str),
                ann.map(json),
                sc.and_then(|s| s.get("root")).filter(|r| !r.is_null()).map(json),
            ])?;
            if let Some(tags) = ann.and_then(|a| a.get("tags")).and_then(Value::as_array) {
                for tag in tags.iter().filter_map(Value::as_str) {
                    tx.execute("INSERT OR IGNORE INTO scan_node_tags (node_path, tag) VALUES (?1, ?2)", params![n.path, tag])?;
                }
            }
        }
    }
    {
        let mut ins = tx.prepare(
            "INSERT INTO scan_links (source_path, target_path, kind, confidence, sources_json, original_trigger,
             normalized_trigger, location_line, location_offset, occurrences_json, resolved_target, raw)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        )?;
        for l in &scan.links {
            ins.execute(params![
                l.source,
                l.target,
                l.kind,
                l.confidence,
                json(&l.sources),
                l.trigger.as_ref().map(|t| &t.original_trigger),
                l.trigger.as_ref().map(|t| &t.normalized_trigger),
                l.location.as_ref().map(|x| x.line),
                l.location.as_ref().map(|x| x.offset),
                json(&l.occurrences),
                l.resolved_target,
                l.raw,
            ])?;
        }
    }
    {
        let mut ins = tx.prepare(
            "INSERT INTO scan_issues (analyzer_id, severity, node_ids_json, message, detail, fix_json, data_json)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
        )?;
        for i in &scan.issues {
            // fix_json keeps the tail's key order so a read-back rebuilds it.
            ins.execute(params![
                i.analyzer_id,
                i.severity,
                json(&i.node_ids),
                i.message,
                i.get("detail").and_then(Value::as_str),
                i.get("fix").map(|f| json(f)),
                i.get("data").map(|d| json(d)),
            ])?;
        }
    }
    tx.execute(
        "INSERT INTO scan_meta (id, roots_json, scanned_at, scanned_by_name, scanned_by_version, scanned_by_spec_version,
         providers_json, stats_files_walked, stats_files_skipped, stats_duration_ms, scan_ceiling, scan_truncated,
         max_render_nodes, files_oversized, oversized_files_json, tokenizer, active_provider, schema_fingerprint)
         VALUES (1,?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
        params![
            json(&scan.roots),
            scan.scanned_at,
            scan.scanned_by.name,
            scan.scanned_by.version,
            scan.scanned_by.spec_version,
            json(&scan.providers),
            scan.stats.files_walked,
            scan.stats.files_skipped,
            scan.stats.duration_ms,
            scan.scan_ceiling,
            scan.scan_truncated,
            scan.max_render_nodes,
            scan.stats.files_oversized,
            json(&scan.oversized_files),
            scan.tokenizer,
            scan.active_provider,
            schema_fingerprint(),
        ],
    )?;
    tx.commit()
}

fn parse<T: serde::de::DeserializeOwned>(s: Option<String>) -> Option<T> {
    s.and_then(|s| serde_json::from_str(&s).ok())
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// Read the persisted scan back, rows in insertion order.
pub fn load(conn: &Connection) -> rusqlite::Result<Option<ScanResult>> {
    let meta = conn
        .query_row(
            "SELECT roots_json, scanned_at, scanned_by_version, providers_json, stats_files_walked, stats_files_skipped,
             stats_duration_ms, scan_ceiling, scan_truncated, max_render_nodes, oversized_files_json, active_provider
             FROM scan_meta WHERE id = 1",
            [],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, bool>(8)?,
                    r.get::<_, i64>(9)?,
                    r.get::<_, Option<String>>(10)?,
                    r.get::<_, Option<String>>(11)?,
                ))
            },
        )
        .optional()?;
    let Some((roots, scanned_at, providers, walked, skipped, duration, ceiling, truncated, max_nodes, oversized, active)) = meta
    else {
        return Ok(None);
    };

    let mut st = conn.prepare("SELECT * FROM scan_nodes ORDER BY rowid")?;
    let nodes: Vec<Node> = st
        .query_map([], |r| {
            let virtual_: bool = r.get("virtual")?;
            let tokens_total: Option<u64> = r.get("tokens_total")?;
            // After a DB round-trip every node carries the full overlay shape.
            let present: bool = r.get("sidecar_present")?;
            let sidecar = Some(serde_json::json!({
                "present": present,
                "status": r.get::<_, Option<String>>("sidecar_status")?,
                "annotations": parse::<Value>(r.get("annotations_json")?).unwrap_or(Value::Null),
                "root": parse::<Value>(r.get("sidecar_root_json")?).unwrap_or(Value::Null),
            }));
            Ok(Node {
                path: r.get("path")?,
                kind: r.get("kind")?,
                provider: r.get("provider")?,
                body_hash: r.get("body_hash")?,
                frontmatter_hash: r.get("frontmatter_hash")?,
                bytes: Triple { frontmatter: r.get("bytes_frontmatter")?, body: r.get("bytes_body")?, total: r.get("bytes_total")? },
                links_out_count: r.get("links_out_count")?,
                links_in_count: r.get("links_in_count")?,
                external_refs_count: r.get("external_refs_count")?,
                r#virtual: virtual_.then_some(true),
                derived_from: parse(r.get("derived_from_json")?),
                frontmatter: parse(r.get("frontmatter_json")?),
                modified_at_ms: r.get("modified_at_ms")?,
                tokens: tokens_total.map(|total| -> rusqlite::Result<Triple> {
                    Ok(Triple { frontmatter: r.get("tokens_frontmatter")?, body: r.get("tokens_body")?, total })
                }).transpose()?,
                sidecar,
                external_refs: parse::<Vec<Value>>(r.get("external_refs_json")?).map(|v| {
                    v.into_iter()
                        .map(|e| ExternalRef {
                            url: e["url"].as_str().unwrap_or("").into(),
                            line: e["line"].as_u64().unwrap_or(0),
                            original_trigger: e["originalTrigger"].as_str().unwrap_or("").into(),
                        })
                        .collect()
                }),
                loaded: true,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut st = conn.prepare("SELECT * FROM scan_links ORDER BY id")?;
    let links: Vec<Link> = st
        .query_map([], |r| {
            let original: Option<String> = r.get("original_trigger")?;
            let normalized: Option<String> = r.get("normalized_trigger")?;
            let line: Option<u64> = r.get("location_line")?;
            let occurrences: Vec<Value> = parse(r.get("occurrences_json")?).unwrap_or_default();
            Ok(Link {
                source: r.get("source_path")?,
                target: r.get("target_path")?,
                kind: leak(r.get("kind")?),
                confidence: r.get("confidence")?,
                sources: parse(r.get("sources_json")?).unwrap_or_default(),
                raw: r.get::<_, Option<String>>("raw")?.unwrap_or_default(),
                trigger: original.zip(normalized).map(|(o, n)| Trigger { original_trigger: o, normalized_trigger: n }),
                location: line.map(|line| -> rusqlite::Result<Location> {
                    Ok(Location { line, offset: r.get::<_, Option<u64>>("location_offset")?.unwrap_or(0) })
                }).transpose()?,
                occurrences: occurrences
                    .into_iter()
                    .map(|o| Occurrence {
                        extractor: o["extractor"].as_str().unwrap_or("").into(),
                        original_trigger: o["originalTrigger"].as_str().unwrap_or("").into(),
                        location: o["location"]["line"].as_u64().map(|line| OccLocation { line }),
                        context: o["context"].as_str().map(|c| leak(c.to_string())),
                    })
                    .collect(),
                resolved_target: r.get("resolved_target")?,
                loaded: Some(r.get::<_, Option<String>>("resolved_target")?.is_some()),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut st = conn.prepare("SELECT analyzer_id, severity, node_ids_json, message, fix_json, data_json, detail FROM scan_issues ORDER BY id")?;
    let issues: Vec<Issue> = st
        .query_map([], |r| {
            let mut issue = Issue::new(&r.get::<_, String>(0)?, leak(r.get(1)?), parse(r.get(2)?).unwrap_or_default(), r.get(3)?);
            if let Some(d) = r.get::<_, Option<String>>(6)? {
                issue = issue.detail(d);
            }
            if let Some(f) = parse::<Value>(r.get(4)?) {
                issue.tail.insert("fix".into(), f);
            }
            if let Some(d) = parse::<Value>(r.get(5)?) {
                issue = issue.data(d);
            }
            Ok(issue)
        })?
        .collect::<rusqlite::Result<_>>()?;

    let oversized_files: Vec<OversizedFile> = parse::<Vec<Value>>(oversized)
        .unwrap_or_default()
        .into_iter()
        .map(|o| OversizedFile { path: o["path"].as_str().unwrap_or("").into(), bytes: o["bytes"].as_u64().unwrap_or(0) })
        .collect();
    Ok(Some(ScanResult {
        schema_version: 1,
        scanned_at,
        roots: parse(Some(roots)).unwrap_or_default(),
        providers: parse::<Vec<String>>(Some(providers)).unwrap_or_default().into_iter().map(leak).collect(),
        scanned_by: ScannedBy { name: "agent-skill-map", version: env!("CARGO_PKG_VERSION"), spec_version: crate::scan::SPEC_VERSION },
        tokenizer: "cl100k_base",
        active_provider: active,
        scan_ceiling: ceiling as u64,
        scan_truncated: truncated,
        max_render_nodes: max_nodes as u64,
        stats: Stats {
            files_walked: walked as u64,
            files_skipped: skipped as u64,
            files_oversized: oversized_files.len() as u64,
            nodes_count: nodes.len() as u64,
            links_count: links.len() as u64,
            issues_count: issues.len() as u64,
            duration_ms: duration as u64,
        },
        oversized_files,
        nodes,
        links,
        issues,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_matches_original() {
        assert_eq!(schema_fingerprint(), "2340cc3e65877681e089b855ee3a8bf02ee1adb62ee3285a87d5fa201a8cae4d");
    }
}

/// JSON of a scan read back from the DB, with the key order the original's
/// row mappers produce (it differs from a fresh scan's order).
pub fn loaded_json(scan: &ScanResult) -> Value {
    use serde_json::{Map, to_value};
    fn v<T: serde::Serialize>(x: &T) -> Value {
        serde_json::to_value(x).unwrap_or_default()
    }
    let mut root = Map::new();
    root.insert("schemaVersion".into(), v(&scan.schema_version));
    root.insert("scannedAt".into(), v(&scan.scanned_at));
    root.insert("roots".into(), v(&scan.roots));
    root.insert("providers".into(), v(&scan.providers));
    root.insert("scannedBy".into(), v(&scan.scanned_by));
    root.insert("tokenizer".into(), v(&scan.tokenizer));
    root.insert("activeProvider".into(), v(&scan.active_provider));
    root.insert("scanCeiling".into(), v(&scan.scan_ceiling));
    root.insert("scanTruncated".into(), v(&scan.scan_truncated));
    root.insert("maxRenderNodes".into(), v(&scan.max_render_nodes));
    root.insert("oversizedFiles".into(), v(&scan.oversized_files));
    let nodes: Vec<Value> = scan
        .nodes
        .iter()
        .map(|n| {
            let full = to_value(n).unwrap_or_default();
            let mut o = Map::new();
            for k in [
                "path", "kind", "provider", "bodyHash", "frontmatterHash", "bytes", "linksOutCount", "linksInCount",
                "externalRefsCount", "frontmatter", "sidecar", "tokens", "externalRefs", "modifiedAtMs", "virtual", "derivedFrom",
            ] {
                if let Some(x) = full.get(k) {
                    o.insert(k.into(), x.clone());
                }
            }
            Value::Object(o)
        })
        .collect();
    root.insert("nodes".into(), Value::Array(nodes));
    let links: Vec<Value> = scan
        .links
        .iter()
        .map(|l| {
            let full = to_value(l).unwrap_or_default();
            let mut o = Map::new();
            for k in ["source", "target", "kind", "confidence", "sources", "trigger", "location", "occurrences", "resolvedTarget", "raw"] {
                if let Some(x) = full.get(k) {
                    o.insert(k.into(), x.clone());
                }
            }
            Value::Object(o)
        })
        .collect();
    root.insert("links".into(), Value::Array(links));
    root.insert("issues".into(), v(&scan.issues));
    root.insert("stats".into(), v(&scan.stats));
    Value::Object(root)
}

