//! Command-line surface. Verb names, flags, exit codes and human text follow
//! skill-map's CLI contract (spec_doc §18) so both binaries can run the same
//! conformance scenarios.

use crate::config;
use crate::enrich;
use crate::findings;
use crate::history;
use crate::model::{Issue, ScanResult};
use crate::plugins;
use crate::record;
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
        /// Explicit alias of the incremental default.
        #[arg(long)]
        changed: bool,
        /// Force a complete re-extraction (renames are still detected).
        #[arg(long)]
        full: bool,
        /// Allow a zero-result scan to wipe a populated DB.
        #[arg(long)]
        allow_empty: bool,
        /// Alias of `watch`.
        #[arg(long)]
        watch: bool,
    },
    /// Watch the project and rescan after each debounced batch of changes.
    Watch {
        /// Skip token counting.
        #[arg(long)]
        no_tokens: bool,
        #[arg(long)]
        max_scan: Option<u64>,
        #[arg(long)]
        max_nodes: Option<u64>,
        /// Consecutive batch failures before shutting down (0 disables).
        #[arg(long, default_value_t = 5)]
        max_consecutive_failures: u32,
    },
    /// Bump a node's sidecar: increment annotations.version, refresh hashes, stamp audit.
    Bump {
        node: Option<String>,
        /// Bump every node whose sidecar reports drift.
        #[arg(long)]
        pending: bool,
        /// After each successful --pending bump, `git add` the .sm file.
        #[arg(long)]
        staged: bool,
        /// Bump even when the node is fresh.
        #[arg(long)]
        force: bool,
        /// Confirm writing .sm sidecar files in this project.
        #[arg(long)]
        yes: bool,
    },
    /// Sidecar (.sm) maintenance verbs.
    Sidecars {
        #[command(subcommand)]
        command: SidecarsCommand,
    },
    /// Durable per-node issue suppressions.
    Issues {
        #[command(subcommand)]
        command: IssuesCommand,
    },
    /// List orphan / auto-rename issues, or act on them.
    Orphans {
        /// orphan | medium | ambiguous
        #[arg(long)]
        kind: Option<String>,
        #[command(subcommand)]
        command: Option<OrphansCommand>,
    },
    /// Install / inspect the sm-process-jobs agent skill.
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    /// The probabilistic job queue.
    Jobs {
        #[command(subcommand)]
        command: JobsCommand,
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
    /// Close a running job with success or failure. Nonce is the sole credential.
    Record(record::RecordArgs),
    /// Print stored probabilistic findings (finder judgments + kernel safety rows).
    Findings(findings::FindingsArgs),
    /// Filter execution records.
    History(history::HistoryArgs),
    /// Configuration commands.
    Config(config::ConfigArgs),
    /// Plugins maintenance verbs.
    Plugins(plugins::PluginsArgs),
    /// Refresh enrichment rows.
    Enrich(enrich::EnrichArgs),
}

