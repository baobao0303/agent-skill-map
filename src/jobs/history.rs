//! The execution history surface: `sm history` and `sm history stats`,
//! per `execution-record.schema.json` and `history-stats.schema.json`.

use crate::cli::{Cli, CliError};
use crate::format;
use crate::jobs;
use crate::jsjson;
use crate::scope::Scope;
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

#[derive(Args, Debug, Clone)]
pub struct HistoryArgs {
    #[arg(short = 'n')]
    pub node: Option<String>,
    #[arg(long)]
    pub extension: Option<String>,
    #[arg(long)]
    pub status: Option<String>,
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub until: Option<String>,
    #[arg(long)]
    pub limit: Option<String>,

    #[command(subcommand)]
    pub command: Option<HistorySubcommand>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum HistorySubcommand {
    /// Aggregate counts, tokens, periods, top nodes, and error rates over state_executions.
    Stats {
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        until: Option<String>,
        #[arg(short = 'n', long = "node")]
        node: Option<String>,
        #[arg(long)]
        period: Option<String>,
        #[arg(long)]
        top: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct ExecutionRow {
    pub id: String,
    pub kind: String,
    pub extension_id: String,
    pub extension_version: String,
    pub node_ids: Vec<String>,
    pub content_hash: Option<String>,
    pub status: String,
    pub failure_reason: Option<String>,
    pub exit_code: Option<i64>,
    pub runner: Option<String>,
    pub started_at: i64,
    pub finished_at: i64,
    pub duration_ms: Option<i64>,
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
    pub model: Option<String>,
    pub report_json: Option<String>,
    pub job_id: Option<String>,
}

pub fn parse_iso_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (date_part, time_part) = match s.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => match s.split_once(' ') {
            Some((d, t)) => (d, Some(t)),
            None => (s, None),
        },
    };

    let date_segs: Vec<&str> = date_part.split('-').collect();
    if date_segs.len() != 3 {
        return None;
    }
    let year: i32 = date_segs[0].parse().ok()?;
    let month_num: u8 = date_segs[1].parse().ok()?;
    let day: u8 = date_segs[2].parse().ok()?;
    let month = time::Month::try_from(month_num).ok()?;
    let date = time::Date::from_calendar_date(year, month, day).ok()?;

    let (hour, minute, second, milli) = if let Some(t_str) = time_part {
        let clean_t = t_str.trim_end_matches('Z');
        let time_digits = if let Some((before_plus, _)) = clean_t.split_once('+') {
            before_plus
        } else if let Some((before_minus, _)) = clean_t.split_once('-') {
            before_minus
        } else {
            clean_t
        };

        let (hms_part, ms_part) = match time_digits.split_once('.') {
            Some((hms, ms)) => (hms, Some(ms)),
            None => (time_digits, None),
        };

        let t_segs: Vec<&str> = hms_part.split(':').collect();
        if t_segs.len() < 2 {
            return None;
        }
        let h: u8 = t_segs[0].parse().ok()?;
        let m: u8 = t_segs[1].parse().ok()?;
        let s: u8 = if t_segs.len() >= 3 {
            t_segs[2].parse().ok()?
        } else {
            0
        };

        let milli: u16 = if let Some(ms_str) = ms_part {
            let padded = format!("{:0<3}", ms_str);
            padded[..3].parse().ok()?
        } else {
            0
        };
        (h, m, s, milli)
    } else {
        (0, 0, 0, 0)
    };

    let time = time::Time::from_hms_milli(hour, minute, second, milli).ok()?;
    let dt = date.with_time(time).assume_utc();
    Some((dt.unix_timestamp_nanos() / 1_000_000) as i64)
}

fn format_iso_utc(ms: i64) -> String {
    let t = match time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000) {
        Ok(t) => t,
        Err(_) => time::OffsetDateTime::UNIX_EPOCH,
    };
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

fn bucket_start_ms(ms: i64, period: &str) -> i64 {
    let t = match time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000) {
        Ok(t) => t,
        Err(_) => time::OffsetDateTime::UNIX_EPOCH,
    };
    let year = t.year();
    let month = t.month();
    let day = t.day();

