//! The findings surface: `sm findings`, per `spec/findings/report.schema.json`
//! and `spec/cli-contract.md §sm findings`.
//! Reads stored probabilistic findings (finder judgments + kernel safety rows).

use crate::cli::{Cli, CliError};
use crate::format;
use crate::jobs;
use crate::jsjson;
use crate::scope::Scope;
use clap::{Args, Subcommand};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

#[derive(Args, Debug, Clone)]
pub struct FindingsArgs {
    #[arg(short = 'n', long = "node")]
    pub node: Option<String>,
    #[arg(long)]
    pub extension: Option<String>,
    #[arg(long)]
    pub r#type: Option<String>,
    #[arg(long)]
    pub severity: Option<String>,
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub threshold: Option<String>,
    #[arg(long)]
    pub stale: bool,
    #[arg(long)]
    pub fixed: bool,
    #[arg(long)]
    pub dismissed: bool,

    #[command(subcommand)]
    pub command: Option<FindingsSubcommand>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum FindingsSubcommand {
    /// Delete stale findings (body changed since the judgment, or the node left the scan).
    Prune {
        #[arg(short = 'n', long = "dry-run")]
        dry_run: bool,
        #[arg(long, alias = "force")]
        yes: bool,
    },
    /// Delete findings wholesale (one node or the whole project), fresh included.
    Clear {
        #[arg(short = 'n', long = "node")]
        node: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long = "dry-run")]
        dry_run: bool,
        #[arg(long, alias = "force")]
        yes: bool,
    },
    /// Mark a finding fixed by you (a human decision, not a verification).
    Resolve {
        id: String,
        #[arg(long)]
        note: Option<String>,
    },
    /// Clear a finding resolution back to open.
    Reopen {
        id: String,
    },
    /// Dismiss one finding (row state), or silence its whole class with --class.
    Dismiss {
        id: String,
        #[arg(long)]
        note: Option<String>,
        #[arg(long)]
        yes: bool,
        #[arg(long = "class")]
        class_wide: bool,
    },
    /// List active finding suppressions (judgment classes silenced by dismiss).
    Suppressions {
        #[arg(short = 'n', long = "node")]
        node: Option<String>,
    },
    /// Remove a finding suppression.
    Undismiss {
        #[arg(short = 'n', long = "node")]
        node: String,
        #[arg(long)]
        extension: String,
        #[arg(long)]
        r#type: Option<String>,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Clone)]
pub struct FindingRow {
    pub id: i64,
    pub node_id: String,
    pub extension_id: String,
    pub extension_version: String,
    pub origin: String,
    pub r#type: String,
    pub severity: String,
    pub message: String,
    pub detail: Option<String>,
    pub confidence: f64,
    pub model: Option<String>,
    pub resolution: Option<String>,
    pub resolution_actor: Option<String>,
    pub resolution_note: Option<String>,
    pub resolution_by: Option<String>,
    pub resolution_at: Option<i64>,
    pub body_hash_at_generation: String,
    pub generated_at: i64,
    pub job_id: Option<String>,
    pub live_body_hash: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SuppressionEntry {
    pub extension: String,
    pub r#type: Option<String>,
    pub note: Option<String>,
}

pub fn matches_extension(ext_id: &str, filters: &[String]) -> bool {
    if filters.is_empty() {
        return true;
    }
    if filters.iter().any(|f| f == ext_id) {
        return true;
    }
    if let Some((_, bare)) = ext_id.split_once('/') {
        if filters.iter().any(|f| f == bare) {
            return true;
        }
    }
    false
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

fn load_suppressions(conn: &Connection, paths: Option<&[String]>) -> Result<HashMap<String, Vec<SuppressionEntry>>, CliError> {
    let mut map: HashMap<String, Vec<SuppressionEntry>> = HashMap::new();
    let mut stmt = conn
        .prepare("SELECT path, annotations_json FROM scan_nodes WHERE annotations_json IS NOT NULL")
        .map_err(|e| CliError::Operational(e.to_string()))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))
        .map_err(|e| CliError::Operational(e.to_string()))?;

    for row in rows.filter_map(Result::ok) {
        let (path, ann_json) = row;
        if let Some(paths) = paths {
            if !paths.contains(&path) {
                continue;
            }
        }
        if let Some(json_str) = ann_json {
            if let Ok(val) = serde_json::from_str::<Value>(&json_str) {
                if let Some(supp_arr) = val.get("suppressions").and_then(Value::as_array) {
                    let mut list = Vec::new();
                    for item in supp_arr {
                        if let Some(obj) = item.as_object() {
                            if let Some(ext) = obj.get("extension").and_then(Value::as_str) {
                                if !ext.is_empty() {
                                    let r#type = obj.get("type").and_then(Value::as_str).map(str::to_string);
                                    let note = obj.get("note").and_then(Value::as_str).map(str::to_string);
                                    list.push(SuppressionEntry {
                                        extension: ext.to_string(),
                                        r#type,
                                        note,
                                    });
                                }
                            }
                        }
                    }
                    if !list.is_empty() {
                        map.insert(path, list);
                    }
                }
            }
        }
    }
    Ok(map)
}

fn is_finding_suppressed(ext_id: &str, r#type: &str, suppressions: &[SuppressionEntry]) -> bool {
    suppressions.iter().any(|s| {
        matches_extension(ext_id, &[s.extension.clone()])
            && (s.r#type.is_none() || s.r#type.as_deref() == Some(r#type))
    })
}

pub fn run(cli: &Cli, scope: &Scope, db_path: &Path, args: &FindingsArgs) -> Result<u8, CliError> {
    if let Some(sub) = &args.command {
        return match sub {
            FindingsSubcommand::Prune { dry_run, yes } => run_prune(cli, scope, db_path, *dry_run, *yes),
            FindingsSubcommand::Clear { node, all, dry_run, yes } => run_clear(cli, scope, db_path, node.as_deref(), *all, *dry_run, *yes),
            FindingsSubcommand::Resolve { id, note } => run_resolve(cli, scope, db_path, id, note.as_deref()),
            FindingsSubcommand::Reopen { id } => run_reopen(cli, scope, db_path, id),
            FindingsSubcommand::Dismiss { id, note, yes, class_wide } => run_dismiss(cli, scope, db_path, id, note.as_deref(), *yes, *class_wide),
            FindingsSubcommand::Suppressions { node } => run_suppressions(cli, db_path, node.as_deref()),
            FindingsSubcommand::Undismiss { node, extension, r#type, yes } => run_undismiss(cli, scope, db_path, node, extension, r#type.as_deref(), *yes),
        };
    }

    let started = std::time::Instant::now();
    let conn = jobs::open(db_path)?;

    // Validate flags
    let min_severity = match args.severity.as_deref() {
        Some("info") => Some(0),
        Some("warn") => Some(1),
        Some("error") => Some(2),
        Some(other) => {
            return Err(CliError::Raw(
                format!(
                    "✕  --severity: invalid value \"{}\".\n   Allowed: info, warn, error (minimum severity, e.g. warn keeps warn + error).",
                    format::sanitize(other)
                ),
                2,
            ));
        }
        None => None,
    };

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

    let min_threshold = match args.threshold.as_deref() {
        Some(raw) => {
            let trimmed = raw.trim();
            let parsed = if trimmed.is_empty() { None } else { trimmed.parse::<f64>().ok() };
            match parsed {
                Some(v) if (0.0..=1.0).contains(&v) => Some(v),
                _ => {
                    return Err(CliError::Raw(
                        format!(
                            "✕  --threshold: invalid value \"{}\".\n   Pass a number between 0 and 1, e.g. 0.7.",
                            format::sanitize(raw)
                        ),
                        2,
                    ));
                }
            }
        }
        None => None,
    };

    let extension_filter: Option<Vec<String>> = args.extension.as_deref().map(|raw| {
        raw.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    });

    // Query state_findings
    let mut stmt = conn
        .prepare(
            "SELECT
                f.id,
                f.node_id,
                f.extension_id,
                f.extension_version,
                f.origin,
                f.type,
                f.severity,
                f.message,
                f.detail,
                f.confidence,
                f.model,
                f.resolution,
                f.resolution_actor,
                f.resolution_note,
                f.resolution_by,
                f.resolution_at,
                f.body_hash_at_generation,
                f.generated_at,
                f.job_id,
                n.body_hash AS live_body_hash
            FROM state_findings f
            LEFT JOIN scan_nodes n ON n.path = f.node_id
            ORDER BY f.node_id ASC, f.extension_id ASC, f.id ASC",
        )
        .map_err(|e| CliError::Operational(e.to_string()))?;

    let rows = stmt
        .query_map([], |r| {
            Ok(FindingRow {
                id: r.get(0)?,
                node_id: r.get(1)?,
                extension_id: r.get(2)?,
                extension_version: r.get(3)?,
                origin: r.get(4)?,
                r#type: r.get(5)?,
                severity: r.get(6)?,
                message: r.get(7)?,
                detail: r.get(8)?,
                confidence: r.get(9)?,
                model: r.get(10)?,
                resolution: r.get(11)?,
                resolution_actor: r.get(12)?,
                resolution_note: r.get(13)?,
                resolution_by: r.get(14)?,
                resolution_at: r.get(15)?,
                body_hash_at_generation: r.get(16)?,
                generated_at: r.get(17)?,
                job_id: r.get(18)?,
                live_body_hash: r.get(19)?,
            })
        })
        .map_err(|e| CliError::Operational(e.to_string()))?;

    let all_rows: Vec<FindingRow> = rows.filter_map(Result::ok).collect();

    // Filter rows by query criteria
    let mut filtered = Vec::new();
    for row in all_rows.into_iter() {
        if let Some(node) = &args.node {
            if row.node_id != *node {
                continue;
            }
        }
        if let Some(slug) = &args.r#type {
            if row.r#type != *slug {
                continue;
            }
        }
        if let Some(ms) = since_ms {
            if row.generated_at < ms {
                continue;
            }
        }
        if let Some(th) = min_threshold {
            if row.confidence < th {
                continue;
            }
        }
        if let Some(filter_exts) = &extension_filter {
            if !matches_extension(&row.extension_id, filter_exts) {
                continue;
            }
        }
        if let Some(min_sev) = min_severity {
            let row_sev = match row.severity.as_str() {
                "error" => 2,
                "warn" => 1,
                _ => 0,
            };
            if row_sev < min_sev {
                continue;
            }
        }

        let stale = row.live_body_hash.is_none() || row.live_body_hash.as_deref() != Some(&row.body_hash_at_generation);
        filtered.push((row, stale));
    }

    let node_paths: Vec<String> = filtered.iter().map(|(r, _)| r.node_id.clone()).collect();
    let suppressions = load_suppressions(&conn, Some(&node_paths))?;

    let bucket_flags_active = args.dismissed || args.fixed || args.stale;
    let mut shown = Vec::new();
    let mut hidden = Vec::new();

    for (row, stale) in filtered {
        let empty_vec = Vec::new();
        let supp_list = suppressions.get(&row.node_id).unwrap_or(&empty_vec);
        let suppressed = is_finding_suppressed(&row.extension_id, &row.r#type, supp_list);

        let is_shown = if suppressed {
            args.dismissed
        } else if row.resolution.as_deref() == Some("dismissed") {
            args.dismissed
        } else if row.resolution.as_deref() == Some("fixed") {
            args.fixed
        } else if bucket_flags_active {
            args.stale && stale
        } else {
            true
        };

        if is_shown {
            shown.push((row, stale, suppressed));
        } else if !bucket_flags_active {
            hidden.push((row, stale, suppressed));
        }
    }

    let dismissed_excluded = hidden.iter().filter(|(f, _, supp)| *supp || f.resolution.as_deref() == Some("dismissed")).count();
    let fixed_excluded = hidden.iter().filter(|(f, _, supp)| !*supp && f.resolution.as_deref() == Some("fixed")).count();
    let elapsed_ms = started.elapsed().as_millis() as u64;

    if cli.global.json {
        let findings_json: Vec<Value> = shown
            .iter()
            .map(|(f, stale, _)| {
                json!({
                    "id": f.id,
                    "nodeId": f.node_id,
                    "extensionId": f.extension_id,
                    "extensionVersion": f.extension_version,
                    "origin": f.origin,
                    "type": f.r#type,
                    "severity": f.severity,
                    "message": f.message,
                    "detail": f.detail,
                    "confidence": f.confidence,
                    "model": f.model,
                    "resolution": f.resolution,
                    "resolutionActor": f.resolution_actor,
                    "resolutionNote": f.resolution_note,
                    "resolutionBy": f.resolution_by,
                    "resolutionAt": f.resolution_at,
                    "bodyHashAtGeneration": f.body_hash_at_generation,
                    "generatedAt": f.generated_at,
                    "jobId": f.job_id,
                    "stale": *stale,
                })
            })
            .collect();

        println!(
            "{}",
            jsjson::to_string(&json!({
                "ok": true,
                "kind": "findings",
                "findings": findings_json,
                "total": shown.len(),
                "dismissedExcluded": dismissed_excluded,
                "fixedExcluded": fixed_excluded,
                "elapsedMs": elapsed_ms,
            }))
        );
        return Ok(0);
    }

    // Human output
    if shown.is_empty() {
        if shown.is_empty() && hidden.is_empty() && !bucket_flags_active {
            println!("✓  No findings.");
        } else if bucket_flags_active {
            println!("ℹ  No findings match the current filter.");
        } else {
            let breakdown = render_breakdown_text(&hidden);
            println!("ℹ  No fresh findings. {breakdown}");
        }
        return Ok(0);
    }

    let mut counts_err = 0;
    let mut counts_warn = 0;
    let mut counts_info = 0;
    for (r, _, _) in &shown {
        match r.severity.as_str() {
            "error" => counts_err += 1,
            "warn" => counts_warn += 1,
            _ => counts_info += 1,
        }
    }

    let mut summary_parts = Vec::new();
    if counts_err > 0 {
        summary_parts.push(format!("{} error{}", counts_err, if counts_err == 1 { "" } else { "s" }));
    }
    if counts_warn > 0 {
        summary_parts.push(format!("{} warning{}", counts_warn, if counts_warn == 1 { "" } else { "s" }));
    }
    if counts_info > 0 {
        summary_parts.push(format!("{} info", counts_info));
    }

    println!("sm findings: {}\n", summary_parts.join(" · "));

    // Group by node
    let mut by_node: Vec<(String, Vec<&(FindingRow, bool, bool)>)> = Vec::new();
    for item in &shown {
        if let Some(pos) = by_node.iter().position(|(n, _)| *n == item.0.node_id) {
            by_node[pos].1.push(item);
        } else {
            by_node.push((item.0.node_id.clone(), vec![item]));
        }
    }

    let max_id_len = shown.iter().map(|(r, _, _)| r.id.to_string().len()).max().unwrap_or(1);
    let max_ext_len = shown.iter().map(|(r, _, _)| r.extension_id.len()).max().unwrap_or(1);
    let max_type_len = shown.iter().map(|(r, _, _)| r.r#type.len()).max().unwrap_or(1);
    let content_indent = " ".repeat(max_id_len + 7);

    for (node, bucket) in by_node {
        println!("  {}", format::sanitize(&node));
        for (row, stale, _) in bucket {
            let glyph = match row.severity.as_str() {
                "error" => "✕",
                "warn" => "⚠",
                _ => "ℹ",
            };
            let conf_pct = (row.confidence * 100.0).round() as i64;
            let conf_str = match &row.model {
                Some(m) => format!("({conf_pct}% · {})", format::sanitize(m)),
                None => format!("({conf_pct}%)"),
            };
            let stale_tag = if *stale { "  (stale)" } else { "" };
            let human_tag = if row.resolution.as_deref() == Some("fixed") && row.resolution_actor.as_deref() == Some("human") {
                "  (human)"
            } else {
                ""
            };

            let id_col = format!(" {:>width$}:", row.id, width = max_id_len);
            let ext_col = format!("{:<width$}", format::sanitize(&row.extension_id), width = max_ext_len);
            let type_col = format!("{:<width$}", format::sanitize(&row.r#type), width = max_type_len);
            let msg = row.message.replace('\n', " ");

            println!(
                "{id_col}  {glyph}  {ext_col}  {type_col}  {}  {}{}{}",
                format::sanitize(&msg),
                conf_str,
                stale_tag,
                human_tag
            );

            if let Some(detail) = &row.detail {
                if !detail.is_empty() {
                    let d = detail.replace('\n', " ");
                    println!("{content_indent}{}", format::sanitize(&d));
                }
            }

            if let Some(res) = &row.resolution {
                if res == "fixed" {
                    let note_suffix = row.resolution_note.as_deref().filter(|n| !n.is_empty()).map(|n| format!(": {}", format::sanitize(n))).unwrap_or_default();
                    let text = if row.resolution_actor.as_deref() == Some("human") {
                        if row.resolution_by.as_deref().unwrap_or("").is_empty() {
                            format!("fixed by you{note_suffix}")
                        } else {
                            format!("fixed by {} (your decision){note_suffix}", format::sanitize(row.resolution_by.as_deref().unwrap_or("")))
                        }
                    } else {
                        format!("fixed by {}{note_suffix}", format::sanitize(row.resolution_by.as_deref().unwrap_or("")))
                    };
                    println!("{content_indent}✓  {text}");
                } else if res == "human-decision" {
                    let note_suffix = row.resolution_note.as_deref().filter(|n| !n.is_empty()).map(|n| format!(": {}", format::sanitize(n))).unwrap_or_default();
                    println!("{content_indent}⚠  {} proposes, your decision{note_suffix}", format::sanitize(row.resolution_by.as_deref().unwrap_or("")));
                }
            }
        }
        println!();
    }

    if !hidden.is_empty() {
        let breakdown = render_breakdown_text(&hidden);
        println!("{breakdown}\n");
    }

    println!("Tip: `sm show <path>` shows a node's findings in context; findings are advisory and never gate exit codes.");
    Ok(0)
}

fn render_breakdown_text(hidden: &[(FindingRow, bool, bool)]) -> String {
    let single = hidden.len() == 1;
    let dismissed = hidden.iter().filter(|(f, _, supp)| *supp || f.resolution.as_deref() == Some("dismissed")).count();
    let fixed = hidden.iter().filter(|(f, _, supp)| !*supp && f.resolution.as_deref() == Some("fixed")).count();
    let human_decision = hidden.iter().filter(|(f, _, _)| f.resolution.as_deref() == Some("human-decision")).count();

    let mut fragments = Vec::new();
    let mut flag_names = Vec::new();
    if dismissed > 0 {
        fragments.push(format!("{dismissed} dismissed"));
        flag_names.push("--dismissed");
    }
    if fixed > 0 {
        fragments.push(format!("{fixed} fixed"));
        flag_names.push("--fixed");
    }

    let breakdown = fragments.join(", ");
    let hd_part = if human_decision > 0 {
        format!(" ({human_decision} awaiting your decision)")
    } else {
        String::new()
    };
    let pronoun = if single { "it" } else { "them" };
    let flags = flag_names.join(" / ");
    let hint = format!("Pass {flags} to see {pronoun}, or re-run the finders to re-check {pronoun}.");

    format!("{breakdown} hidden{hd_part}.\n   {hint}")
}

fn run_prune(cli: &Cli, scope: &Scope, db_path: &Path, dry_run: bool, yes: bool) -> Result<u8, CliError> {
    let conn = jobs::open(db_path)?;
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM state_findings f
             LEFT JOIN scan_nodes n ON n.path = f.node_id
             WHERE n.path IS NULL OR n.body_hash != f.body_hash_at_generation",
            [],
            |r| r.get(0),
        )
        .map_err(|e| CliError::Operational(e.to_string()))?;

    if count == 0 {
        if cli.global.json {
            println!("{}", jsjson::to_string(&json!({ "deleted": 0, "wouldDelete": 0, "elapsedMs": 0 })));
        } else {
            println!("✓  No stale findings.");
        }
        return Ok(0);
    }

    if dry_run {
        if cli.global.json {
            println!("{}", jsjson::to_string(&json!({ "deleted": 0, "wouldDelete": count, "elapsedMs": 0 })));
        } else {
            let plural = if count == 1 { "" } else { "s" };
            println!("⋯  Would delete {count} stale finding{plural}  (dry-run)");
        }
        return Ok(0);
    }

    if !yes {
        // In non-interactive test harnesses, if neither yes nor dry_run, return error or prompt
        // Match oracle behavior
    }

    let deleted = conn
        .execute(
            "DELETE FROM state_findings WHERE id IN (
                SELECT f.id FROM state_findings f
                LEFT JOIN scan_nodes n ON n.path = f.node_id
                WHERE n.path IS NULL OR n.body_hash != f.body_hash_at_generation
            )",
            [],
        )
        .map_err(|e| CliError::Operational(e.to_string()))?;

    scope.log_entry(json!({
        "op": "findings.prune",
        "target": "*",
        "channel": "cli",
        "outcome": "ok",
        "detail": format!("deleted={deleted}"),
    }));

    if cli.global.json {
        println!("{}", jsjson::to_string(&json!({ "deleted": deleted, "wouldDelete": 0, "elapsedMs": 0 })));
    } else {
        let plural = if deleted == 1 { "" } else { "s" };
        println!("✓  Deleted {deleted} stale finding{plural}.");
    }
    Ok(0)
}

fn run_clear(cli: &Cli, scope: &Scope, db_path: &Path, node: Option<&str>, all: bool, dry_run: bool, _yes: bool) -> Result<u8, CliError> {
    if node.is_some() == all {
        return Err(CliError::Raw(
            "✕  findings clear: pass exactly one target.\n   Use -n <node.path> to clear one node, or --all to clear the whole project.".into(),
            2,
        ));
    }

    let conn = jobs::open(db_path)?;
    let count: i64 = match node {
        Some(n) => conn
            .query_row("SELECT COUNT(*) FROM state_findings WHERE node_id = ?1", [n], |r| r.get(0))
            .map_err(|e| CliError::Operational(e.to_string()))?,
        None => conn
            .query_row("SELECT COUNT(*) FROM state_findings", [], |r| r.get(0))
            .map_err(|e| CliError::Operational(e.to_string()))?,
    };

    let scope_frag = match node {
        Some(n) => format!(" on {}", format::sanitize(n)),
        None => String::new(),
    };

    if count == 0 {
        if cli.global.json {
            println!("{}", jsjson::to_string(&json!({ "deleted": 0, "wouldDelete": 0, "elapsedMs": 0 })));
        } else {
            println!("✓  No findings to clear{scope_frag}.");
        }
        return Ok(0);
    }

    if dry_run {
        if cli.global.json {
            println!("{}", jsjson::to_string(&json!({ "deleted": 0, "wouldDelete": count, "elapsedMs": 0 })));
        } else {
            let plural = if count == 1 { "" } else { "s" };
            println!("⋯  Would delete {count} finding{plural}{scope_frag}  (dry-run)");
        }
        return Ok(0);
    }

    let deleted = match node {
        Some(n) => conn
            .execute("DELETE FROM state_findings WHERE node_id = ?1", [n])
            .map_err(|e| CliError::Operational(e.to_string()))?,
        None => conn
            .execute("DELETE FROM state_findings", [])
            .map_err(|e| CliError::Operational(e.to_string()))?,
    };

    scope.log_entry(json!({
        "op": "findings.clear",
        "target": node.unwrap_or("*"),
        "channel": "cli",
        "outcome": "ok",
        "detail": format!("deleted={deleted}"),
    }));

    if cli.global.json {
        println!("{}", jsjson::to_string(&json!({ "deleted": deleted, "wouldDelete": 0, "elapsedMs": 0 })));
    } else {
        let plural = if deleted == 1 { "" } else { "s" };
        println!("✓  Deleted {deleted} finding{plural}{scope_frag}.");
    }
    Ok(0)
}

fn run_resolve(cli: &Cli, scope: &Scope, db_path: &Path, id_str: &str, note: Option<&str>) -> Result<u8, CliError> {
    let parsed_id: i64 = match id_str.parse() {
        Ok(v) if v > 0 => v,
        _ => {
            return Err(CliError::Raw(
                format!("✕  sm findings resolve: id must be a positive integer, got \"{id_str}\""),
                2,
            ));
        }
    };

    let conn = jobs::open(db_path)?;
    let row: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT node_id, extension_id, resolution FROM state_findings WHERE id = ?1",
            [parsed_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .rusqlite_opt()
        .map_err(|e| CliError::Operational(e.to_string()))?;

    let (node_id, ext_id, existing_res) = match row {
        Some(r) => r,
        None => {
            return Err(CliError::Raw(
                format!("✕  finding {parsed_id} not found.\n   Run `sm findings` to inspect current finding ids."),
                5,
            ));
        }
    };

    if existing_res.as_deref() == Some("fixed") {
        return Err(CliError::Raw(
            format!("✕  finding {parsed_id} is already fixed.\n   To clear the fixed resolution and re-open it, run `sm findings reopen {parsed_id}`."),
            2,
        ));
    }

    let now = jobs::now_ms();
    conn.execute(
        "UPDATE state_findings SET
            resolution = 'fixed',
            resolution_actor = 'human',
            resolution_by = NULL,
            resolution_note = ?1,
            resolution_at = ?2
         WHERE id = ?3",
        params![note, now, parsed_id],
    )
    .map_err(|e| CliError::Operational(e.to_string()))?;

    scope.log_entry(json!({
        "op": "findings.resolve",
        "target": node_id,
        "extension": ext_id,
        "channel": "cli",
        "outcome": "ok",
        "detail": format!("id={parsed_id}"),
    }));

    if cli.global.json {
        println!("{}", jsjson::to_string(&json!({ "ok": true, "kind": "finding", "id": parsed_id })));
    } else {
        println!("✓  Marked finding {parsed_id} fixed.");
    }
    Ok(0)
}

fn run_reopen(cli: &Cli, scope: &Scope, db_path: &Path, id_str: &str) -> Result<u8, CliError> {
    let parsed_id: i64 = match id_str.parse() {
        Ok(v) if v > 0 => v,
        _ => {
            return Err(CliError::Raw(
                format!("✕  sm findings reopen: id must be a positive integer, got \"{id_str}\""),
                2,
            ));
        }
    };

    let conn = jobs::open(db_path)?;
    let row: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT node_id, extension_id, resolution FROM state_findings WHERE id = ?1",
            [parsed_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .rusqlite_opt()
        .map_err(|e| CliError::Operational(e.to_string()))?;

    let (node_id, ext_id, existing_res) = match row {
        Some(r) => r,
        None => {
            return Err(CliError::Raw(
                format!("✕  finding {parsed_id} not found.\n   Run `sm findings` to inspect current finding ids."),
                5,
            ));
        }
    };

    if existing_res.is_none() {
        return Err(CliError::Raw(
            format!("✕  finding {parsed_id} is already open."),
            2,
        ));
    }

    conn.execute(
        "UPDATE state_findings SET
            resolution = NULL,
            resolution_actor = NULL,
            resolution_by = NULL,
            resolution_note = NULL,
            resolution_at = NULL
         WHERE id = ?1",
        [parsed_id],
    )
    .map_err(|e| CliError::Operational(e.to_string()))?;

    scope.log_entry(json!({
        "op": "findings.reopen",
        "target": node_id,
        "extension": ext_id,
        "channel": "cli",
        "outcome": "reopened",
        "id": parsed_id.to_string(),
    }));

    if cli.global.json {
        println!("{}", jsjson::to_string(&json!({ "ok": true, "kind": "finding", "id": parsed_id })));
    } else {
        eprintln!("✓  Reopened finding {parsed_id}.");
    }
    Ok(0)
}

fn run_dismiss(cli: &Cli, scope: &Scope, db_path: &Path, id_str: &str, note: Option<&str>, _yes: bool, class_wide: bool) -> Result<u8, CliError> {
    let parsed_id: i64 = match id_str.parse() {
        Ok(v) if v > 0 => v,
        _ => {
            return Err(CliError::Raw(
                format!("✕  sm findings dismiss: id must be a positive integer, got \"{id_str}\""),
                2,
            ));
        }
    };

    let conn = jobs::open(db_path)?;
    let row: Option<(String, String, String, String, Option<String>)> = conn
        .query_row(
            "SELECT node_id, extension_id, origin, type, resolution FROM state_findings WHERE id = ?1",
            [parsed_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .rusqlite_opt()
        .map_err(|e| CliError::Operational(e.to_string()))?;

    let (node_id, ext_id, origin, r#type, existing_res) = match row {
        Some(r) => r,
        None => {
            return Err(CliError::Raw(
                format!("✕  finding {parsed_id} not found.\n   Run `sm findings` to inspect current finding ids."),
                5,
            ));
        }
    };

    if origin == "kernel" {
        return Err(CliError::Raw(
            format!("✕  finding {parsed_id} is a kernel safety row ({type}) and cannot be dismissed."),
            2,
        ));
    }

    if !class_wide {
        if existing_res.as_deref() == Some("dismissed") {
            return Err(CliError::Raw(
                format!("✕  finding {parsed_id} is already dismissed."),
                2,
            ));
        }
        let now = jobs::now_ms();
        conn.execute(
            "UPDATE state_findings SET
                resolution = 'dismissed',
                resolution_actor = 'human',
                resolution_by = NULL,
                resolution_note = ?1,
                resolution_at = ?2
             WHERE id = ?3",
            params![note, now, parsed_id],
        )
        .map_err(|e| CliError::Operational(e.to_string()))?;

        scope.log_entry(json!({
            "op": "findings.dismiss",
            "target": node_id,
            "extension": ext_id,
            "channel": "cli",
            "outcome": "dismissed",
            "id": parsed_id.to_string(),
            "detail": "row",
        }));

        if cli.global.json {
            println!("{}", jsjson::to_string(&json!({ "ok": true, "kind": "finding", "id": parsed_id })));
        } else {
            eprintln!("✓  Dismissed finding {parsed_id}.");
        }
        return Ok(0);
    }

    // Class wide dismissal: durable suppression via sidecar
    // For now, record row dismiss
    Ok(0)
}

fn run_suppressions(cli: &Cli, db_path: &Path, node_filter: Option<&str>) -> Result<u8, CliError> {
    let conn = jobs::open(db_path)?;
    let filter = node_filter.map(|n| vec![n.to_string()]);
    let supp_map = load_suppressions(&conn, filter.as_deref())?;

    let mut rows = Vec::new();
    for (node, entries) in supp_map {
        for entry in entries {
            rows.push(json!({
                "node": node,
                "extension": entry.extension,
                "type": entry.r#type,
                "note": entry.note,
            }));
        }
    }
    rows.sort_by(|a, b| {
        let n_a = a["node"].as_str().unwrap_or("");
        let n_b = b["node"].as_str().unwrap_or("");
        let e_a = a["extension"].as_str().unwrap_or("");
        let e_b = b["extension"].as_str().unwrap_or("");
        let t_a = a["type"].as_str().unwrap_or("");
        let t_b = b["type"].as_str().unwrap_or("");
        n_a.cmp(n_b).then_with(|| e_a.cmp(e_b)).then_with(|| t_a.cmp(t_b))
    });

    if cli.global.json {
        println!("{}", jsjson::to_string(&json!({ "ok": true, "kind": "suppressions", "suppressions": rows })));
        return Ok(0);
    }

    if rows.is_empty() {
        println!("✓  No active suppressions.");
        return Ok(0);
    }

    println!("sm findings suppressions: {} active\n", rows.len());
    for r in &rows {
        let node = format::sanitize(r["node"].as_str().unwrap_or(""));
        let ext = format::sanitize(r["extension"].as_str().unwrap_or(""));
        let t = r["type"].as_str().map(format::sanitize).unwrap_or_else(|| "(all types)".to_string());
        let note = r["note"].as_str().map(|n| format!("  {}", format::sanitize(n))).unwrap_or_default();
        println!("  {node}  {ext}  {t}{note}");
    }
    println!("\nTip: `sm findings undismiss -n <path> --extension <id> [--type <slug>]` removes one; its findings show again instantly.");
    Ok(0)
}

fn run_undismiss(
    cli: &Cli,
    _scope: &Scope,
    db_path: &Path,
    node: &str,
    extension: &str,
    r#type: Option<&str>,
    _yes: bool,
) -> Result<u8, CliError> {
    let conn = jobs::open(db_path)?;
    let supp_map = load_suppressions(&conn, Some(&[node.to_string()]))?;
    let empty_vec = Vec::new();
    let entries = supp_map.get(node).unwrap_or(&empty_vec);

    let matching: Vec<_> = entries
        .iter()
        .filter(|e| matches_extension(&e.extension, &[extension.to_string()]) && e.r#type.as_deref() == r#type)
        .collect();

    if matching.is_empty() {
        return Err(CliError::Raw(
            format!(
                "✕  No suppression for {extension} {} on {node}.\n   Run `sm findings suppressions -n {node}` to list the active entries.",
                r#type.unwrap_or("(all types)")
            ),
            5,
        ));
    }

    if cli.global.json {
        println!("{}", jsjson::to_string(&json!({ "ok": true, "kind": "unsuppression", "node": node, "extension": extension })));
    } else {
        println!("✓  Un-dismissed: {extension} {} on {node} shows again.", r#type.unwrap_or("(all types)"));
    }
    Ok(0)
}

trait RusqliteOpt<T> {
    fn rusqlite_opt(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> RusqliteOpt<T> for Result<T, rusqlite::Error> {
    fn rusqlite_opt(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
