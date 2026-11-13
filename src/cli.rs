//! Command-line surface. Verb names, flags, exit codes and human text follow
//! skill-map's CLI contract (spec_doc §18) so both binaries can run the same
//! conformance scenarios.

use crate::model::{Issue, ScanResult};
use crate::scope::Scope;
use crate::{format, jsjson, provider, scan, store};
use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "asm", version, about = "Map, inspect and audit AI-agent markdown ecosystems as a graph")]
pub struct Cli {
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Args)]
pub struct Global {
    /// Emit machine-readable output on stdout.
    #[arg(long, global = true, env = "SKILL_MAP_JSON", value_parser = clap::builder::BoolishValueParser::new())]
    pub json: bool,
    /// Suppress non-error stderr output.
    #[arg(short, long, global = true)]
    pub quiet: bool,
    /// Disable ANSI colors.
    #[arg(long, global = true)]
    pub no_color: bool,
    /// Override the database file location.
    #[arg(long, global = true, env = "SKILL_MAP_DB")]
    pub db: Option<PathBuf>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Print the version matrix.
    Version,
    /// Scan the project and persist the graph.
    Scan {
        /// Skip token counting.
        #[arg(long)]
        no_tokens: bool,
        /// Hard ceiling on files accepted by the walker.
        #[arg(long)]
        max_scan: Option<u64>,
        /// Maximum nodes the map renders.
        #[arg(long)]
        max_nodes: Option<u64>,
        /// Compute without writing the database.
        #[arg(short = 'n', long)]
        dry_run: bool,
    },
    /// Print the current issues from the database.
    Check {
        /// Restrict to issues of this node.
        #[arg(short = 'n', long = "node")]
        node: Option<String>,
    },
    /// List scanned nodes.
    List {
        #[arg(long)]
        kind: Option<String>,
    },
    /// Render the graph with a formatter.
    Graph {
        #[arg(long, default_value = "ascii")]
        format: String,
    },
}

pub enum CliError {
    Usage(String),
    NotFound(String),
    Operational(String),
}

impl CliError {
    pub fn exit_code(&self) -> u8 {
        match self {
            CliError::Usage(_) | CliError::Operational(_) => 2,
            CliError::NotFound(_) => 5,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            CliError::Usage(m) | CliError::NotFound(m) | CliError::Operational(m) => m,
        }
    }
}

pub struct Out {
    pub color: bool,
}

pub fn dispatch(cli: &Cli, out: &Out) -> Result<u8, CliError> {
    let root = std::env::current_dir().map_err(|e| CliError::Operational(e.to_string()))?;
    let scope = Scope::new(root);
    let Some(command) = &cli.command else {
        return Err(CliError::Usage("no verb given; run `asm --help`".into()));
    };
    match command {
        Command::Version => version(cli, &scope),
        Command::Scan { no_tokens, max_scan, max_nodes, dry_run } => {
            run_scan(cli, out, &scope, !no_tokens, *max_scan, *max_nodes, *dry_run)
        }
        Command::Check { node } => check(cli, &scope, node.as_deref()),
        Command::List { kind } => list(cli, &scope, kind.as_deref()),
        Command::Graph { format } => graph(cli, &scope, format),
    }
}

fn db_path(cli: &Cli, scope: &Scope) -> PathBuf {
    cli.global.db.clone().unwrap_or_else(|| scope.db_path())
}

fn version(cli: &Cli, scope: &Scope) -> Result<u8, CliError> {
    let db = db_path(cli, scope);
    let db_schema = if db.exists() {
        store::open_existing(&db)
            .and_then(|c| c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)))
            .map(|v| v.to_string())
            .unwrap_or_else(|_| "-".into())
    } else {
        "-".into()
    };
    if cli.global.json {
        println!("{}", jsjson::to_string(&json!({ "asm": env!("CARGO_PKG_VERSION"), "spec": scan::SPEC_VERSION, "dbSchema": db_schema })));
    } else {
        println!("  asm        {}", env!("CARGO_PKG_VERSION"));
        println!("  spec       {}", scan::SPEC_VERSION);
        println!("  runtime    Rust");
        println!("  db-schema  {db_schema}");
    }
    Ok(0)
}

fn setting_u64(settings: &serde_json::Map<String, Value>, key: &str, default: u64) -> u64 {
    settings.get("scan").and_then(|s| s.get(key)).and_then(Value::as_u64).unwrap_or(default)
}