    if period == "month" {
        let date = time::Date::from_calendar_date(year, month, 1).unwrap_or(time::Date::MIN);
        (date.with_time(time::Time::MIDNIGHT).assume_utc().unix_timestamp_nanos() / 1_000_000) as i64
    } else if period == "day" {
        let date = time::Date::from_calendar_date(year, month, day).unwrap_or(time::Date::MIN);
        (date.with_time(time::Time::MIDNIGHT).assume_utc().unix_timestamp_nanos() / 1_000_000) as i64
    } else {
        let weekday_offset = match t.weekday() {
            time::Weekday::Monday => 0,
            time::Weekday::Tuesday => 1,
            time::Weekday::Wednesday => 2,
            time::Weekday::Thursday => 3,
            time::Weekday::Friday => 4,
            time::Weekday::Saturday => 5,
            time::Weekday::Sunday => 6,
        };
        let date = time::Date::from_calendar_date(year, month, day).unwrap_or(time::Date::MIN);
        let monday_date = date - time::Duration::days(weekday_offset);
        (monday_date.with_time(time::Time::MIDNIGHT).assume_utc().unix_timestamp_nanos() / 1_000_000) as i64
    }
}

fn matches_extension(ext_id: &str, target: &str) -> bool {
    if ext_id == target {
        return true;
    }
    if let Some((_, bare)) = ext_id.split_once('/') {
        if bare == target {
            return true;
        }
    }
    false
}

