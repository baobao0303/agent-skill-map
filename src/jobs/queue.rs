//! The job queue (`sm jobs submit | list | show | preview | claim | status |
//! cancel | fail`), per `job-lifecycle.md`: probabilistic extensions are
//! rendered into self-contained job content and handed to external agents
//! through an atomic claim; skill-map never runs them itself.

use crate::catalog::{self, Ext, Target};
use crate::cli::{Cli, CliError};
use crate::model::Node;
use crate::scope::Scope;
use crate::{format, jsjson, scan, store};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};
use std::path::Path;

const PREAMBLE: &str = include_str!("../schema/spec/preamble-v2.txt");
const REPORT_BASE: &str = include_str!("../schema/spec/report-base.schema.json");
const SUMMARY_MARKDOWN: &str = include_str!("../schema/spec/summaries/markdown.schema.json");
const FINDINGS_REPORT: &str = include_str!("../schema/spec/findings/report.schema.json");
const NUL: &str = "\0";

fn op(e: impl std::fmt::Display) -> CliError {
    CliError::Operational(e.to_string())
}

pub fn now_ms() -> i64 {
    scan::now_ms()
}

fn sha256(s: &str) -> String {
    scan::sha256_pub(s)
}

pub fn open(db: &Path) -> Result<Connection, CliError> {
    if !db.exists() {
        return Err(CliError::NotFound(format!("DB not found at {}\n   Run `sm scan` first.", db.display())));
    }
    store::open_existing(db).map_err(op)
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("OS randomness");
    hex::encode(buf)
}

/// `YYYYMMDD-HHMMSS` (UTC), the time part of job / execution / run ids.
pub fn id_timestamp() -> String {
    let t = time::OffsetDateTime::now_utc();
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", t.year(), t.month() as u8, t.day(), t.hour(), t.minute(), t.second())
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}-{}", id_timestamp(), random_hex(2))
}

// ---------------------------------------------------------------- rendering

fn fence_json(text: &str) -> String {
    let body = if text.ends_with('\n') { text.to_string() } else { format!("{text}\n") };
    format!("```json\n{body}```")
}

/// Does `value` hold a `$ref` starting with `prefix` anywhere?
fn has_ref(value: &Value, prefix: &str) -> bool {
    match value {
        Value::Array(a) => a.iter().any(|v| has_ref(v, prefix)),
        Value::Object(o) => {
            o.get("$ref").and_then(Value::as_str).is_some_and(|r| r.starts_with(prefix)) || o.values().any(|v| has_ref(v, prefix))
        }
        _ => false,
    }
}

pub fn is_findings_schema(s: &Value) -> bool {
    has_ref(s, "https://skill-map.ai/spec/v1/findings/report.schema.json")
}
pub fn is_summary_schema(s: &Value) -> bool {
    has_ref(s, "https://skill-map.ai/spec/v1/summaries/markdown.schema.json")
}
pub fn is_tags_schema(s: &Value) -> bool {
    has_ref(s, "https://skill-map.ai/spec/v1/tags/")
}

/// `buildReportContract`: the extension's schema, its namespace envelope,
/// then `report-base`, each verbatim in a fenced json block.
fn report_contract(text: &str, schema: &Value) -> String {
    let mut blocks = vec![fence_json(text)];
    if is_findings_schema(schema) {
        blocks.push(fence_json(FINDINGS_REPORT));
    } else if is_summary_schema(schema) {
        blocks.push(fence_json(SUMMARY_MARKDOWN));
    }
    blocks.push(fence_json(REPORT_BASE));
    format!(
        "## Report contract\n\nYour JSON report MUST validate against the first schema below. The blocks after it are the canonical schemas it references via `$ref` (the URLs are identifiers, never fetched).\n\n{}",
        blocks.join("\n\n")
    )
}