#[derive(Subcommand)]
pub enum SidecarsCommand {
    /// Refresh a sidecar's identity hashes to match the live node (no version bump).
    Refresh {
        node: String,
        #[arg(long)]
        yes: bool,
    },
    /// Delete orphan .sm files (sidecars whose .md no longer exists).
    Prune {
        #[arg(short = 'n', long)]
        dry_run: bool,
        /// Skip the interactive confirmation prompt.
        #[arg(long, alias = "force")]
        yes: bool,
    },
    /// Scaffold an empty `<basename>.sm` next to a node.
    Annotate {
        node: String,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub enum IssuesCommand {
    /// Dismiss a deterministic analyzer issue (analyzer + exact value) on one node, durably.
    Dismiss {
        analyzer: String,
        value: String,
        #[arg(short = 'n', long = "node")]
        node: String,
        #[arg(long)]
        note: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Remove an issue suppression written by dismiss.
    Undismiss {
        analyzer: String,
        value: String,
        #[arg(short = 'n', long = "node")]
        node: String,
        #[arg(long)]
        yes: bool,
    },
    /// List active issue suppressions.
    Suppressions {
        #[arg(short = 'n', long = "node")]
        node: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum OrphansCommand {
    /// Migrate state_* FKs from an orphan path to a live node.
    Reconcile {
        orphan: String,
        #[arg(long)]
        to: String,
        #[arg(short = 'n', long)]
        dry_run: bool,
    },
    /// Reverse a medium- or ambiguous-confidence auto-rename.
    UndoRename {
        new_path: String,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        force: bool,
        #[arg(short = 'n', long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub enum AgentCommand {
    /// Materialise the sm-process-jobs process skill into the lens's skill directory.
    Install {
        #[arg(long = "for")]
        for_provider: Option<String>,
    },
    /// Remove the materialised sm-process-jobs process skill.
    Uninstall {
        #[arg(long = "for")]
        for_provider: Option<String>,
    },
    /// Report the sm-process-jobs process skill install state.
    Status {
        #[arg(long = "for")]
        for_provider: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum JobsCommand {
    /// Enqueue a probabilistic extension against a node (-n), every matching node (--all), or none (nodeless).
    Submit {
        extension: String,
        #[arg(short = 'n')]
        node: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        force: bool,
        #[arg(long, allow_hyphen_values = true)]
        ttl: Option<String>,
        #[arg(long, allow_hyphen_values = true)]
        priority: Option<String>,
    },
    /// List jobs.
    List {
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        extension: Option<String>,
        #[arg(long)]
        node: Option<String>,
    },
    /// Job detail.
    Show { id: String },
    /// Print a job's rendered content.
    Preview {
        id: Option<String>,
        #[arg(long)]
        last: bool,
    },
    /// Atomically claim the next queued job.
    Claim {
        #[arg(long)]
        filter: Option<String>,
        #[arg(long)]
        wait: bool,
        #[arg(long)]
        interval: Option<String>,
        #[arg(long)]
        timeout: Option<String>,
    },
    /// Counts per status, or one job's status.
    Status { id: Option<String> },
    /// Cancel a queued / running job (or --all).
    Cancel {
        id: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Fail a queued / running job with reason user-failed (or --all).
    Fail {
        id: Option<String>,
        #[arg(long)]
        all: bool,
    },
}

pub enum CliError {
    Usage(String),
    NotFound(String),
    Operational(String),
    /// Printed verbatim (no glyph prefix) with this exit code.
    Raw(String, u8),
}

impl CliError {
    pub fn exit_code(&self) -> u8 {
        match self {
            CliError::Usage(_) | CliError::Operational(_) => 2,
            CliError::NotFound(_) => 5,
            CliError::Raw(_, code) => *code,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            CliError::Usage(m) | CliError::NotFound(m) | CliError::Operational(m) | CliError::Raw(m, _) => m,
        }
    }
}

pub fn dispatch(cli: &Cli) -> Result<u8, CliError> {
    let root = std::env::current_dir().map_err(|e| CliError::Operational(e.to_string()))?;
    let scope = Scope::new(root);
    let Some(command) = &cli.command else {
        return Err(CliError::Usage("no verb given; run `asm --help`".into()));
    };
    match command {
        Command::Version => version(cli, &scope),
        Command::Scan { no_tokens, max_scan, max_nodes, dry_run, changed, full, allow_empty, watch: watch_alias } => {
            if *watch_alias {
                let conflict = [(*dry_run, "--dry-run", "Drop --dry-run or use --watch alone. The watcher always persists each incremental batch."),
                    (*changed, "--changed", "Drop --changed or use --watch alone. The watcher is incremental by definition."),
                    (*full, "--full", "Drop --full or use --watch alone. The watcher is incremental by definition; run a one-shot `sm scan --full` first if you need a clean rebuild."),
                    (*allow_empty, "--allow-empty", "Drop --allow-empty or use --watch alone. The watcher never produces a zero-result wipe.")]
                .into_iter()
                .find(|c| c.0);
                if let Some((_, flag, hint)) = conflict {
                    return Err(CliError::Usage(format!("--watch cannot be combined with {flag}.\n   {hint}")));
                }
                return run_watch(cli, &scope, !no_tokens, *max_scan, *max_nodes, 5);
            }
            if *full && *changed {
                return Err(CliError::Usage(
                    "--full and --changed cannot be combined.\n   --changed is the explicit alias of the incremental default and --full forces a complete re-extraction; pick one.".into(),
                ));
            }
            run_scan(cli, &scope, ScanFlags { tokenize: !no_tokens, max_scan: *max_scan, max_nodes: *max_nodes, dry_run: *dry_run, changed: *changed, full: *full, allow_empty: *allow_empty })
        }
        Command::Watch { no_tokens, max_scan, max_nodes, max_consecutive_failures } => {
            run_watch(cli, &scope, !no_tokens, *max_scan, *max_nodes, *max_consecutive_failures)
        }
        Command::Check { node } => check(cli, &scope, node.as_deref()),
        Command::Issues { command } => {
            use crate::issues::{self, Target};
            let db = db_path(cli, &scope);
            match command {
                IssuesCommand::Dismiss { analyzer, value, node, note, yes } => {
                    issues::dismiss(cli, &scope, &db, Target { analyzer, value, node, yes: *yes }, note.as_deref())
                }
                IssuesCommand::Undismiss { analyzer, value, node, yes } => {
                    issues::undismiss(cli, &scope, &db, Target { analyzer, value, node, yes: *yes })
                }
                IssuesCommand::Suppressions { node } => issues::suppressions(cli, &db, node.as_deref()),
            }
        }
        Command::Orphans { kind, command } => {
            let db = db_path(cli, &scope);
            match command {
                None => crate::orphans::list(cli, &db, kind.as_deref()),
                Some(OrphansCommand::Reconcile { orphan, to, dry_run }) => {
                    crate::orphans::reconcile(cli, &scope, &db, orphan, to, *dry_run)
                }
                Some(OrphansCommand::UndoRename { new_path, from, force, dry_run }) => crate::orphans::undo_rename(
                    cli,
                    &scope,
                    &db,
                    crate::orphans::UndoArgs { new_path, from: from.as_deref(), force: *force, dry_run: *dry_run },
                ),
            }
        }
        Command::Jobs { command } => {
            use crate::jobs;
            let db = db_path(cli, &scope);
            match command {
                JobsCommand::Submit { extension, node, all, force, ttl, priority } => jobs::submit(
                    cli,
                    &scope,
                    &db,
                    jobs::SubmitArgs {
                        extension,
                        node: node.as_deref(),
                        all: *all,
                        force: *force,
                        ttl: ttl.as_deref(),
                        priority: priority.as_deref(),
                    },
                ),
                JobsCommand::List { status, extension, node } => {
                    jobs::list(cli, &db, status.as_deref(), extension.as_deref(), node.as_deref())
                }
                JobsCommand::Show { id } => jobs::show(cli, &db, id),
                JobsCommand::Preview { id, last } => jobs::preview(&db, id.as_deref(), *last),
                JobsCommand::Claim { filter, wait, interval, timeout } => jobs::claim(
                    cli,
                    &db,
                    jobs::ClaimArgs { filter: filter.as_deref(), wait: *wait, interval: interval.as_deref(), timeout: timeout.as_deref() },
                ),
                JobsCommand::Status { id } => jobs::status(cli, &db, id.as_deref()),
                JobsCommand::Cancel { id, all } => jobs::terminate(cli, &scope, &db, id.as_deref(), *all, false),
                JobsCommand::Fail { id, all } => jobs::terminate(cli, &scope, &db, id.as_deref(), *all, true),
            }
        }
        Command::Agent { command } => match command {
            AgentCommand::Install { for_provider } => crate::agent::install(cli, &scope, for_provider.as_deref()),
            AgentCommand::Uninstall { for_provider } => crate::agent::uninstall(cli, &scope, for_provider.as_deref()),
            AgentCommand::Status { for_provider } => crate::agent::status_cmd(cli, &scope, for_provider.as_deref()),
        },
        Command::Sidecars { command } => {
            let db = db_path(cli, &scope);
            match command {
                SidecarsCommand::Refresh { node, yes } => crate::sidecars::refresh(cli, &scope, &db, node, *yes),
                SidecarsCommand::Prune { dry_run, yes } => crate::sidecars::prune(cli, &scope, *dry_run, *yes),
                SidecarsCommand::Annotate { node, force, yes } => crate::sidecars::annotate(cli, &scope, &db, node, *force, *yes),
            }
        }
        Command::Bump { node, pending, staged, force, yes } => crate::bump::run(
            cli,
            &scope,
            &db_path(cli, &scope),
            crate::bump::Args { node: node.as_deref(), pending: *pending, staged: *staged, force: *force, yes: *yes },
        ),
        Command::List { kind } => list(cli, &scope, kind.as_deref()),
        Command::Graph { format } => graph(cli, &scope, format),
        Command::Record(args) => record::run(cli, &scope, &db_path(cli, &scope), args),
        Command::Findings(args) => findings::run(cli, &scope, &db_path(cli, &scope), args),
        Command::History(args) => history::run(cli, &scope, &db_path(cli, &scope), args),
        Command::Config(args) => config::run(cli, &scope, args),
        Command::Plugins(args) => plugins::run(cli, &scope, args),
        Command::Enrich(args) => enrich::run(cli, &scope, args),
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
/// The active lens without side effects (settings, else one marker, else the
/// open `agent-skills` default; several markers fall back to the default).
pub fn detect_lens(scope: &Scope) -> String {
    if let Some(id) = scope.settings().get("activeProvider").and_then(Value::as_str)
        && provider::get(id).is_some()
    {
        return id.to_string();
    }
    match provider::detect_candidates(&scope.root).as_slice() {
        [one] => one.to_string(),
        _ => "agent-skills".into(),
    }
}

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

struct ScanFlags {
    tokenize: bool,
    max_scan: Option<u64>,
    max_nodes: Option<u64>,
    dry_run: bool,
    changed: bool,
    full: bool,
    allow_empty: bool,
}

fn scan_opts(scope: &Scope, lens: String, tokenize: bool, max_scan: Option<u64>, max_nodes: Option<u64>) -> scan::ScanOpts {
    let settings = scope.settings();
    let ignore = settings
        .get("ignore")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    scan::ScanOpts {
        root: scope.root.clone(),
        lens,
        ignore,
        respect_gitignore: settings.get("scan").and_then(|s| s.get("respectGitignore")).and_then(Value::as_bool).unwrap_or(false),
        max_scan: max_scan.unwrap_or_else(|| setting_u64(&settings, "maxScan", 5000)),
        max_nodes: max_nodes.unwrap_or_else(|| setting_u64(&settings, "maxNodes", 256)),
        max_file_size: setting_u64(&settings, "maxFileSizeBytes", 1 << 20),
        tokenize,
        follow_external_symlinks: false,
        changed: None,
        ignored_references: settings
            .get("plugins")
            .and_then(|p| p.pointer("/core/extensions/reference-broken/settings/ignored-references"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|e| Some((e.get("type")?.as_str()?.to_string(), e.get("value")?.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// The persisted snapshot and its extractor runs, or `None` when there is
/// no DB or it holds no nodes.
fn load_prior(db: &std::path::Path) -> Option<(ScanResult, scan::PriorRuns)> {
    if !db.exists() {
        return None;
    }
    let conn = store::open_existing(db).ok()?;
    let prior = store::load(&conn).ok().flatten().filter(|p| !p.nodes.is_empty())?;
    Some((prior, store::load_extractor_runs(&conn).unwrap_or_default()))
}

fn run_scan(cli: &Cli, scope: &Scope, flags: ScanFlags) -> Result<u8, CliError> {
    let ScanFlags { tokenize, max_scan, max_nodes, dry_run, changed, full, allow_empty } = flags;
    if !dry_run {
        scope.provision().map_err(|e| CliError::Operational(e.to_string()))?;
    }
    let lens = resolve_lens(scope, dry_run, cli.global.json)?;
    let opts = scan_opts(scope, lens, tokenize, max_scan, max_nodes);
    let db = db_path(cli, scope);
    let prior = load_prior(&db);
    if changed && prior.is_none() {
        eprint!("--changed: no prior snapshot found; running full scan.\n");
    }
    let scan::Outcome { mut result, renames, extractor_runs } =
        scan::run_incremental(&opts, prior.as_ref().map(|p| &p.0), prior.as_ref().filter(|_| !full).map(|p| &p.1), full);
    if !dry_run {
        let mut conn = store::open(&db, &scope.backups_dir()).map_err(|e| CliError::Operational(e.to_string()))?;
        if result.nodes.is_empty() && !allow_empty {
            let existing = store::count_scan_rows(&conn).map_err(|e| CliError::Operational(e.to_string()))?;
            if existing > 0 {
                return Err(CliError::Usage(format!(
                    "Refusing to wipe a populated DB ({existing} rows in scan_*) with a zero-result scan.\n   Pass --allow-empty to override. If this is unexpected, double-check the root paths."
                )));
            }
        }
        store::persist(&mut conn, &mut result, &renames, &extractor_runs).map_err(|e| CliError::Operational(e.to_string()))?;
        scope.log_operation("scan", "cli", "ok", &format!("nodes={} issues={}", result.nodes.len(), result.issues.len()));
    }
    let errors = result.issues.iter().filter(|i| i.severity == "error").count();
    if cli.global.json {
        println!("{}", jsjson::to_string(&result.to_json()));
    } else {
        // Distinct affected nodes per severity, not issue counts.
        let affected = |sev: &str| {
            let mut ids: Vec<&str> = Vec::new();
            for i in result.issues.iter().filter(|i| i.severity == sev) {
                if i.node_ids.is_empty() && !ids.contains(&"") {
                    ids.push("");
                }
                for id in &i.node_ids {
                    if !ids.contains(&id.as_str()) {
                        ids.push(id);
                    }
                }
            }
            ids.len()
        };
        let (errs, warnings, infos) = (affected("error"), affected("warn"), affected("info"));
        let mut counts = vec![plural(result.nodes.len(), "node", "nodes"), plural(result.links.len(), "link", "links")];
        if errs > 0 {
            counts.push(plural(errs, "error", "errors"));
        }
        if warnings > 0 {
            counts.push(plural(warnings, "warning", "warnings"));
        }
        if infos > 0 {
            counts.push(format!("{infos} info"));
        }
        if errs + warnings + infos == 0 {
            counts.push("0 issues".into());
        }
        let glyph = if errors > 0 { " " } else { "✓" };
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

/// `sm watch` / `sm scan --watch`: an initial incremental scan, then one
/// rescan (persisted) per debounced batch of filesystem events until
/// SIGINT/SIGTERM. Every batch prints one summary line, or one ScanResult
/// per line under --json.
fn run_watch(cli: &Cli, scope: &Scope, tokenize: bool, max_scan: Option<u64>, max_nodes: Option<u64>, breaker: u32) -> Result<u8, CliError> {
    use notify::{RecursiveMode, Watcher};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    let json_mode = cli.global.json;
    scope.provision().map_err(|e| CliError::Operational(e.to_string()))?;
    // The watcher detects the lens in memory only: nothing persisted or announced.
    let lens = resolve_lens(scope, true, json_mode)?;
    let debounce = scope
        .settings()
        .get("scan")
        .and_then(|s| s.get("watch"))
        .and_then(|w| w.get("debounceMs"))
        .and_then(Value::as_u64)
        .unwrap_or(300);
    if !json_mode {
        eprint!("sm watch: starting on 1 root(s), debounce {debounce}ms\n");
    }
    let db = db_path(cli, scope);
    let mut prior = load_prior(&db);
    let batch = |prior: &Option<(ScanResult, scan::PriorRuns)>,
                 changed: Option<scan::ChangedPaths>|
     -> Result<ScanResult, String> {
        let mut opts = scan_opts(scope, lens.clone(), tokenize, max_scan, max_nodes);
        opts.changed = changed;
        let scan::Outcome { mut result, renames, extractor_runs } =
            scan::run_incremental(&opts, prior.as_ref().map(|p| &p.0), prior.as_ref().map(|p| &p.1), false);
        let mut conn = store::open(&db, &scope.backups_dir()).map_err(|e| e.to_string())?;
        store::persist(&mut conn, &mut result, &renames, &extractor_runs).map_err(|e| e.to_string())?;
        scope.log_operation("scan", "watcher", "ok", &format!("nodes={} issues={}", result.nodes.len(), result.issues.len()));
        Ok(result)
    };
    let render = |r: &ScanResult| {
        if json_mode {
            println!("{}", jsjson::to_string(&r.to_json()));
        } else {
            let (n, l, i) = (r.nodes.len(), r.links.len(), r.issues.len());
            println!(
                "✓  {} · {} · {}   in {}ms",
                plural(n, "node", "nodes"),
                plural(l, "link", "links"),
                plural(i, "issue", "issues"),
                r.stats.duration_ms
            );
        }
    };
    let first = batch(&prior, None).map_err(|m| CliError::Operational(format!("sm watch: initial scan failed: {m}")))?;
    render(&first);
    // Every batch diffs against the persisted snapshot, as read back.
    prior = load_prior(&db);
    // The initial scan is not counted in "stopped after N batch(es)".
    let mut batches = 0u64;

    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let _ = ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst));
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(tx).map_err(|e| CliError::Operational(format!("sm watch: watcher error: {e}")))?;
    watcher
        .watch(&scope.root, RecursiveMode::Recursive)
        .map_err(|e| CliError::Operational(format!("sm watch: watcher error: {e}")))?;
    if !json_mode {
        eprint!("sm watch: ready. Press Ctrl+C to stop.\n");
    }
    let ignore = scan::ignore_matcher(&scan_opts(scope, lens.clone(), tokenize, max_scan, max_nodes));
    // Root-relative POSIX paths of the files an event touched, minus ignored ones.
    let touched = |ev: &notify::Event| -> Vec<String> {
        if matches!(ev.kind, notify::EventKind::Access(_)) {
            return Vec::new();
        }
        ev.paths
            .iter()
            .filter_map(|p| p.strip_prefix(&scope.root).ok())
            .filter(|rel| !rel.as_os_str().is_empty() && !scope.root.join(rel).is_dir())
            .map(|rel| rel.to_string_lossy().replace('\\', "/"))
            .filter(|rel| !ignore.matched_path_or_any_parents(rel, false).is_ignore())
            .collect()
    };
    let mut pending: Vec<String> = Vec::new();
    let mut failures = 0u32;
    let mut code = 0u8;
    while !stop.load(Ordering::SeqCst) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(ev)) => pending.extend(touched(&ev)),
            Ok(Err(e)) => eprintln!("✕  sm watch: watcher error: {}", format::sanitize(&e.to_string())),
            Err(_) => {}
        }
        if pending.is_empty() {
            continue;
        }
        // Debounce: keep collecting until the tree is quiet for `debounce` ms.
        while let Ok(ev) = rx.recv_timeout(Duration::from_millis(debounce)) {
            if let Ok(ev) = ev {
                pending.extend(touched(&ev));
            }
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let mut seen = std::collections::HashSet::new();
        let mut changed = scan::ChangedPaths::default();
        for rel in pending.drain(..).filter(|r| seen.insert(r.clone())) {
            if scope.root.join(&rel).is_file() {
                changed.changed.push(rel);
            } else {
                changed.removed.push(rel);
            }
        }
        match batch(&prior, Some(changed)) {
            Ok(r) => {
                render(&r);
                prior = load_prior(&db);
                batches += 1;
                failures = 0;
            }
            Err(m) => {
                eprintln!("✕  sm watch: batch failed: {}", format::sanitize(&m));
                failures += 1;
                if breaker > 0 && failures >= breaker {
                    eprint!("✕  sm watch: {failures} consecutive batch failures, shutting down.\n   Last error: {m}\n");
                    code = 2;
                    break;
                }
            }
        }
    }
    if !json_mode {
        eprint!("sm watch: stopped after {batches} batch(es).\n");
    }
    Ok(code)
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