pub fn run(cli: &Cli, _scope: &Scope, db_path: &Path, args: &HistoryArgs) -> Result<u8, CliError> {
    if let Some(sub) = &args.command {
        return match sub {
            HistorySubcommand::Stats { since, until, node, period, top } => {
                run_stats(cli, db_path, since.as_deref(), until.as_deref(), node.as_deref(), period.as_deref(), top.as_deref())
            }
        };
    }

    let conn = jobs::open(db_path)?;

    let since_ms = match args.since.as_deref() {
        Some(s) => match parse_iso_ms(s) {
            Some(ms) => Some(ms),
            None => {
                return Err(CliError::Raw(
                    format!(
                        "✕  --since: cannot parse \"{}\" as a date.\n   Pass an ISO date, e.g. 2026-07-01 or 2026-07-01T12:00:00Z.",
                        format::sanitize(s)
                    ),
                    2,
                ));
            }
        },
        None => None,
    };

    let until_ms = match args.until.as_deref() {
        Some(s) => match parse_iso_ms(s) {
            Some(ms) => Some(ms),
            None => {
                return Err(CliError::Raw(
                    format!(
                        "✕  --until: cannot parse \"{}\" as a date.\n   Pass an ISO date, e.g. 2026-07-01 or 2026-07-01T12:00:00Z.",
                        format::sanitize(s)
                    ),
                    2,
                ));
            }
        },
        None => None,
    };

    let status_filter: Option<Vec<String>> = match args.status.as_deref() {
        Some(s) => {
            let statuses: Vec<String> = s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
            for st in &statuses {
                if st != "completed" && st != "failed" && st != "cancelled" {
                    return Err(CliError::Raw(
                        format!(
                            "✕  --status: invalid status \"{}\".\n   Allowed: completed, failed, cancelled.",
                            format::sanitize(st)
                        ),
                        2,
                    ));
                }
            }
            Some(statuses)
        }
        None => None,
    };

    let limit: Option<usize> = match args.limit.as_deref() {
        Some(raw) => match raw.trim().parse::<usize>() {
            Ok(v) if v > 0 => Some(v),
            _ => {
                return Err(CliError::Raw(
                    format!(
                        "✕  --limit: must be a positive integer, got \"{}\".",
                        format::sanitize(raw)
                    ),
                    2,
                ));
            }
        },
        None => None,
    };

    let mut stmt = conn
        .prepare(
            "SELECT
                id, kind, extension_id, extension_version, node_ids_json, content_hash,
                status, failure_reason, exit_code, runner, started_at, finished_at,
                duration_ms, tokens_in, tokens_out, model, report_json, job_id
            FROM state_executions
            ORDER BY started_at DESC, id DESC",
        )
        .map_err(|e| CliError::Operational(e.to_string()))?;

    let rows = stmt
        .query_map([], |r| {
            let node_ids_json: String = r.get(4)?;
            let node_ids: Vec<String> = serde_json::from_str(&node_ids_json).unwrap_or_default();
            Ok(ExecutionRow {
                id: r.get(0)?,
                kind: r.get(1)?,
                extension_id: r.get(2)?,
                extension_version: r.get(3)?,
                node_ids,
                content_hash: r.get(5)?,
                status: r.get(6)?,
                failure_reason: r.get(7)?,
                exit_code: r.get(8)?,
                runner: r.get(9)?,
                started_at: r.get(10)?,
                finished_at: r.get(11)?,
                duration_ms: r.get(12)?,
                tokens_in: r.get(13)?,
                tokens_out: r.get(14)?,
                model: r.get(15)?,
                report_json: r.get(16)?,
                job_id: r.get(17)?,
            })
        })
        .map_err(|e| CliError::Operational(e.to_string()))?;

    let mut filtered = Vec::new();
    for row in rows.filter_map(Result::ok) {
        if let Some(target_node) = &args.node {
            if !row.node_ids.contains(target_node) {
                continue;
            }
        }
        if let Some(target_ext) = &args.extension {
            if !matches_extension(&row.extension_id, target_ext) {
                continue;
            }
        }
        if let Some(statuses) = &status_filter {
            if !statuses.contains(&row.status) {
                continue;
            }
        }
        if let Some(since) = since_ms {
            if row.started_at < since {
                continue;
            }
        }
        if let Some(until) = until_ms {
            if row.started_at >= until {
                continue;
            }
        }

        filtered.push(row);
        if let Some(l) = limit {
            if filtered.len() >= l {
                break;
            }
        }
    }

    if cli.global.json {
        let json_arr: Vec<Value> = filtered
            .into_iter()
            .map(|r| {
                json!({
                    "id": r.id,
                    "kind": r.kind,
                    "extensionId": r.extension_id,
                    "extensionVersion": r.extension_version,
                    "nodeIds": r.node_ids,
                    "contentHash": r.content_hash,
                    "status": r.status,
                    "failureReason": r.failure_reason,
                    "exitCode": r.exit_code,
                    "runner": r.runner,
                    "startedAt": r.started_at,
                    "finishedAt": r.finished_at,
                    "durationMs": r.duration_ms,
                    "tokensIn": r.tokens_in,
                    "tokensOut": r.tokens_out,
                    "model": r.model,
                    "reportPath": r.report_json,
                    "jobId": r.job_id,
                })
            })
            .collect();

        println!("{}", jsjson::to_string(&json_arr));
        return Ok(0);
    }

    if filtered.is_empty() {
        println!("No execution records found.");
        return Ok(0);
    }

    println!("  ID                          STARTED              EXTENSION                     STATUS     DURATION    TOKENS  NODES");
    for r in &filtered {
        let started_str = format_iso_utc(r.started_at).chars().take(19).collect::<String>().replace('T', " ") + "Z";
        let tokens_str = format!("{}/{}", r.tokens_in.unwrap_or(0), r.tokens_out.unwrap_or(0));
        let duration_str = r.duration_ms.map(|d| format!("{d}ms")).unwrap_or_else(|| "-".to_string());
        let status_str = match &r.failure_reason {
            Some(reason) if !reason.is_empty() => format!("{} ({})", r.status, reason),
            _ => r.status.clone(),
        };

        println!(
            "  {:<26}  {:<19}  {:<28}  {:<10}  {:>8}  {:>8}  {:>5}",
            format::sanitize(&r.id),
            started_str,
            format::sanitize(&r.extension_id),
            format::sanitize(&status_str),
            duration_str,
            tokens_str,
            r.node_ids.len()
        );
    }

    let noun = if filtered.len() == 1 { "execution" } else { "executions" };
    println!("\n  {} {noun}\n  Tip: pass -n <path> to restrict to one node, or --json for machine-readable output.", filtered.len());
    Ok(0)
}