fn escape_attr(v: &str) -> String {
    v.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

/// `</user-content>` inside the body is defused with a zero-width space.
fn escape_close(body: &str) -> String {
    let re = fancy_regex::Regex::new(r"(?i)(</\s*user-content\s*)>").unwrap();
    re.replace_all(body, "$1&#x200B;>").into_owned()
}

pub fn unescape_close(content: &str) -> String {
    let re = fancy_regex::Regex::new(r"(?i)(</\s*user-content\s*)&#x200B;>").unwrap();
    re.replace_all(content, "$1>").into_owned()
}

fn render(template: &str, sections: &[&str], node: Option<(&str, &str)>) -> String {
    let mut parts: Vec<String> = sections.iter().map(|s| s.to_string()).collect();
    if let Some((path, body)) = node {
        parts.push(format!("<user-content id=\"{}\">\n{}\n</user-content>", escape_attr(path), escape_close(body)));
    }
    format!("{PREAMBLE}\n{}", template.split("{{userContent}}").collect::<Vec<_>>().join(&parts.join("\n\n")))
}

fn current_tags_section(tags: &[String]) -> String {
    format!(
        "## Current tags\n\nThe document below already carries these tags. They are READ-ONLY context, not a task: reuse one verbatim when it fits, propose only what is genuinely missing, and do NOT re-emit these existing tags in your report.\n\n```json\n{}\n```",
        jsjson::to_string_pretty(tags)
    )
}

/// Tags a node already carries (sidecar annotations, then frontmatter).
fn node_tags(node: &Node) -> Vec<String> {
    let from = |v: Option<&Value>| -> Vec<String> {
        v.and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
    };
    let mut tags = from(node.sidecar.as_ref().and_then(|s| s.get("annotations")).and_then(|a| a.get("tags")));
    for t in from(node.frontmatter.as_ref().and_then(|f| f.get("tags"))) {
        if !tags.contains(&t) {
            tags.push(t);
        }
    }
    tags
}

// ---------------------------------------------------------------- rows

const JOB_COLS: &str = "id, extension_id, extension_version, extension_kind, auto_fix, finding_ids_json, node_id, content_hash, nonce, \
     priority, status, failure_reason, runner, ttl_seconds, created_at, claimed_at, finished_at, expires_at, submitted_by";

/// `rowToJob` (key order is the wire order).
fn row_to_job(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let ids: Option<String> = r.get(5)?;
    let finding_ids = ids
        .and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok())
        .map(|v| Value::Array(v.into_iter().filter(|x| x.is_i64() || x.is_u64()).collect()))
        .unwrap_or(Value::Null);
    let mut m = Map::new();
    m.insert("id".into(), json!(r.get::<_, String>(0)?));
    m.insert("extensionId".into(), json!(r.get::<_, String>(1)?));
    m.insert("extensionVersion".into(), json!(r.get::<_, String>(2)?));
    m.insert("extensionKind".into(), json!(r.get::<_, String>(3)?));
    m.insert("autoFix".into(), json!(r.get::<_, i64>(4)? != 0));
    m.insert("findingIds".into(), finding_ids);
    m.insert("nodeId".into(), json!(r.get::<_, String>(6)?));
    m.insert("contentHash".into(), json!(r.get::<_, String>(7)?));
    m.insert("nonce".into(), json!(r.get::<_, String>(8)?));
    m.insert("priority".into(), json!(r.get::<_, i64>(9)?));
    m.insert("status".into(), json!(r.get::<_, String>(10)?));
    m.insert("failureReason".into(), json!(r.get::<_, Option<String>>(11)?));
    m.insert("runner".into(), json!(r.get::<_, Option<String>>(12)?));
    m.insert("ttlSeconds".into(), json!(r.get::<_, Option<i64>>(13)?));
    m.insert("createdAt".into(), json!(r.get::<_, i64>(14)?));
    m.insert("claimedAt".into(), json!(r.get::<_, Option<i64>>(15)?));
    m.insert("finishedAt".into(), json!(r.get::<_, Option<i64>>(16)?));
    m.insert("expiresAt".into(), json!(r.get::<_, Option<i64>>(17)?));
    m.insert("submittedBy".into(), json!(r.get::<_, Option<String>>(18)?));
    Ok(Value::Object(m))
}

pub fn get_job(conn: &Connection, id: &str) -> Result<Option<Value>, CliError> {
    conn.query_row(&format!("SELECT {JOB_COLS} FROM state_jobs WHERE id = ?1"), [id], row_to_job).optional().map_err(op)
}

fn public(mut job: Value) -> Value {
    if let Some(m) = job.as_object_mut() {
        m.remove("nonce");
    }
    job
}

fn s<'a>(job: &'a Value, k: &str) -> &'a str {
    job.get(k).and_then(Value::as_str).unwrap_or("")
}

pub fn job_content(conn: &Connection, hash: &str) -> Result<Option<String>, CliError> {
    conn.query_row("SELECT content FROM state_job_contents WHERE content_hash = ?1", [hash], |r| r.get(0)).optional().map_err(op)
}