/// Resolve the active lens: settings → marker auto-detect (persisted when
/// unambiguous) → the open `agent-skills` default.
fn resolve_lens(scope: &Scope, dry_run: bool, json_mode: bool) -> Result<String, CliError> {
    let mut settings = scope.settings();
    if let Some(id) = settings.get("activeProvider").and_then(Value::as_str)
        && provider::get(id).is_some_and(|p| p.gated)
    {
        return Ok(id.to_string());
    }
    let candidates = provider::detect_candidates(&scope.root);
    match candidates.as_slice() {
        [] => Ok("agent-skills".into()),
        [one] => {
            if !dry_run {
                settings.insert("activeProvider".into(), json!(one));
                settings.insert("activeProviderMarkers".into(), json!([one]));
                scope.write_settings(&settings).map_err(|e| CliError::Operational(e.to_string()))?;
                let notice = format!("Auto-detected activeProvider = {one} from filesystem markers; persisted to .skill-map/settings.json.");
                // stdout carries only the document in --json mode.
                if json_mode {
                    eprintln!("{notice}");
                } else {
                    println!("{notice}");
                }
            }
            Ok(one.to_string())
        }
        many => Err(CliError::Usage(format!(
            "several provider markers found ({}); pick one with `asm config set activeProvider <id>`",
            many.join(", ")
        ))),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn run_scan(
    cli: &Cli,
    out: &Out,
    scope: &Scope,
    tokenize: bool,
    max_scan: Option<u64>,
    max_nodes: Option<u64>,
    dry_run: bool,
) -> Result<u8, CliError> {
    if !dry_run {
        scope.provision().map_err(|e| CliError::Operational(e.to_string()))?;
    }
    let lens = resolve_lens(scope, dry_run, cli.global.json)?;
    let settings = scope.settings();
    let ignore = settings
        .get("ignore")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    let opts = scan::ScanOpts {
        root: scope.root.clone(),
        lens,
        ignore,
        respect_gitignore: settings.get("scan").and_then(|s| s.get("respectGitignore")).and_then(Value::as_bool).unwrap_or(false),
        max_scan: max_scan.unwrap_or_else(|| setting_u64(&settings, "maxScan", 5000)),
        max_nodes: max_nodes.unwrap_or_else(|| setting_u64(&settings, "maxNodes", 256)),
        max_file_size: setting_u64(&settings, "maxFileSizeBytes", 1 << 20),
        tokenize,
        follow_external_symlinks: false,
    };
    let result = scan::run(&opts);
    let db = db_path(cli, scope);
    if !dry_run {
        let mut conn = store::open(&db, &scope.backups_dir()).map_err(|e| CliError::Operational(e.to_string()))?;
        store::persist(&mut conn, &result).map_err(|e| CliError::Operational(e.to_string()))?;
        scope.log_operation("scan", "ok", &format!("nodes={} issues={}", result.nodes.len(), result.issues.len()));
    }
    let errors = result.issues.iter().filter(|i| i.severity == "error").count();
    if cli.global.json {
        println!("{}", jsjson::to_string(&result));
    } else {
        let warnings = result.issues.iter().filter(|i| i.severity == "warn").count();
        let infos = result.issues.iter().filter(|i| i.severity == "info").count();
        let mut counts = vec![plural(result.nodes.len(), "node", "nodes"), plural(result.links.len(), "link", "links")];
        if errors > 0 {
            counts.push(plural(errors, "error", "errors"));
        }
        if warnings > 0 {
            counts.push(plural(warnings, "warning", "warnings"));
        }
        if infos > 0 {
            counts.push(format!("{infos} info"));
        }
        if errors + warnings + infos == 0 {
            counts.push("0 issues".into());
        }
        let glyph = if !out.color { " " } else if errors > 0 { "✕" } else { "✓" };
        println!("  {glyph}  {}   in {}", counts.join(" · "), format_elapsed(result.stats.duration_ms));
        let shown = db.strip_prefix(&scope.root).unwrap_or(&db).display().to_string();
        if dry_run {
            println!("     would persist to {shown}  (dry-run)");
        } else {
            println!("     {shown}");
        }
    }
    Ok(if errors > 0 { 1 } else { 0 })
}

pub fn format_elapsed(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m {}s", ms / 60_000, ((ms % 60_000) as f64 / 1000.0).round() as u64)
    }
}

fn load(cli: &Cli, scope: &Scope) -> Result<ScanResult, CliError> {
    let db = db_path(cli, scope);
    if !db.exists() {
        return Err(CliError::NotFound(format!("DB not found at {}\n   Run `sm scan` first.", db.display())));
    }
    let conn = store::open_existing(&db).map_err(|e| CliError::Operational(e.to_string()))?;
    store::load(&conn)
        .map_err(|e| CliError::Operational(e.to_string()))?
        .ok_or_else(|| CliError::NotFound(format!("no scan persisted in {}\n   Run `sm scan` first.", db.display())))
}

fn check(cli: &Cli, scope: &Scope, node: Option<&str>) -> Result<u8, CliError> {
    let scan = load(cli, scope)?;
    let issues: Vec<&Issue> = scan.issues.iter().filter(|i| node.is_none_or(|n| i.node_ids.iter().any(|id| id == n))).collect();
    let exit = if issues.iter().any(|i| i.severity == "error") { 1 } else { 0 };
    if cli.global.json {
        println!("{}", jsjson::to_string(&issues));
        return Ok(exit);
    }
    if issues.is_empty() {
        print!("✓  No issues.\n");
        return Ok(0);
    }
    let count = |s: &str| issues.iter().filter(|i| i.severity == s).count();
    let mut summary = Vec::new();
    if count("error") > 0 {
        summary.push(plural(count("error"), "error", "errors"));
    }
    if count("warn") > 0 {
        summary.push(plural(count("warn"), "warning", "warnings"));
    }
    if count("info") > 0 {
        summary.push(format!("{} info", count("info")));
    }
    let mut text = format!("sm check: {}\n\n", summary.join(" · "));
    let width = issues.iter().map(|i| format::sanitize(&i.analyzer_id).encode_utf16().count()).max().unwrap_or(0);
    let mut files: Vec<(String, Vec<&Issue>)> = Vec::new();
    for i in &issues {
        let primary = format::sanitize(i.node_ids.first().map_or("(no file)", String::as_str));
        match files.iter_mut().find(|(f, _)| *f == primary) {
            Some((_, b)) => b.push(i),
            None => files.push((primary, vec![i])),
        }
    }
    let rank = |s: &str| match s {
        "error" => 0,
        "warn" => 1,
        _ => 2,
    };
    let mut sections = Vec::new();
    for (file, mut bucket) in files {
        bucket.sort_by_key(|i| rank(i.severity));
        let mut s = format!("  {file}\n");
        for i in bucket {
            let glyph = match i.severity {
                "error" => "✕",
                "warn" => "⚠",
                _ => "ℹ",
            };
            let id = format::sanitize(&i.analyzer_id);
            let pad = " ".repeat(width.saturating_sub(id.encode_utf16().count()));
            let mut msg = format::sanitize(&i.message);
            let needle = format!(" from {file}");
            if file != "(no file)" && msg.contains(&needle) {
                msg = msg.replacen(&needle, "", 1);
            }
            let flat = collapse_newlines(&msg);
            s.push_str(&format!("    {glyph}  {id}{pad}   {flat}\n"));
        }
        sections.push(s);
    }
    text.push_str(&sections.join("\n"));
    text.push_str("\nTip: `sm enrich <node>` to revalidate a file after fixes.\n");
    print!("{text}");
    Ok(exit)
}

fn collapse_newlines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_nl = false;
    for c in s.chars() {
        if c == '\n' {
            if !prev_nl {
                out.push(' ');
            }
            prev_nl = true;
        } else {
            out.push(c);
            prev_nl = false;
        }
    }
    out
}

