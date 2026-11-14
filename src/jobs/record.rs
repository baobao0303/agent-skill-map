//! The job callback: `sm record`, per `spec/job-lifecycle.md`: an external
//! agent that claimed a job closes it here. `sm record` verifies `--nonce`
//! against the job, validates the `--status completed` report against the
//! action's report schema, writes the `state_executions` row, and transitions
//! the job to completed / failed atomically.

use crate::catalog;
use crate::cli::{Cli, CliError};
use crate::format;
use crate::jobs::{self, is_summary_schema, is_tags_schema};
use crate::jsjson;
use crate::scope::Scope;
use crate::validate;
use clap::Args;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::io::Read;
use std::path::Path;

#[derive(Args, Debug, Clone)]
pub struct RecordArgs {
    #[arg(long)]
    pub id: String,
    #[arg(long)]
    pub nonce: String,
    #[arg(long)]
    pub status: String,
    #[arg(long)]
    pub report: Option<String>,
    #[arg(long)]
    pub tokens_in: Option<String>,
    #[arg(long)]
    pub tokens_out: Option<String>,
    #[arg(long)]
    pub duration_ms: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub error: Option<String>,
}

struct RecordMetrics {
    tokens_in: Option<i64>,
    tokens_out: Option<i64>,
    duration_ms: Option<i64>,
    model: Option<String>,
}

fn parse_non_neg_int_flag(flag: &str, raw: Option<&str>) -> Result<Option<i64>, CliError> {
    let Some(raw) = raw else { return Ok(None) };
    let trimmed = raw.trim();
    if trimmed.is_empty() || !trimmed.chars().all(|c| c.is_ascii_digit()) {
        return Err(CliError::Raw(
            format!("✕  sm record: {flag} must be a non-negative integer, got {raw}"),
            2,
        ));
    }
    match trimmed.parse::<i64>() {
        Ok(v) if v >= 0 => Ok(Some(v)),
        _ => Err(CliError::Raw(
            format!("✕  sm record: {flag} must be a non-negative integer, got {raw}"),
            2,
        )),
    }
}

struct ResolvedExtension {
    schema: Value,
    analyzer_ids: Option<Vec<String>>,
}

fn resolve_extension(scope: &Scope, extension_id: &str, extension_kind: &str) -> Result<ResolvedExtension, String> {
    if extension_id.starts_with("skill:") {
        let schema = validate::schema("https://skill-map.ai/spec/v1/skill-actions/report.schema.json")
            .cloned()
            .ok_or_else(|| "no report schema".to_string())?;
        return Ok(ResolvedExtension {
            schema,
            analyzer_ids: None,
        });
    }

    // 1. Built-ins
    for item in catalog::BUILT_INS.iter() {
        let plugin = item.get("pluginId").and_then(Value::as_str).unwrap_or("");
        let id = item.get("id").and_then(Value::as_str).unwrap_or("");
        let qualified = format!("{plugin}/{id}");
        let kind = item.get("kind").and_then(Value::as_str).unwrap_or("");

        if (qualified == extension_id || id == extension_id) && kind == extension_kind {
            let schema = item.get("reportSchema").filter(|v| v.is_object()).cloned();
            let Some(schema) = schema else {
                return Err("no report schema".to_string());
            };
            let analyzer_ids = item
                .get("precondition")
                .and_then(|p| p.get("analyzerIds"))
                .and_then(Value::as_array)
                .map(|arr| arr.iter().filter_map(Value::as_str).map(str::to_string).collect());
            return Ok(ResolvedExtension {
                schema,
                analyzer_ids,
            });
        }
    }

    // 2. Drop-ins
    let plugins_dir = scope.dir().join("plugins");
    if plugins_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(plugins_dir) {
            for p in entries.filter_map(Result::ok) {
                let plugin_name = p.file_name().to_string_lossy().to_string();
                let folder = match extension_kind {
                    "action" => "actions",
                    "analyzer" => "analyzers",
                    _ => continue,
                };
                let kind_dir = p.path().join(folder);
                if !kind_dir.is_dir() {
                    continue;
                }
                if let Ok(ext_entries) = std::fs::read_dir(kind_dir) {
                    for e in ext_entries.filter_map(Result::ok) {
                        let id = e.file_name().to_string_lossy().to_string();
                        let qualified = format!("{plugin_name}/{id}");
                        if qualified == extension_id || id == extension_id {
                            let report_schema_path = e.path().join("report.schema.json");
                            if !report_schema_path.is_file() {
                                return Err("no report schema".to_string());
                            }
                            let text = std::fs::read_to_string(&report_schema_path).map_err(|e| e.to_string())?;
                            let schema: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;

                            let manifest_path = e.path().join("extension.json");
                            let analyzer_ids = if manifest_path.is_file() {
                                std::fs::read_to_string(manifest_path)
                                    .ok()
                                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                                    .and_then(|m| m.get("precondition").cloned())
                                    .and_then(|p| p.get("analyzerIds").cloned())
                                    .and_then(|a| a.as_array().cloned())
                                    .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                            } else {
                                None
                            };

                            return Ok(ResolvedExtension {
                                schema,
                                analyzer_ids,
                            });
                        }
                    }
                }
            }
        }
    }

    Err(format!("{extension_kind} not found"))
}