/// `reapExpired`: running jobs past their TTL fail as `abandoned`.
pub fn reap(conn: &Connection, now: i64) -> Result<Vec<String>, CliError> {
    let mut st = conn
        .prepare(
            "UPDATE state_jobs SET status = 'failed', failure_reason = 'abandoned', finished_at = ?1
             WHERE status = 'running' AND expires_at IS NOT NULL AND expires_at < ?1 RETURNING id",
        )
        .map_err(op)?;
    st.query_map([now], |r| r.get(0)).map_err(op)?.collect::<Result<_, _>>().map_err(op)
}

// ---------------------------------------------------------------- submit

pub struct SubmitArgs<'a> {
    pub extension: &'a str,
    pub node: Option<&'a str>,
    pub all: bool,
    pub force: bool,
    pub ttl: Option<&'a str>,
    pub priority: Option<&'a str>,
}

fn submit_err(m: &str) -> CliError {
    CliError::Usage(format!("sm jobs submit: {}", format::sanitize(m)))
}

fn parse_int(raw: Option<&str>) -> Result<Option<i64>, ()> {
    match raw {
        None => Ok(None),
        Some(t) => t.trim().parse::<i64>().map(Some).map_err(|_| ()),
    }
}

fn jobs_config(scope: &Scope) -> Value {
    scope.settings().get("jobs").cloned().unwrap_or(Value::Null)
}

fn per_extension(map: Option<&Value>, ext: &Ext) -> Option<i64> {
    let m = map?.as_object()?;
    m.get(&ext.qualified()).or_else(|| m.get(&ext.id)).and_then(Value::as_i64)
}

struct Prepared<'a> {
    ext: &'a Ext,
    template: String,
    contract: String,
    template_hash: String,
    ttl: Option<i64>,
    priority: i64,
    is_tagger: bool,
}

enum Outcome {
    Created(String, String),
    Duplicate(String, String),
    Drift(String),
    Unreadable(String, String),
}

fn content_hash(ext: &Ext, path: &str, body_hash: &str, fm_hash: &str, template_hash: &str) -> String {
    sha256(&[ext.qualified().as_str(), &ext.version, path, body_hash, fm_hash, template_hash].join(NUL))
}

fn active_duplicate(conn: &Connection, ext: &Ext, node: &str, hash: &str) -> Result<Option<String>, CliError> {
    conn.query_row(
        "SELECT id FROM state_jobs WHERE extension_id = ?1 AND extension_version = ?2 AND node_id = ?3 AND content_hash = ?4
         AND status IN ('queued','running') LIMIT 1",
        params![ext.qualified(), ext.version, node, hash],
        |r| r.get(0),
    )
    .optional()
    .map_err(op)
}

fn insert_job(conn: &mut Connection, p: &Prepared, node: &str, hash: &str, content: &str) -> Result<Outcome, CliError> {
    let now = now_ms();
    let id = new_id("d");
    let tx = conn.transaction().map_err(op)?;
    tx.execute(
        "INSERT INTO state_job_contents (content_hash, content, created_at) VALUES (?1, ?2, ?3) ON CONFLICT(content_hash) DO NOTHING",
        params![hash, content, now],
    )
    .map_err(op)?;
    let inserted = tx.execute(
        &format!("INSERT INTO state_jobs ({JOB_COLS}) VALUES (?1,?2,?3,?4,0,NULL,?5,?6,?7,?8,'queued',NULL,NULL,?9,?10,NULL,NULL,NULL,NULL)"),
        params![id, p.ext.qualified(), p.ext.version, p.ext.kind, node, hash, random_hex(16), p.priority, p.ttl, now],
    );
    match inserted {
        Ok(_) => {
            tx.commit().map_err(op)?;
            Ok(Outcome::Created(node.into(), id))
        }
        Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation => {
            drop(tx);
            let existing = active_duplicate(conn, p.ext, node, hash)?.unwrap_or(id);
            Ok(Outcome::Duplicate(node.into(), existing))
        }
        Err(e) => Err(op(e)),
    }
}

fn submit_one(conn: &mut Connection, scope: &Scope, p: &Prepared, node: &Node, force: bool) -> Result<Outcome, CliError> {
    let tags_section = if p.is_tagger {
        let tags = node_tags(node);
        (!tags.is_empty()).then(|| current_tags_section(&tags))
    } else {
        None
    };
    let template_hash = match &tags_section {
        None => p.template_hash.clone(),
        Some(t) => sha256(&format!("{PREAMBLE}{}{t}{}", p.template, p.contract)),
    };
    let hash = content_hash(p.ext, &node.path, &node.body_hash, &node.frontmatter_hash, &template_hash);
    if !force && let Some(existing) = active_duplicate(conn, p.ext, &node.path, &hash)? {
        return Ok(Outcome::Duplicate(node.path.clone(), existing));
    }
    let body = match scan::read_body(&scope.root, &node.path, &node.provider) {
        Ok(b) => b,
        Err(detail) => return Ok(Outcome::Unreadable(node.path.clone(), detail)),
    };
    if sha256(&body) != node.body_hash {
        return Ok(Outcome::Drift(node.path.clone()));
    }
    let mut sections: Vec<&str> = Vec::new();
    if let Some(t) = &tags_section {
        sections.push(t);
    }
    sections.push(&p.contract);
    let content = render(&p.template, &sections, Some((&node.path, &body)));
    insert_job(conn, p, &node.path, &hash, &content)
}