fn list(cli: &Cli, scope: &Scope, kind: Option<&str>) -> Result<u8, CliError> {
    let scan = load(cli, scope)?;
    let mut nodes: Vec<_> = scan.nodes.iter().filter(|n| kind.is_none_or(|k| n.kind == k)).collect();
    nodes.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    if cli.global.json {
        println!("{}", jsjson::to_string(&nodes));
        return Ok(0);
    }
    if nodes.is_empty() {
        print!("No nodes found.\n");
        return Ok(0);
    }
    let issues_of = |p: &str| scan.issues.iter().map(|i| i.node_ids.iter().filter(|id| *id == p).count()).sum::<usize>();
    let len = |s: &str| s.encode_utf16().count();
    let rows: Vec<[String; 7]> = nodes
        .iter()
        .map(|n| {
            [
                format::sanitize(&n.path),
                format::sanitize(&n.kind),
                n.links_out_count.to_string(),
                n.links_in_count.to_string(),
                n.external_refs_count.to_string(),
                issues_of(&n.path).to_string(),
                n.tokens.map_or("-".to_string(), |t| t.total.to_string()),
            ]
        })
        .collect();
    let headers = ["PATH", "KIND", "OUT", "IN", "EXT", "ISSUES", "TOKENS"];
    let mut w: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for r in &rows {
        for (i, c) in r.iter().enumerate() {
            w[i] = w[i].max(len(c));
        }
    }
    w[0] = w[0].min(60);
    let fmt_row = |cells: &[String]| {
        let mut parts = Vec::new();
        for (i, c) in cells.iter().enumerate() {
            let mut c = c.clone();
            if i == 0 && c.chars().count() > w[0] {
                let chars: Vec<char> = c.chars().collect();
                c = format!("…{}", chars[chars.len() - w[0] + 1..].iter().collect::<String>());
            }
            let pad = " ".repeat(w[i].saturating_sub(len(&c)));
            parts.push(if i < 2 { format!("{c}{pad}") } else { format!("{pad}{c}") });
        }
        format!("  {}", parts.join("  "))
    };
    let mut lines = vec![fmt_row(&headers.map(String::from))];
    lines.extend(rows.iter().map(|r| fmt_row(r)));
    lines.push(String::new());
    lines.push(plural(nodes.len(), "node", "nodes"));
    lines.push("Tip: `sm show <path>` for details, `sm check` for issues.".into());
    println!("{}", lines.join("\n"));
    Ok(0)
}

fn graph(cli: &Cli, scope: &Scope, fmt: &str) -> Result<u8, CliError> {
    let scan = load(cli, scope)?;
    let text = if fmt == "json" { Some(jsjson::to_string(&store::loaded_json(&scan))) } else { format::render(fmt, &scan) };
    let text = text
        .ok_or_else(|| CliError::Usage(format!("unknown --format '{fmt}' (ascii, json, mermaid, dot)")))?;
    if text.ends_with('\n') {
        print!("{text}");
    } else {
        println!("{text}");
    }
    Ok(0)
}