fn matches_qualified_extension_filter(extension_id: &str, filter: &[String]) -> bool {
    if filter.is_empty() {
        return true;
    }
    if filter.iter().any(|f| f == extension_id) {
        return true;
    }
    if let Some((_, ext_name)) = extension_id.split_once('/') {
        if filter.iter().any(|f| f == ext_name) {
            return true;
        }
    }
    false
}

fn record_failed_in_db(
    conn: &mut Connection,
    job: &Value,
    failure_reason: &str,
    error_text: Option<&str>,
    metrics: &RecordMetrics,
    now: i64,
) -> Result<String, CliError> {
    let job_id = job["id"].as_str().unwrap_or("");
    let job_ext_id = job["extensionId"].as_str().unwrap_or("");
    let job_ext_version = job["extensionVersion"].as_str().unwrap_or("");
    let job_node_id = job["nodeId"].as_str().unwrap_or("");
    let job_content_hash = job["contentHash"].as_str();
    let job_runner = job["runner"].as_str();
    let job_claimed_at = job["claimedAt"].as_i64();

    let tx = conn.transaction().map_err(|e| CliError::Operational(e.to_string()))?;
    let updated = tx
        .execute(
            "UPDATE state_jobs SET status = 'failed', failure_reason = ?1, finished_at = ?2 WHERE id = ?3 AND status = 'running'",
            params![failure_reason, now, job_id],
        )
        .map_err(|e| CliError::Operational(e.to_string()))?;

    if updated == 0 {
        return Err(CliError::Raw(
            format!("✕  sm record: job {job_id} is not in running state"),
            2,
        ));
    }

    let exec_id = jobs::new_id("e");
    let node_ids_json = jsjson::to_string(&vec![job_node_id]);
    tx.execute(
        "INSERT INTO state_executions (
            id, kind, extension_id, extension_version, node_ids_json, content_hash,
            status, failure_reason, exit_code, runner, started_at, finished_at,
            duration_ms, tokens_in, tokens_out, model, report_json, job_id
        ) VALUES (?1, 'action', ?2, ?3, ?4, ?5, 'failed', ?6, NULL, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            exec_id,
            job_ext_id,
            job_ext_version,
            node_ids_json,
            job_content_hash,
            failure_reason,
            job_runner,
            job_claimed_at.unwrap_or(now),
            now,
            metrics.duration_ms,
            metrics.tokens_in,
            metrics.tokens_out,
            metrics.model,
            error_text,
            job_id,
        ],
    )
    .map_err(|e| CliError::Operational(e.to_string()))?;

    tx.commit().map_err(|e| CliError::Operational(e.to_string()))?;
    Ok(exec_id)
}