fn precondition_matches(node: &Node, pre: Option<&Value>) -> bool {
    let Some(pre) = pre else { return true };
    let list = |k: &str| pre.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>());
    if let Some(ps) = list("provider")
        && !ps.is_empty()
        && !ps.contains(&node.provider.as_str())
    {
        return false;
    }
    if let Some(ks) = list("kind")
        && !ks.is_empty()
        && !ks.iter().any(|k| *k == node.kind || k.split_once('/').is_some_and(|(p, kk)| p == node.provider && kk == node.kind))
    {
        return false;
    }
    if let Some(fs) = list("frontmatterMissing") {
        let fm = node.frontmatter.as_ref();
        let missing = |f: &str| fm.and_then(|m| m.get(f)).is_none_or(|v| v.is_null() || v.as_str() == Some(""));
        if !fs.is_empty() && !fs.iter().any(|f| missing(f)) {
            return false;
        }
    }
    true
}

pub fn submit(cli: &Cli, scope: &Scope, db: &Path, a: SubmitArgs) -> Result<u8, CliError> {
    let mut conn = open(db)?;
    if a.all && a.node.is_some() {
        return Err(submit_err("-n and --all are mutually exclusive"));
    }
    let ttl_flag = parse_int(a.ttl).map_err(|_| submit_err(&format!("--ttl must be an integer number of seconds, got {}", a.ttl.unwrap_or(""))))?;
    let prio_flag = parse_int(a.priority).map_err(|_| submit_err(&format!("--priority must be an integer, got {}", a.priority.unwrap_or(""))))?;
    let catalog = catalog::load(scope);
    let ext = match catalog::resolve(&catalog, a.extension) {
        Target::Found(e) => e,
        Target::NotFound => return Err(CliError::NotFound(format!("sm jobs submit: extension {} not found", a.extension))),
        Target::Deterministic(mode) => {
            return Err(submit_err(&format!(
                "extension {} is {mode}; only probabilistic extensions are queued (deterministic actions run in-process)",
                a.extension
            )));
        }
        Target::Ambiguous(x, y) => {
            return Err(submit_err(&format!(
                "extension {} matches both a probabilistic action and a probabilistic analyzer; disambiguate with action:{x} or analyzer:{y}",
                a.extension
            )));
        }
    };
    let Some(template) = ext.prompt.clone() else {
        return Err(submit_err(&format!("cannot resolve the prompt template for {} (no source directory)", a.extension)));
    };
    let (Some(text), Some(schema)) = (&ext.report_text, &ext.report_schema) else {
        return Err(submit_err(&format!("cannot resolve the report schema for {} (no source directory)", a.extension)));
    };
    let contract = report_contract(text, schema);
    let jobs_cfg = jobs_config(scope);
    let ttl = match ttl_flag {
        Some(t) if t < 0 => {
            return Err(submit_err(&format!(
                "invalid --ttl {t}: must be a non-negative integer number of seconds (0 disarms the expiry)"
            )));
        }
        Some(0) => None,
        Some(t) => Some(t),
        None => per_extension(jobs_cfg.get("perExtensionTtl"), ext).or_else(|| jobs_cfg.get("ttlSeconds").and_then(Value::as_i64)),
    };
    let priority = prio_flag
        .or_else(|| per_extension(jobs_cfg.get("perExtensionPriority"), ext))
        .unwrap_or(ext.default_priority.unwrap_or(0));
    let p = Prepared {
        ext,
        template_hash: sha256(&format!("{PREAMBLE}{template}{contract}")),
        template,
        contract,
        ttl,
        priority,
        is_tagger: ext.kind == "action" && is_tags_schema(schema),
    };
    if ext.nodeless {
        if a.all || a.node.is_some() {
            return Err(submit_err(&format!("extension {} takes no node; submit it without -n / --all", a.extension)));
        }
    } else if !a.all && a.node.is_none() {
        return Err(submit_err("pass -n <node.path> or --all"));
    }
    let (installed, fresh) = crate::agent::presence(&scope.root);
    if !installed {
        return Err(submit_err(
            "no processing agent is set up. skill-map never runs jobs itself. Run `sm agent install` to install the skill to process the jobs",
        ));
    }
    if !fresh && !cli.global.json && !cli.global.quiet {
        eprintln!("•  the installed sm-process-jobs skill is from an older CLI; run sm agent install to refresh it");
    }

    if ext.nodeless {
        let node_id = format!("sm://{}", ext.qualified());
        let zero = "0".repeat(64);
        let hash = content_hash(ext, &node_id, &zero, &zero, &p.template_hash);
        let outcome = match (!a.force).then(|| active_duplicate(&conn, ext, &node_id, &hash)).transpose()?.flatten() {
            Some(existing) => Outcome::Duplicate(node_id, existing),
            None => {
                let content = render(&p.template, &[&p.contract], None);
                insert_job(&mut conn, &p, &node_id, &hash, &content)?
            }
        };
        return report_single(cli, scope, &conn, &p, outcome);
    }

    let nodes = store::load(&conn).map_err(op)?.map(|s| s.nodes).unwrap_or_default();
    if let Some(path) = a.node {
        let Some(node) = nodes.iter().find(|n| n.path == path) else {
            return Err(CliError::NotFound(format!("sm jobs submit: node {path} is not in the latest scan")));
        };
        if node.r#virtual == Some(true) {
            return Err(submit_err(&format!("node {path} is virtual (no backing file to render)")));
        }
        let outcome = submit_one(&mut conn, scope, &p, node, a.force)?;
        return report_single(cli, scope, &conn, &p, outcome);
    }

    let targets: Vec<&Node> =
        nodes.iter().filter(|n| n.r#virtual != Some(true) && precondition_matches(n, ext.precondition.as_ref())).collect();
    let mut outcomes = Vec::new();
    for n in &targets {
        outcomes.push(submit_one(&mut conn, scope, &p, n, a.force)?);
    }
    let created: Vec<(&String, &String)> =
        outcomes.iter().filter_map(|o| if let Outcome::Created(n, id) = o { Some((n, id)) } else { None }).collect();
    for (node, id) in &created {
        log_submit(scope, &p, node, id);
    }
    let refused: Vec<&Outcome> = outcomes.iter().filter(|o| !matches!(o, Outcome::Created(..))).collect();
    if cli.global.json {
        let refused_json: Vec<Value> = refused
            .iter()
            .map(|o| match o {
                Outcome::Duplicate(n, e) => json!({ "nodeId": n, "existingId": e, "reason": "duplicate" }),
                Outcome::Unreadable(n, d) => json!({ "nodeId": n, "reason": "unreadable", "detail": d }),
                Outcome::Drift(n) | Outcome::Created(n, _) => json!({ "nodeId": n, "reason": "drift" }),
            })
            .collect();
        println!(
            "{}",
            jsjson::to_string(&json!({
                "submitted": created.iter().map(|(n, id)| json!({ "id": id, "nodeId": n })).collect::<Vec<_>>(),
                "refused": refused_json,
                "counts": { "submitted": created.len(), "refused": refused.len(), "total": targets.len() },
            }))
        );
        return Ok(0);
    }
    if cli.global.quiet {
        return Ok(0);
    }
    if targets.is_empty() {
        eprintln!("•  no nodes match the precondition for {}", a.extension);
        return Ok(0);
    }
    for (node, id) in &created {
        eprintln!("✓  queued {}  {}", format::sanitize(id), format::sanitize(node));
    }
    for o in &refused {
        match o {
            Outcome::Duplicate(n, e) => {
                eprintln!("•  duplicate: active job {} already covers {}", format::sanitize(e), format::sanitize(n))
            }
            Outcome::Unreadable(_, d) => eprintln!("•  unreadable: {}", format::sanitize(d)),
            Outcome::Drift(n) | Outcome::Created(n, _) => {
                eprintln!("•  drift: {} changed on disk since the last scan (run sm scan)", format::sanitize(n))
            }
        }
    }
    eprintln!("✓  submitted {}, refused {} (of {} matching node(s))", created.len(), refused.len(), targets.len());
    Ok(0)
}