fn run_stats(
    cli: &Cli,
    db_path: &Path,
    since_str: Option<&str>,
    until_str: Option<&str>,
    node_filter: Option<&str>,
    period_str: Option<&str>,
    top_str: Option<&str>,
) -> Result<u8, CliError> {
    let started = std::time::Instant::now();
    let conn = jobs::open(db_path)?;

    let since_ms = match since_str {
        Some(s) => match parse_iso_ms(s) {
            Some(ms) => Some(ms),
            None => {
                return Err(CliError::Raw(
                    format!(
                        "✕  --since: cannot parse \"{}\" as a date.\n   Pass an ISO date, e.g. 2026-07-01 or 2026-07-01T12:00:00Z.",
                        format::sanitize(s)
                    ),
                    2,
                ));
            }
        },
        None => None,
    };

    let until_ms = match until_str {
        Some(s) => match parse_iso_ms(s) {
            Some(ms) => ms,
            None => {
                return Err(CliError::Raw(
                    format!(
                        "✕  --until: cannot parse \"{}\" as a date.\n   Pass an ISO date, e.g. 2026-07-01 or 2026-07-01T12:00:00Z.",
                        format::sanitize(s)
                    ),
                    2,
                ));
            }
        },
        None => jobs::now_ms(),
    };

    let period = match period_str {
        Some("day") => "day",
        Some("week") => "week",
        Some("month") => "month",
        Some(other) => {
            return Err(CliError::Raw(
                format!(
                    "✕  --period: invalid value \"{}\".\n   Allowed: day, week, month.",
                    format::sanitize(other)
                ),
                2,
            ));
        }
        None => "month",
    };

    let top_n: usize = match top_str {
        Some(raw) => match raw.trim().parse::<usize>() {
            Ok(v) if v > 0 => v,
            _ => {
                return Err(CliError::Raw(
                    format!(
                        "✕  --top: must be a positive integer, got \"{}\".",
                        format::sanitize(raw)
                    ),
                    2,
                ));
            }
        },
        None => 10,
    };

    let mut stmt = conn
        .prepare(
            "SELECT
                id, kind, extension_id, extension_version, node_ids_json, content_hash,
                status, failure_reason, exit_code, runner, started_at, finished_at,
                duration_ms, tokens_in, tokens_out, model, report_json, job_id
            FROM state_executions",
        )
        .map_err(|e| CliError::Operational(e.to_string()))?;

    let rows = stmt
        .query_map([], |r| {
            let node_ids_json: String = r.get(4)?;
            let node_ids: Vec<String> = serde_json::from_str(&node_ids_json).unwrap_or_default();
            Ok(ExecutionRow {
                id: r.get(0)?,
                kind: r.get(1)?,
                extension_id: r.get(2)?,
                extension_version: r.get(3)?,
                node_ids,
                content_hash: r.get(5)?,
                status: r.get(6)?,
                failure_reason: r.get(7)?,
                exit_code: r.get(8)?,
                runner: r.get(9)?,
                started_at: r.get(10)?,
                finished_at: r.get(11)?,
                duration_ms: r.get(12)?,
                tokens_in: r.get(13)?,
                tokens_out: r.get(14)?,
                model: r.get(15)?,
                report_json: r.get(16)?,
                job_id: r.get(17)?,
            })
        })
        .map_err(|e| CliError::Operational(e.to_string()))?;

    let mut executions_count = 0;
    let mut completed_count = 0;
    let mut failed_count = 0;
    let mut tokens_in_total = 0;
    let mut tokens_out_total = 0;
    let mut duration_ms_total = 0;

    struct ExtAcc {
        extension_id: String,
        extension_version: String,
        executions_count: usize,
        tokens_in: i64,
        tokens_out: i64,
        durations: Vec<i64>,
        failed_count: usize,
    }

    struct PeriodAcc {
        period_start: i64,
        executions_count: usize,
        tokens_in: i64,
        tokens_out: i64,
    }

    struct NodeAcc {
        node_path: String,
        executions_count: usize,
        last_executed_at: i64,
    }

    let mut per_extension: HashMap<String, ExtAcc> = HashMap::new();
    let mut per_period: HashMap<i64, PeriodAcc> = HashMap::new();
    let mut per_node: HashMap<String, NodeAcc> = HashMap::new();

    let mut per_failure_reason: HashMap<String, usize> = HashMap::new();
    for reason in &["runner-error", "report-invalid", "timeout", "abandoned", "job-file-missing", "user-failed"] {
        per_failure_reason.insert(reason.to_string(), 0);
    }

    for row in rows.filter_map(Result::ok) {
        if let Some(since) = since_ms {
            if row.started_at < since {
                continue;
            }
        }
        if row.started_at >= until_ms {
            continue;
        }
        if let Some(target_node) = node_filter {
            if !row.node_ids.contains(&target_node.to_string()) {
                continue;
            }
        }

        executions_count += 1;
        let t_in = row.tokens_in.unwrap_or(0);
        let t_out = row.tokens_out.unwrap_or(0);
        tokens_in_total += t_in;
        tokens_out_total += t_out;
        if let Some(d) = row.duration_ms {
            duration_ms_total += d;
        }

        if row.status == "completed" {
            completed_count += 1;
        } else if row.status == "failed" {
            failed_count += 1;
        }

        if let Some(reason) = &row.failure_reason {
            if let Some(entry) = per_failure_reason.get_mut(reason) {
                *entry += 1;
            }
        }

        let ext_key = format!("{}@{}", row.extension_id, row.extension_version);
        let ext_acc = per_extension.entry(ext_key).or_insert_with(|| ExtAcc {
            extension_id: row.extension_id.clone(),
            extension_version: row.extension_version.clone(),
            executions_count: 0,
            tokens_in: 0,
            tokens_out: 0,
            durations: Vec::new(),
            failed_count: 0,
        });
        ext_acc.executions_count += 1;
        ext_acc.tokens_in += t_in;
        ext_acc.tokens_out += t_out;
        if let Some(d) = row.duration_ms {
            ext_acc.durations.push(d);
        }
        if row.status == "failed" {
            ext_acc.failed_count += 1;
        }

        let p_start = bucket_start_ms(row.started_at, period);
        let period_acc = per_period.entry(p_start).or_insert_with(|| PeriodAcc {
            period_start: p_start,
            executions_count: 0,
            tokens_in: 0,
            tokens_out: 0,
        });
        period_acc.executions_count += 1;
        period_acc.tokens_in += t_in;
        period_acc.tokens_out += t_out;

        for node in &row.node_ids {
            let node_acc = per_node.entry(node.clone()).or_insert_with(|| NodeAcc {
                node_path: node.clone(),
                executions_count: 0,
                last_executed_at: 0,
            });
            node_acc.executions_count += 1;
            if row.started_at > node_acc.last_executed_at {
                node_acc.last_executed_at = row.started_at;
            }
        }
    }

    let mut tokens_per_extension: Vec<Value> = per_extension
        .values()
        .map(|acc| {
            let (mean, median) = if acc.durations.is_empty() {
                (None, None)
            } else {
                let sum: i64 = acc.durations.iter().sum();
                let mean_val = (sum as f64 / acc.durations.len() as f64).round() as i64;
                let mut sorted = acc.durations.clone();
                sorted.sort();
                let mid = sorted.len() / 2;
                let median_val = if sorted.len() % 2 == 1 {
                    sorted[mid]
                } else {
                    ((sorted[mid - 1] + sorted[mid]) as f64 / 2.0).round() as i64
                };
                (Some(mean_val), Some(median_val))
            };

            json!({
                "extensionId": acc.extension_id,
                "extensionVersion": acc.extension_version,
                "executionsCount": acc.executions_count,
                "tokensIn": acc.tokens_in,
                "tokensOut": acc.tokens_out,
                "durationMsMean": mean,
                "durationMsMedian": median,
            })
        })
        .collect();

    tokens_per_extension.sort_by(|a, b| {
        let sum_a = a["tokensIn"].as_i64().unwrap_or(0) + a["tokensOut"].as_i64().unwrap_or(0);
        let sum_b = b["tokensIn"].as_i64().unwrap_or(0) + b["tokensOut"].as_i64().unwrap_or(0);
        sum_b.cmp(&sum_a)
    });

    let mut sorted_periods: Vec<&PeriodAcc> = per_period.values().collect();
    sorted_periods.sort_by_key(|p| p.period_start);

    let executions_per_period: Vec<Value> = sorted_periods
        .into_iter()
        .map(|acc| {
            json!({
                "periodStart": format_iso_utc(acc.period_start),
                "periodUnit": period,
                "executionsCount": acc.executions_count,
                "tokensIn": acc.tokens_in,
                "tokensOut": acc.tokens_out,
            })
        })
        .collect();

    let mut sorted_nodes: Vec<&NodeAcc> = per_node.values().collect();
    sorted_nodes.sort_by(|a, b| {
        if b.executions_count != a.executions_count {
            b.executions_count.cmp(&a.executions_count)
        } else {
            b.last_executed_at.cmp(&a.last_executed_at)
        }
    });

    let top_nodes: Vec<Value> = sorted_nodes
        .into_iter()
        .take(top_n)
        .map(|acc| {
            json!({
                "nodePath": acc.node_path,
                "executionsCount": acc.executions_count,
                "lastExecutedAt": acc.last_executed_at,
            })
        })
        .collect();

    let mut per_extension_rates: Vec<Value> = per_extension
        .values()
        .map(|acc| {
            let rate = if acc.executions_count == 0 {
                0.0
            } else {
                acc.failed_count as f64 / acc.executions_count as f64
            };
            json!({
                "extensionId": acc.extension_id,
                "rate": rate,
                "executionsCount": acc.executions_count,
                "failedCount": acc.failed_count,
            })
        })
        .collect();

    per_extension_rates.sort_by(|a, b| {
        let r_a = a["rate"].as_f64().unwrap_or(0.0);
        let r_b = b["rate"].as_f64().unwrap_or(0.0);
        r_b.partial_cmp(&r_a).unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a["extensionId"].as_str().unwrap_or("").cmp(b["extensionId"].as_str().unwrap_or("")))
    });

    let global_rate = if executions_count == 0 {
        0.0
    } else {
        failed_count as f64 / executions_count as f64
    };

    let elapsed_ms = started.elapsed().as_millis() as u64;

    let stats = json!({
        "schemaVersion": 1,
        "range": {
            "since": since_ms.map(format_iso_utc),
            "until": format_iso_utc(until_ms),
        },
        "totals": {
            "executionsCount": executions_count,
            "completedCount": completed_count,
            "failedCount": failed_count,
            "tokensIn": tokens_in_total,
            "tokensOut": tokens_out_total,
            "durationMsTotal": duration_ms_total,
        },
        "tokensPerExtension": tokens_per_extension,
        "executionsPerPeriod": executions_per_period,
        "topNodes": top_nodes,
        "errorRates": {
            "global": global_rate,
            "perExtension": per_extension_rates,
            "perFailureReason": {
                "runner-error": per_failure_reason.get("runner-error").copied().unwrap_or(0),
                "report-invalid": per_failure_reason.get("report-invalid").copied().unwrap_or(0),
                "timeout": per_failure_reason.get("timeout").copied().unwrap_or(0),
                "abandoned": per_failure_reason.get("abandoned").copied().unwrap_or(0),
                "job-file-missing": per_failure_reason.get("job-file-missing").copied().unwrap_or(0),
                "user-failed": per_failure_reason.get("user-failed").copied().unwrap_or(0),
            },
        },
        "elapsedMs": elapsed_ms,
    });

    if cli.global.json {
        println!("{}", jsjson::to_string(&stats));
        return Ok(0);
    }

    let error_pct = format!("{:.1}", global_rate * 100.0);
    println!("sm history stats: {executions_count} executions · {failed_count} failed · {error_pct}% error rate\n");
    let since_display = since_ms.map(format_iso_utc).unwrap_or_else(|| "all-time".to_string());
    println!("  Window: {since_display} to {}", format_iso_utc(until_ms));
    println!("  Totals: {completed_count} completed, {failed_count} failed, {tokens_in_total}/{tokens_out_total} tokens");

    Ok(0)
}