fn emit_synthetic_envelope(
    run_id: &str,
    now: i64,
    job: &Value,
    exec_id: &str,
    status: &str,
    metrics: &RecordMetrics,
    error_text: Option<&str>,
    failure_reason: Option<&str>,
    tags_proposed: Option<&[String]>,
) {
    let job_id = job["id"].as_str().unwrap_or("");
    let job_ext_id = job["extensionId"].as_str().unwrap_or("");
    let job_ext_version = job["extensionVersion"].as_str().unwrap_or("");
    let job_ext_kind = job["extensionKind"].as_str().unwrap_or("");
    let job_node_id = job["nodeId"].as_str().unwrap_or("");
    let job_ttl_seconds = job.get("ttlSeconds").and_then(Value::as_i64);
    let job_priority = job.get("priority").and_then(Value::as_i64).unwrap_or(0);
    let completed = status == "completed";

    // 1. run.started
    println!(
        "{}",
        jsjson::to_string(&json!({
            "type": "run.started",
            "timestamp": now,
            "runId": run_id,
            "jobId": null,
            "data": { "mode": "external" }
        }))
    );

    // 2. job.claimed
    println!(
        "{}",
        jsjson::to_string(&json!({
            "type": "job.claimed",
            "timestamp": now,
            "runId": run_id,
            "jobId": job_id,
            "data": {
                "extensionId": job_ext_id,
                "extensionVersion": job_ext_version,
                "extensionKind": job_ext_kind,
                "nodeId": job_node_id,
                "ttlSeconds": job_ttl_seconds,
                "priority": job_priority
            }
        }))
    );

    // 3. job.callback.received
    println!(
        "{}",
        jsjson::to_string(&json!({
            "type": "job.callback.received",
            "timestamp": now,
            "runId": run_id,
            "jobId": job_id,
            "data": {
                "status": status,
                "model": metrics.model,
                "executionId": exec_id
            }
        }))
    );

    // 4. job.completed or job.failed
    if completed {
        let mut data = serde_json::Map::new();
        data.insert("extensionId".into(), json!(job_ext_id));
        data.insert("extensionKind".into(), json!(job_ext_kind));
        data.insert("nodeId".into(), json!(job_node_id));
        data.insert("durationMs".into(), json!(metrics.duration_ms));
        data.insert("tokensIn".into(), json!(metrics.tokens_in));
        data.insert("tokensOut".into(), json!(metrics.tokens_out));
        data.insert("model".into(), json!(metrics.model));
        data.insert("executionId".into(), json!(exec_id));
        if let Some(tags) = tags_proposed {
            data.insert("tagsProposed".into(), json!(tags));
        }
        println!(
            "{}",
            jsjson::to_string(&json!({
                "type": "job.completed",
                "timestamp": now,
                "runId": run_id,
                "jobId": job_id,
                "data": Value::Object(data)
            }))
        );
    } else {
        println!(
            "{}",
            jsjson::to_string(&json!({
                "type": "job.failed",
                "timestamp": now,
                "runId": run_id,
                "jobId": job_id,
                "data": {
                    "reason": failure_reason,
                    "message": error_text,
                    "exitCode": null,
                    "durationMs": metrics.duration_ms
                }
            }))
        );
    }

    // 5. run.summary
    println!(
        "{}",
        jsjson::to_string(&json!({
            "type": "run.summary",
            "timestamp": now,
            "runId": run_id,
            "jobId": null,
            "data": {
                "jobsAttempted": 1,
                "jobsCompleted": if completed { 1 } else { 0 },
                "jobsFailed": if completed { 0 } else { 1 },
                "totalDurationMs": metrics.duration_ms.unwrap_or(0),
                "totalTokensIn": metrics.tokens_in.unwrap_or(0),
                "totalTokensOut": metrics.tokens_out.unwrap_or(0)
            }
        }))
    );
}