fn log_submit(scope: &Scope, p: &Prepared, node: &str, id: &str) {
    scope.log_entry(json!({
        "op": "jobs.submit", "target": node, "extension": p.ext.qualified(), "channel": "cli", "outcome": "queued", "id": id,
    }));
}

fn report_single(cli: &Cli, scope: &Scope, conn: &Connection, p: &Prepared, outcome: Outcome) -> Result<u8, CliError> {
    match outcome {
        Outcome::Duplicate(node, existing) => {
            if cli.global.json {
                println!("{}", jsjson::to_string(&json!({ "nodeId": node, "duplicate": true, "existingId": existing })));
            } else if !cli.global.quiet {
                eprintln!("•  duplicate: active job {} already covers {}", format::sanitize(&existing), format::sanitize(&node));
            }
            Ok(3)
        }
        Outcome::Drift(node) => Err(submit_err(&format!(
            "node {} changed on disk since the last scan; run sm scan and resubmit",
            format::sanitize(&node)
        ))),
        Outcome::Unreadable(_, detail) => Err(submit_err(&detail)),
        Outcome::Created(node, id) => {
            log_submit(scope, p, &node, &id);
            if cli.global.json {
                let job = get_job(conn, &id)?.unwrap_or(Value::Null);
                println!("{}", jsjson::to_string(&job));
            } else {
                println!("{id}");
            }
            Ok(0)
        }
    }
}

// ---------------------------------------------------------------- read verbs

pub fn list(cli: &Cli, db: &Path, status: Option<&str>, extension: Option<&str>, node: Option<&str>) -> Result<u8, CliError> {
    let conn = open(db)?;
    let mut sql = format!("SELECT {JOB_COLS} FROM state_jobs WHERE 1=1");
    let mut args: Vec<String> = Vec::new();
    if let Some(s) = status {
        args.push(s.into());
        sql += &format!(" AND status = ?{}", args.len());
    }
    if let Some(e) = extension {
        args.push(e.into());
        args.push(format!("%/{e}"));
        sql += &format!(" AND (extension_id = ?{} OR extension_id LIKE ?{})", args.len() - 1, args.len());
    }
    if let Some(n) = node {
        args.push(n.into());
        sql += &format!(" AND node_id = ?{}", args.len());
    }
    sql += " ORDER BY created_at DESC, id DESC";
    let mut st = conn.prepare(&sql).map_err(op)?;
    let jobs: Vec<Value> = st
        .query_map(rusqlite::params_from_iter(args.iter()), row_to_job)
        .map_err(op)?
        .collect::<Result<_, _>>()
        .map_err(op)?;
    if cli.global.json {
        let pubs: Vec<Value> = jobs.into_iter().map(public).collect();
        println!("{}", jsjson::to_string(&pubs));
        return Ok(0);
    }
    if jobs.is_empty() {
        if !cli.global.quiet {
            let filtered = status.is_some() || extension.is_some() || node.is_some();
            eprintln!("•  no jobs{}", if filtered { " matching the filter" } else { "" });
        }
        return Ok(0);
    }
    let mut out = String::new();
    for j in &jobs {
        out += &format!(
            "{}  {}  {}  {}  {}\n",
            format::sanitize(s(j, "id")),
            format::sanitize(s(j, "status")),
            j["priority"],
            format::sanitize(s(j, "extensionId")),
            format::sanitize(s(j, "nodeId"))
        );
    }
    print!("{out}");
    Ok(0)
}

pub fn iso(ms: Option<i64>) -> String {
    let Some(ms) = ms else { return "(none)".into() };
    let t = time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.year(),
        t.month() as u8,
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.millisecond()
    )
}

pub fn show(cli: &Cli, db: &Path, id: &str) -> Result<u8, CliError> {
    let conn = open(db)?;
    let Some(job) = get_job(&conn, id)? else {
        return Err(CliError::NotFound(format!("sm jobs show: job {id} not found")));
    };
    if cli.global.json {
        println!("{}", jsjson::to_string(&public(job)));
        return Ok(0);
    }
    let ttl = job["ttlSeconds"].as_i64().map_or("(none)".to_string(), |t| format!("{t}s"));
    print!(
        "job {}\n  status       {}\n  extension    {}\n  kind         {}\n  node         {}\n  priority     {}\n  ttl          {ttl}\n  contentHash  {}\n  createdAt    {}\n  claimedAt    {}\n  finishedAt   {}\n  runner       {}\n",
        format::sanitize(s(&job, "id")),
        format::sanitize(s(&job, "status")),
        format::sanitize(s(&job, "extensionId")),
        format::sanitize(s(&job, "extensionKind")),
        format::sanitize(s(&job, "nodeId")),
        job["priority"],
        format::sanitize(s(&job, "contentHash")),
        iso(job["createdAt"].as_i64()),
        iso(job["claimedAt"].as_i64()),
        iso(job["finishedAt"].as_i64()),
        job["runner"].as_str().map_or("(none)".to_string(), format::sanitize)
    );
    Ok(0)
}