pub fn run(cli: &Cli, scope: &Scope, db_path: &Path, args: &RecordArgs) -> Result<u8, CliError> {
    if args.status != "completed" && args.status != "failed" {
        return Err(CliError::Raw(
            format!("✕  sm record: --status must be completed or failed, got {}", args.status),
            2,
        ));
    }
    if args.status == "completed" && args.report.is_none() {
        return Err(CliError::Raw(
            "✕  sm record: --status completed requires --report <path|-> (the agent's report)".into(),
            2,
        ));
    }

    let metrics = RecordMetrics {
        tokens_in: parse_non_neg_int_flag("--tokens-in", args.tokens_in.as_deref())?,
        tokens_out: parse_non_neg_int_flag("--tokens-out", args.tokens_out.as_deref())?,
        duration_ms: parse_non_neg_int_flag("--duration-ms", args.duration_ms.as_deref())?,
        model: args.model.clone(),
    };

    let mut conn = jobs::open(db_path)?;
    let job = match jobs::get_job(&conn, &args.id)? {
        Some(j) => j,
        None => return Err(CliError::Raw(format!("✕  sm record: job {} not found", args.id), 5)),
    };

    let job_nonce = job["nonce"].as_str().unwrap_or("");
    if job_nonce != args.nonce {
        return Err(CliError::Raw(
            format!("✕  sm record: nonce does not match job {}", args.id),
            4,
        ));
    }

    let job_status = job["status"].as_str().unwrap_or("");
    if job_status != "running" {
        return Err(CliError::Raw(
            format!("✕  sm record: job {} is not in running state (status {})", args.id, format::sanitize(job_status)),
            2,
        ));
    }

    let now = jobs::now_ms();
    let run_id = jobs::new_id("r-ext");

    let job_id = job["id"].as_str().unwrap_or("");
    let job_ext_id = job["extensionId"].as_str().unwrap_or("");
    let job_ext_version = job["extensionVersion"].as_str().unwrap_or("");
    let job_ext_kind = job["extensionKind"].as_str().unwrap_or("");
    let job_node_id = job["nodeId"].as_str().unwrap_or("");
    let job_content_hash = job["contentHash"].as_str();
    let job_runner = job["runner"].as_str();
    let job_claimed_at = job["claimedAt"].as_i64();

    if args.status == "failed" {
        let exec_id = record_failed_in_db(&mut conn, &job, "runner-error", args.error.as_deref(), &metrics, now)?;

        scope.log_entry(json!({
            "op": "jobs.record",
            "target": job_node_id,
            "extension": job_ext_id,
            "channel": "cli",
            "outcome": "failed",
            "id": job_id,
        }));

        if cli.global.json {
            emit_synthetic_envelope(&run_id, now, &job, &exec_id, "failed", &metrics, args.error.as_deref(), Some("runner-error"), None);
        } else if !cli.global.quiet {
            eprintln!(
                "•  recorded {}: job {} failed (runner-error)",
                format::sanitize(&exec_id),
                format::sanitize(job_id)
            );
        }
        return Ok(0);
    }

    // args.status == "completed"
    let report_src = args.report.as_deref().unwrap();
    let report_text = if report_src == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| CliError::Raw(format!("✕  sm record: cannot read the report from stdin ({e})"), 2))?;
        buf
    } else {
        let path = scope.root.join(report_src);
        std::fs::read_to_string(&path)
            .map_err(|e| CliError::Raw(format!("✕  sm record: cannot read the report from {report_src} ({e})"), 2))?
    };

    let parsed: Value = match serde_json::from_str(&report_text) {
        Ok(v) => v,
        Err(e) => {
            let detail = e.to_string();
            record_failed_in_db(&mut conn, &job, "report-invalid", None, &metrics, now)?;
            return Err(CliError::Raw(
                format!("✕  sm record: report failed schema validation: {detail}"),
                2,
            ));
        }
    };

    let resolved = match resolve_extension(scope, job_ext_id, job_ext_kind) {
        Ok(r) => r,
        Err(detail) => {
            return Err(CliError::Raw(
                format!(
                    "✕  sm record: cannot resolve the report schema for extension {} ({})",
                    format::sanitize(job_ext_id),
                    format::sanitize(&detail)
                ),
                2,
            ));
        }
    };

    let errors = validate::validate_schema(&resolved.schema, &parsed);
    if !errors.is_empty() {
        let detail = validate::format(&errors);
        record_failed_in_db(&mut conn, &job, "report-invalid", None, &metrics, now)?;
        return Err(CliError::Raw(
            format!("✕  sm record: report failed schema validation: {detail}"),
            2,
        ));
    }

    if job_ext_kind == "analyzer" {
        const RESERVED: &[&str] = &["injection-detected", "content-suspicious", "content-malformed"];
        let mut found_reserved = Vec::new();
        if let Some(findings) = parsed.get("findings").and_then(Value::as_array) {
            for f in findings {
                if let Some(t) = f.get("type").and_then(Value::as_str) {
                    if RESERVED.contains(&t) && !found_reserved.contains(&t) {
                        found_reserved.push(t);
                    }
                }
            }
        }
        if !found_reserved.is_empty() {
            let detail = format!(
                "findings[] uses the reserved type slug(s) {} (injection-detected / content-suspicious / content-malformed are kernel-derived and MUST NOT be emitted by extensions)",
                found_reserved.join(", ")
            );
            record_failed_in_db(&mut conn, &job, "report-invalid", None, &metrics, now)?;
            return Err(CliError::Raw(
                format!("✕  sm record: report failed schema validation: {detail}"),
                2,
            ));
        }
    }

    // Report is valid, begin atomic commit
    let tx = conn.transaction().map_err(|e| CliError::Operational(e.to_string()))?;

    let updated = tx
        .execute(
            "UPDATE state_jobs SET status = 'completed', failure_reason = NULL, finished_at = ?1 WHERE id = ?2 AND status = 'running'",
            params![now, job_id],
        )
        .map_err(|e| CliError::Operational(e.to_string()))?;

    if updated == 0 {
        return Err(CliError::Raw(
            format!("✕  sm record: job {job_id} is not in running state"),
            2,
        ));
    }

    let exec_id = jobs::new_id("e");
    let node_ids_json = jsjson::to_string(&vec![job_node_id]);
    let report_json = jsjson::to_string(&parsed);

    tx.execute(
        "INSERT INTO state_executions (
            id, kind, extension_id, extension_version, node_ids_json, content_hash,
            status, failure_reason, exit_code, runner, started_at, finished_at,
            duration_ms, tokens_in, tokens_out, model, report_json, job_id
        ) VALUES (?1, 'action', ?2, ?3, ?4, ?5, 'completed', NULL, NULL, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            exec_id,
            job_ext_id,
            job_ext_version,
            node_ids_json,
            job_content_hash,
            job_runner,
            job_claimed_at.unwrap_or(now),
            now,
            metrics.duration_ms,
            metrics.tokens_in,
            metrics.tokens_out,
            metrics.model,
            report_json,
            job_id,
        ],
    )
    .map_err(|e| CliError::Operational(e.to_string()))?;

    // Summaries write-through
    if job_ext_kind == "action" && is_summary_schema(&resolved.schema) {
        let node_info: Option<(String, String)> = tx
            .query_row(
                "SELECT kind, body_hash FROM scan_nodes WHERE path = ?1",
                [job_node_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| CliError::Operational(e.to_string()))?;

        if let Some((node_kind, body_hash)) = node_info {
            tx.execute(
                "INSERT INTO state_summaries (
                    node_id, kind, summarizer_action_id, summarizer_version,
                    body_hash_at_generation, generated_at, model, summary_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                ON CONFLICT(node_id, summarizer_action_id) DO UPDATE SET
                    kind = excluded.kind,
                    summarizer_version = excluded.summarizer_version,
                    body_hash_at_generation = excluded.body_hash_at_generation,
                    generated_at = excluded.generated_at,
                    model = excluded.model,
                    summary_json = excluded.summary_json",
                params![
                    job_node_id,
                    node_kind,
                    job_ext_id,
                    job_ext_version,
                    body_hash,
                    now,
                    metrics.model,
                    report_json,
                ],
            )
            .map_err(|e| CliError::Operational(e.to_string()))?;
        }
    }

    // Findings write-through
    let body_hash: Option<String> = tx
        .query_row("SELECT body_hash FROM scan_nodes WHERE path = ?1", [job_node_id], |r| r.get(0))
        .optional()
        .map_err(|e| CliError::Operational(e.to_string()))?;

    if let Some(body_hash) = body_hash {
        tx.execute(
            "DELETE FROM state_findings WHERE node_id = ?1 AND extension_id = ?2",
            params![job_node_id, job_ext_id],
        )
        .map_err(|e| CliError::Operational(e.to_string()))?;

        if job_ext_kind == "analyzer" {
            if let Some(findings) = parsed.get("findings").and_then(Value::as_array) {
                let fallback_confidence = parsed.get("confidence").and_then(Value::as_f64).unwrap_or(0.0);
                for f in findings {
                    if let Some(obj) = f.as_object() {
                        let f_type = obj.get("type").and_then(Value::as_str).unwrap_or("");
                        let f_severity = match obj.get("severity").and_then(Value::as_str) {
                            Some("error") => "error",
                            Some("warn") => "warn",
                            Some("info") => "info",
                            _ => "info",
                        };
                        let f_message = obj.get("message").and_then(Value::as_str).unwrap_or("");
                        let f_detail = obj.get("detail").and_then(Value::as_str);
                        let f_confidence = obj.get("confidence").and_then(Value::as_f64).unwrap_or(fallback_confidence);

                        tx.execute(
                            "INSERT INTO state_findings (
                                node_id, extension_id, extension_version, origin, type,
                                severity, message, detail, confidence, model,
                                body_hash_at_generation, generated_at, job_id
                            ) VALUES (?1, ?2, ?3, 'extension', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                            params![
                                job_node_id,
                                job_ext_id,
                                job_ext_version,
                                f_type,
                                f_severity,
                                f_message,
                                f_detail,
                                f_confidence,
                                metrics.model,
                                body_hash,
                                now,
                                job_id,
                            ],
                        )
                        .map_err(|e| CliError::Operational(e.to_string()))?;
                    }
                }
            }
        }

        tx.execute(
            "DELETE FROM state_findings WHERE node_id = ?1 AND origin = 'kernel'",
            [job_node_id],
        )
        .map_err(|e| CliError::Operational(e.to_string()))?;

        if let Some(safety) = parsed.get("safety").and_then(Value::as_object) {
            let confidence = parsed.get("confidence").and_then(Value::as_f64).unwrap_or(0.0);
            let mut kernel_rows = Vec::new();

            if safety.get("injectionDetected").and_then(Value::as_bool) == Some(true) {
                kernel_rows.push((
                    "injection-detected",
                    "warn",
                    "The model flagged a prompt-injection attempt inside the node content",
                    safety.get("injectionDetails").and_then(Value::as_str),
                ));
            }
            if safety.get("contentQuality").and_then(Value::as_str) == Some("suspicious") {
                kernel_rows.push((
                    "content-suspicious",
                    "warn",
                    "The model flagged the node content as suspicious (unusual patterns without a concrete injection)",
                    None,
                ));
            }
            if safety.get("contentQuality").and_then(Value::as_str) == Some("malformed") {
                kernel_rows.push((
                    "content-malformed",
                    "warn",
                    "The model flagged the node content as malformed (unparseable or structurally broken)",
                    None,
                ));
            }

            for (k_type, k_sev, k_msg, k_det) in kernel_rows {
                tx.execute(
                    "INSERT INTO state_findings (
                        node_id, extension_id, extension_version, origin, type,
                        severity, message, detail, confidence, model,
                        body_hash_at_generation, generated_at, job_id
                    ) VALUES (?1, ?2, ?3, 'kernel', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        job_node_id,
                        job_ext_id,
                        job_ext_version,
                        k_type,
                        k_sev,
                        k_msg,
                        k_det,
                        confidence,
                        metrics.model,
                        body_hash,
                        now,
                        job_id,
                    ],
                )
                .map_err(|e| CliError::Operational(e.to_string()))?;
            }
        }
    }

    // Fixer resolutions
    if job_ext_kind == "action" {
        if let Some(analyzer_ids) = &resolved.analyzer_ids {
            if !analyzer_ids.is_empty() {
                if let Some(resolved_arr) = parsed.get("resolved").and_then(Value::as_array) {
                    for entry in resolved_arr {
                        if let Some(obj) = entry.as_object() {
                            let id = obj.get("id").and_then(Value::as_i64);
                            let state = obj.get("state").and_then(Value::as_str);
                            let note = obj.get("note").and_then(Value::as_str).unwrap_or("");
                            let by = obj.get("by").and_then(Value::as_str);

                            if let (Some(id), Some(state)) = (id, state) {
                                let (valid_state, valid_by) = match state {
                                    "human-decision" => (true, None),
                                    "fixed" => match by {
                                        Some("human") => (true, Some("human")),
                                        Some("fixer") => (true, Some("fixer")),
                                        _ => (false, None),
                                    },
                                    _ => (false, None),
                                };

                                if valid_state {
                                    let finding: Option<(String, String)> = tx
                                        .query_row(
                                            "SELECT node_id, extension_id FROM state_findings WHERE id = ?1",
                                            [id],
                                            |r| Ok((r.get(0)?, r.get(1)?)),
                                        )
                                        .optional()
                                        .map_err(|e| CliError::Operational(e.to_string()))?;

                                    if let Some((f_node_id, f_ext_id)) = finding {
                                        if f_node_id == job_node_id && matches_qualified_extension_filter(&f_ext_id, analyzer_ids) {
                                            tx.execute(
                                                "UPDATE state_findings SET
                                                    resolution = ?1,
                                                    resolution_actor = ?2,
                                                    resolution_note = ?3,
                                                    resolution_by = ?4,
                                                    resolution_at = ?5
                                                 WHERE id = ?6",
                                                params![state, valid_by, note, job_ext_id, now, id],
                                            )
                                            .map_err(|e| CliError::Operational(e.to_string()))?;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    tx.commit().map_err(|e| CliError::Operational(e.to_string()))?;

    // Tags proposal extraction
    let tags_proposed = if job_ext_kind == "action" && is_tags_schema(&resolved.schema) {
        let mut tags = Vec::new();
        if let Some(arr) = parsed.get("tags").and_then(Value::as_array) {
            for t in arr {
                if let Some(s) = t.as_str() {
                    let trimmed = s.trim();
                    if !trimmed.is_empty() {
                        tags.push(trimmed.to_string());
                    }
                }
            }
        }
        Some(tags)
    } else {
        None
    };

    scope.log_entry(json!({
        "op": "jobs.record",
        "target": job_node_id,
        "extension": job_ext_id,
        "channel": "cli",
        "outcome": "completed",
        "id": job_id,
    }));

    if cli.global.json {
        emit_synthetic_envelope(
            &run_id,
            now,
            &job,
            &exec_id,
            "completed",
            &metrics,
            None,
            None,
            tags_proposed.as_deref(),
        );
    } else if !cli.global.quiet {
        if let Some(tags) = &tags_proposed {
            if !tags.is_empty() {
                let count = tags.len();
                let noun = if count == 1 { "tag" } else { "tags" };
                eprintln!(
                    "ℹ  {} {} proposed for {}: {}, review and save them in the tags editor",
                    count,
                    noun,
                    format::sanitize(job_node_id),
                    format::sanitize(&tags.join(", "))
                );
            }
        }
        eprintln!(
            "✓  recorded {}: job {} completed",
            format::sanitize(&exec_id),
            format::sanitize(job_id)
        );
    }

    Ok(0)
}