pub fn preview(db: &Path, id: Option<&str>, last: bool) -> Result<u8, CliError> {
    let conn = open(db)?;
    if last && id.is_some() {
        return Err(CliError::Usage("sm jobs preview: pass either <job.id> or --last, not both".into()));
    }
    if !last && id.is_none() {
        return Err(CliError::Usage("sm jobs preview: pass <job.id> or --last".into()));
    }
    let job = if last {
        conn.query_row(&format!("SELECT {JOB_COLS} FROM state_jobs ORDER BY created_at DESC, id DESC LIMIT 1"), [], row_to_job)
            .optional()
            .map_err(op)?
    } else {
        get_job(&conn, id.unwrap_or_default())?
    };
    let Some(job) = job else {
        return Err(CliError::NotFound(if last {
            "sm jobs preview: no jobs submitted yet, nothing to preview".into()
        } else {
            format!("sm jobs preview: job {} not found", id.unwrap_or_default())
        }));
    };
    let Some(content) = job_content(&conn, s(&job, "contentHash"))? else {
        return Err(CliError::NotFound(format!(
            "sm jobs preview: job {} has no stored content (state_job_contents row missing)",
            format::sanitize(s(&job, "id"))
        )));
    };
    print!("{}", format::sanitize(&unescape_close(&content)));
    Ok(0)
}

pub struct ClaimArgs<'a> {
    pub filter: Option<&'a str>,
    pub wait: bool,
    pub interval: Option<&'a str>,
    pub timeout: Option<&'a str>,
}

/// `claimNext`: one atomic UPDATE … RETURNING (highest priority, oldest).
fn claim_next(conn: &Connection, now: i64, filter: Option<&str>) -> Result<Option<(String, String, String)>, CliError> {
    let cond = if filter.is_some() { "AND (extension_id = ?2 OR extension_id LIKE '%/' || ?2)" } else { "" };
    let sql = format!(
        "UPDATE state_jobs SET status = 'running', claimed_at = ?1, runner = 'agent',
            expires_at = CASE WHEN ttl_seconds IS NULL THEN NULL ELSE ?1 + ttl_seconds * 1000 END
         WHERE id = (SELECT id FROM state_jobs WHERE status = 'queued' {cond} ORDER BY priority DESC, created_at ASC LIMIT 1)
           AND status = 'queued'
         RETURNING id, nonce, content_hash"
    );
    let map = |r: &rusqlite::Row| Ok((r.get(0)?, r.get(1)?, r.get(2)?));
    let res = match filter {
        Some(f) => conn.query_row(&sql, params![now, f], map),
        None => conn.query_row(&sql, params![now], map),
    };
    res.optional().map_err(op)
}

pub fn claim(cli: &Cli, db: &Path, a: ClaimArgs) -> Result<u8, CliError> {
    let conn = open(db)?;
    let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
    let secs = |raw: Option<&str>, what: &str| -> Result<Option<u64>, CliError> {
        match raw {
            None => Ok(None),
            Some(t) => t.trim().parse::<u64>().ok().filter(|v| *v >= 1).map(Some).ok_or_else(|| {
                CliError::Usage(format!("sm jobs claim: --{what} must be a positive integer number of seconds, got {t}"))
            }),
        }
    };
    let (interval, timeout) = if a.wait { (secs(a.interval, "interval")?, secs(a.timeout, "timeout")?) } else { (None, None) };
    let interval = interval.unwrap_or(2);
    let deadline = timeout.map(|t| std::time::Instant::now() + std::time::Duration::from_secs(t));
    loop {
        let now = now_ms();
        reap(&conn, now)?;
        if let Some((id, nonce, hash)) = claim_next(&conn, now, a.filter)? {
            let Some(content) = job_content(&conn, &hash)? else {
                conn.execute(
                    "UPDATE state_jobs SET status = 'failed', failure_reason = 'job-file-missing', finished_at = ?2 WHERE id = ?1",
                    params![id, now],
                )
                .map_err(op)?;
                return Err(CliError::Usage(format!(
                    "sm jobs claim: job {} has no stored content (state_job_contents row missing); marked failed / job-file-missing",
                    format::sanitize(&id)
                )));
            };
            if cli.global.json {
                println!("{}", jsjson::to_string(&json!({ "id": id, "nonce": nonce, "content": content })));
            } else {
                println!("{id}");
            }
            return Ok(0);
        }
        if !a.wait {
            return Ok(1);
        }
        let sleep = match deadline {
            Some(d) => {
                let left = d.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    return Ok(1);
                }
                left.min(std::time::Duration::from_secs(interval))
            }
            None => std::time::Duration::from_secs(interval),
        };
        std::thread::sleep(sleep);
    }
}

pub fn status(cli: &Cli, db: &Path, id: Option<&str>) -> Result<u8, CliError> {
    let conn = open(db)?;
    if let Some(id) = id {
        let Some(job) = get_job(&conn, id)? else {
            return Err(CliError::NotFound(format!("sm jobs status: job {id} not found")));
        };
        if cli.global.json {
            println!(
                "{}",
                jsjson::to_string(&json!({ "id": job["id"], "status": job["status"], "failureReason": job["failureReason"] }))
            );
        } else {
            println!("job {}  {}", format::sanitize(s(&job, "id")), format::sanitize(s(&job, "status")));
        }
        return Ok(0);
    }
    let mut counts = Map::new();
    for k in ["queued", "running", "completed", "failed", "cancelled"] {
        counts.insert(k.into(), json!(0));
    }
    let mut st = conn.prepare("SELECT status, COUNT(*) FROM state_jobs GROUP BY status").map_err(op)?;
    for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).map_err(op)? {
        let (k, n) = row.map_err(op)?;
        counts.insert(k, json!(n));
    }
    if cli.global.json {
        println!("{}", jsjson::to_string(&counts));
    } else {
        print!(
            "queued     {}\nrunning    {}\ncompleted  {}\nfailed     {}\ncancelled  {}\n",
            counts["queued"], counts["running"], counts["completed"], counts["failed"], counts["cancelled"]
        );
    }
    Ok(0)
}

/// `sm jobs cancel` (`fail = false`) and `sm jobs fail` (`fail = true`).
pub fn terminate(cli: &Cli, scope: &Scope, db: &Path, id: Option<&str>, all: bool, fail: bool) -> Result<u8, CliError> {
    let verb = if fail { "fail" } else { "cancel" };
    let err = |m: String, code: u8| CliError::Raw(format!("✕  sm jobs {verb}: {m}"), code);
    let conn = open(db)?;
    if all && id.is_some() {
        return Err(err("pass either <job.id> or --all, not both".into(), 2));
    }
    if !all && id.is_none() {
        return Err(err("pass <job.id> or --all".into(), 2));
    }
    let (status, reason) = if fail { ("failed", Some("user-failed")) } else { ("cancelled", None) };
    let now = now_ms();
    if all {
        let mut st = conn
            .prepare(
                "UPDATE state_jobs SET status = ?1, failure_reason = ?2, finished_at = ?3 WHERE status IN ('queued','running') RETURNING id",
            )
            .map_err(op)?;
        let ids: Vec<String> =
            st.query_map(params![status, reason, now], |r| r.get(0)).map_err(op)?.collect::<Result<_, _>>().map_err(op)?;
        if !fail {
            scope.log_entry(json!({
                "op": "jobs.cancel", "target": "*", "channel": "cli", "outcome": "cancelled", "detail": format!("cancelled={}", ids.len()),
            }));
        }
        if cli.global.json {
            println!("{}", jsjson::to_string(&json!({ if fail { "failed" } else { "cancelled" }: ids.len() })));
        } else if !cli.global.quiet {
            eprintln!("✓  {} {} active job(s)", if fail { "failed" } else { "cancelled" }, ids.len());
        }
        return Ok(0);
    }
    let id = id.unwrap_or_default();
    let n = conn
        .execute(
            "UPDATE state_jobs SET status = ?1, failure_reason = ?2, finished_at = ?3 WHERE id = ?4 AND status IN ('queued','running')",
            params![status, reason, now, id],
        )
        .map_err(op)?;
    if n == 0 {
        return Err(if get_job(&conn, id)?.is_some() {
            err(format!("job {id} is already terminal"), 2)
        } else {
            err(format!("job {id} not found"), 5)
        });
    }
    if !fail {
        scope.log_entry(json!({ "op": "jobs.cancel", "target": "*", "channel": "cli", "outcome": "cancelled", "id": id }));
    }
    if cli.global.json {
        println!("{}", jsjson::to_string(&json!({ "id": id, if fail { "failed" } else { "cancelled" }: true })));
    } else if !cli.global.quiet {
        eprintln!("✓  {} {id}", if fail { "failed" } else { "cancelled" });
    }
    Ok(0)
}
