//! One scan: walk → classify → extract → resolve → analyze (spec_doc §8).

use crate::extract::{self, NodeInput};
use crate::model::{ExternalRef, Issue, Link, Node, OversizedFile, ScanResult, ScannedBy, Stats, Triple};
use crate::provider::{self, McpDialect, PROVIDERS, Parser};
use crate::walk::{self, IgnoreLayers, WalkOpts};
use crate::{analyze, canon_yaml, diagnose, frontmatter, resolve, tokens, validate};
use rayon::prelude::*;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const SPEC_VERSION: &str = "1.12.7";

pub struct ScanOpts {
    pub root: PathBuf,
    pub lens: String,
    pub ignore: Vec<String>,
    pub respect_gitignore: bool,
    pub max_scan: u64,
    pub max_nodes: u64,
    pub max_file_size: u64,
    pub tokenize: bool,
    pub follow_external_symlinks: bool,
    /// Watcher batches: the files that changed / disappeared since the prior.
    pub changed: Option<ChangedPaths>,
    /// `plugins.core.extensions.reference-broken.settings.ignored-references`.
    pub ignored_references: Vec<(String, String)>,
}

#[derive(Default, Clone)]
pub struct ChangedPaths {
    pub changed: Vec<String>,
    pub removed: Vec<String>,
}

fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

struct Built {
    node: Node,
    body: String,
    body_line_offset: usize,
    issues: Vec<Issue>,
}

/// A rename detected against the prior scan (state rows migrate with it).
pub struct Rename {
    pub from: String,
    pub to: String,
}

pub struct Outcome {
    pub result: ScanResult,
    pub renames: Vec<Rename>,
    pub extractor_runs: Vec<ExtractorRun>,
}

/// One `scan_extractor_runs` row: the cache key an extractor ran under.
#[derive(Clone)]
pub struct ExtractorRun {
    pub node_path: String,
    pub extractor_id: &'static str,
    pub body_hash: String,
    pub sidecar_annotations_hash: String,
    pub settings_hash: String,
    pub ran_at: i64,
}

/// Prior runs keyed by node path, then qualified extractor id:
/// (body hash, sidecar annotations hash, settings hash).
pub type PriorRuns = HashMap<String, HashMap<String, (String, String, String)>>;

/// Qualified ids of the extractors that apply to a node, in registration
/// order (provider and kind preconditions of the built-in extractors).
fn applicable_extractors(lens: &str, kind: &str) -> Vec<&'static str> {
    let any = |ps: &[&str]| ps.contains(&lens);
    let mut out = Vec::new();
    if lens == "claude" {
        out.extend(["claude/at-directive", "claude/backtick-mention"]);
    }
    if kind == "agent" {
        out.push("claude/tools-counter");
    }
    if lens == "codex" {
        out.extend(["codex/backtick-dollar", "codex/dollar-skill"]);
    }
    if any(&["claude", "codex", "antigravity"]) {
        out.push("core/at-file");
    }
    out.push("core/backtick-path");
    if any(&["claude", "antigravity", "opencode"]) {
        out.push("core/backtick-slash");
    }
    out.extend(["core/external-url-counter", "core/markdown-link", "core/mcp-tools"]);
    if any(&["claude", "antigravity", "opencode"]) {
        out.push("core/slash-command");
    }
    out
}

/// The ignore chain (bundled defaults → .gitignore opt-in → config →
/// .skillmapignore) the walker and the watcher both apply.
pub fn ignore_matcher(opts: &ScanOpts) -> ignore::gitignore::Gitignore {
    let root = &opts.root;
    let read_opt = |name: &str| std::fs::read_to_string(root.join(name)).ok();
    let gitignore = if opts.respect_gitignore { read_opt(".gitignore") } else { None };
    let skillmapignore = read_opt(".skillmapignore");
    walk::build_ignore(
        root,
        &IgnoreLayers { gitignore: gitignore.as_deref(), config: &opts.ignore, skillmapignore: skillmapignore.as_deref() },
    )
}

/// sha256 of the canonical YAML of an extractor's resolved settings.
fn settings_hash(extractor: &str) -> String {
    let settings = if extractor == "core/external-url-counter" { json!({ "ignored-domains": [] }) } else { json!({}) };
    sha256_hex(&canon_yaml::dump(&settings))
}

fn sidecar_annotations_hash(node: &Node) -> String {
    let ann = node.sidecar.as_ref().and_then(|s| s.get("annotations")).filter(|a| a.is_object()).cloned().unwrap_or(json!({}));
    sha256_hex(&canon_yaml::dump(&ann))
}

/// Kernel issues a cache-hit node re-emits from the prior snapshot.
const CACHED_KERNEL_ISSUES: [&str; 4] = ["frontmatter-invalid", "frontmatter-malformed", "frontmatter-parse-error", "backtick-unbalanced"];

#[cfg(test)]
pub fn run(opts: &ScanOpts) -> ScanResult {
    run_incremental(opts, None, None, false).result
}

/// Scan, incrementally over the prior snapshot (skill-map's default): a
/// node whose path, body hash and frontmatter hash match the prior — and
/// whose recorded extractor runs still apply — is reused verbatim with its
/// prior links and kernel issues, and its extractors are skipped. Every
/// analyzer still runs over the merged graph. With a prior, renames and
/// orphaned history are detected too; `full` (--full) disables only the
/// reuse, never rename detection.
pub fn run_incremental(opts: &ScanOpts, prior: Option<&ScanResult>, prior_runs: Option<&PriorRuns>, full: bool) -> Outcome {
    let started = std::time::Instant::now();
    let cache_on = !full && prior.is_some_and(|p| p.active_provider.as_deref() == Some(opts.lens.as_str()));
    let prior_nodes: HashMap<&str, &Node> =
        prior.map(|p| p.nodes.iter().map(|n| (n.path.as_str(), n)).collect()).unwrap_or_default();
    let cached_tokens: HashMap<&str, (&str, &str, Triple)> = prior_nodes
        .iter()
        .filter(|_| cache_on)
        .filter_map(|(k, n)| Some((*k, (n.body_hash.as_str(), n.frontmatter_hash.as_str(), n.tokens?))))
        .collect();
    let root = &opts.root;
    let mut plugin_rt = crate::plugins::runtime::load(root);
    let ignore = ignore_matcher(opts);

    let mut nodes: Vec<Node> = Vec::new();
    let mut bodies: Vec<(String, usize)> = Vec::new(); // per real node: body, body_line_offset
    let mut node_issues: Vec<Vec<Issue>> = Vec::new(); // kernel-stamped, per real node
    let mut claimed: HashSet<String> = HashSet::new();
    let mut oversized = Vec::new();
    let mut files_walked = 0u64;
    let mut truncated = false;

    let active: Vec<&provider::Provider> = PROVIDERS.iter().filter(|p| !p.gated || p.id == opts.lens).collect();
    let bctx = BuildCtx { prior_nodes: &prior_nodes, cached_tokens: &cached_tokens, cache_on, lens: &opts.lens, tokenize: opts.tokenize };
    let mut push = |b: Built, claimed: &mut HashSet<String>| {
        claimed.insert(b.node.path.clone());
        bodies.push((b.body, b.body_line_offset));
        node_issues.push(b.issues);
        nodes.push(b.node);
    };

    match (opts.changed.as_ref().filter(|_| cache_on), prior) {
        // Watcher fast path: walk only the changed files, then re-inject
        // every other prior node (claimed by its prior owner, prior order).
        (Some(ch), Some(prior)) => {
            let expand = |paths: &mut dyn Iterator<Item = &String>| -> Vec<String> {
                paths
                    .filter_map(|p| match p.strip_suffix(".sm") {
                        Some(stem) => Some(format!("{stem}.md")).filter(|md| prior_nodes.contains_key(md.as_str())),
                        None => Some(p.clone()),
                    })
                    .collect()
            };
            let mut changed = expand(&mut ch.changed.iter());
            changed.extend(expand(&mut ch.removed.iter().filter(|p| p.ends_with(".sm"))));
            let mut seen = HashSet::new();
            changed.retain(|p| seen.insert(p.clone()));
            let removed: HashSet<&str> = ch.removed.iter().filter(|p| !p.ends_with(".sm")).map(String::as_str).collect();
            for p in &active {
                for rel in &changed {
                    let Some(rule) = p.read.iter().find(|r| rel.ends_with(r.extension)) else { continue };
                    let abs = root.join(rel);
                    let Ok(meta) = std::fs::metadata(&abs) else { continue };
                    if !meta.is_file() || meta.len() > opts.max_file_size {
                        continue;
                    }
                    files_walked += 1;
                    if claimed.contains(rel) {
                        continue;
                    }
                    let e = walk::Entry { rel: rel.clone(), abs, mtime_ms: walk::mtime_ms(&meta) };
                    if let Some(b) = build_entry(&e, p, rule, Gate::Off, &bctx) {
                        push(b, &mut claimed);
                    }
                }
            }
            let fallback = active.iter().find(|p| !p.gated).copied().unwrap_or(active[0]);
            for pn in &prior.nodes {
                let path = pn.path.as_str();
                if pn.r#virtual == Some(true) || seen.contains(path) || removed.contains(path) || claimed.contains(path) {
                    continue;
                }
                let owner = provider::get(&pn.provider).filter(|o| !o.gated || o.id == opts.lens).unwrap_or(fallback);
                let Some(rule) = owner.read.iter().find(|r| path.ends_with(r.extension)).or(owner.read.first()) else { continue };
                files_walked += 1;
                let abs = root.join(path);
                let Ok(meta) = std::fs::metadata(&abs) else { continue };
                let e = walk::Entry { rel: pn.path.clone(), abs, mtime_ms: walk::mtime_ms(&meta) };
                if let Some(b) = build_entry(&e, owner, rule, Gate::Forced, &bctx) {
                    push(b, &mut claimed);
                }
            }
        }
        _ => {
            'providers: for p in &active {
                for rule in p.read {
                    let wopts = WalkOpts {
                        ignore: &ignore,
                        extension: rule.extension,
                        max_file_size: opts.max_file_size,
                        follow_external_symlinks: opts.follow_external_symlinks,
                    };
                    let entries = walk::walk(std::slice::from_ref(root), &wopts, &mut oversized);
                    files_walked += entries.len() as u64;
                    // Parse in parallel; claim sequentially in walk order.
                    let built: Vec<Option<Built>> = entries
                        .par_iter()
                        .map(|e| if claimed.contains(&e.rel) { None } else { build_entry(e, p, rule, Gate::Mtime, &bctx) })
                        .collect();
                    for b in built.into_iter().flatten() {
                        if claimed.len() as u64 >= opts.max_scan {
                            truncated = true;
                            break 'providers;
                        }
                        push(b, &mut claimed);
                    }
                }
            }
        }
    }
    drop(push);

    // Sidecar overlays (real nodes only), then the per-node cache decision.
    let ran_at = now_ms();
    let mut extractor_runs = Vec::new();
    let mut cached_paths: HashSet<String> = HashSet::new();
    for (node, issues) in nodes.iter_mut().zip(node_issues.iter_mut()) {
        let (overlay, issue) = sidecar_overlay(root, node);
        node.sidecar = Some(overlay);
        let sidecar_hash = sidecar_annotations_hash(node);
        let applicable = applicable_extractors(&opts.lens, &node.kind);
        let hit = cache_on
            && prior_nodes.get(node.path.as_str()).is_some_and(|p| {
                (!opts.tokenize || p.tokens.is_some())
                    && p.body_hash == node.body_hash
                    && p.frontmatter_hash == node.frontmatter_hash
                    && prior_runs.is_none_or(|runs| {
                        let for_node = runs.get(&node.path);
                        applicable.iter().all(|id| {
                            for_node.and_then(|m| m.get(*id)).is_some_and(|(b, sc, st)| {
                                *b == node.body_hash && *sc == sidecar_hash && *st == settings_hash(id)
                            })
                        })
                    })
            });
        if hit {
            let p = prior_nodes[node.path.as_str()];
            let mut reused = p.clone();
            reused.sidecar = node.sidecar.take();
            *node = reused;
            *issues = prior
                .unwrap()
                .issues
                .iter()
                .filter(|i| CACHED_KERNEL_ISSUES.contains(&i.analyzer_id.as_str()) && i.node_ids.len() == 1 && i.node_ids[0] == node.path)
                .map(|i| Issue { severity: "warn", ..i.clone() })
                .collect();
            cached_paths.insert(node.path.clone());
        }
        issues.extend(issue);
        extractor_runs.extend(applicable.into_iter().map(|id| ExtractorRun {
            node_path: node.path.clone(),
            extractor_id: id,
            body_hash: node.body_hash.clone(),
            sidecar_annotations_hash: sidecar_hash.clone(),
            settings_hash: settings_hash(id),
            ran_at,
        }));
    }
    let kernel_issues: Vec<Issue> = node_issues.into_iter().flatten().collect();

    // Extraction, in node order; virtual nodes land right after their source.
    let outputs: Vec<extract::Output> = nodes
        .par_iter()
        .zip(bodies.par_iter())
        .map(|(n, (body, off))| {
            if cached_paths.contains(&n.path) {
                return extract::Output::default();
            }
            let input = NodeInput {
                path: &n.path,
                provider: provider::get(&n.provider).map_or("", |p| p.id),
                body,
                body_line_offset: *off,
                frontmatter: n.frontmatter.as_ref().unwrap_or(&Value::Null),
            };
            extract::run(&input, &opts.lens)
        })
        .collect();
    let mut signals = Vec::new();
    let mut with_virtual: Vec<Node> = Vec::with_capacity(nodes.len());
    for (node, out) in nodes.into_iter().zip(outputs) {
        signals.extend(out.signals);
        with_virtual.push(node);
        for v in out.virtual_nodes {
            if with_virtual.iter().any(|n| n.path == v.path) {
                continue;
            }
            with_virtual.push(virtual_node(v.path, v.kind, v.provider, v.derived_from, v.frontmatter));
        }
    }
    let mut nodes = with_virtual;
    // Cache hits: carry their prior virtual nodes, and their prior links
    // (ahead of the freshly resolved ones, in node order).
    let mut reused_links: Vec<Link> = Vec::new();
    if let Some(p) = prior.filter(|_| !cached_paths.is_empty()) {
        for n in &p.nodes {
            let keep = n.r#virtual == Some(true)
                && !nodes.iter().any(|m| m.path == n.path)
                && n.derived_from.as_ref().is_some_and(|d| d.iter().any(|s| cached_paths.contains(s)));
            if keep {
                nodes.push(n.clone());
            }
        }
        for n in nodes.iter().filter(|n| cached_paths.contains(&n.path)) {
            reused_links.extend(p.links.iter().filter(|l| l.source == n.path && !l.sources.is_empty()).cloned());
        }
    }
    apply_config_mcp_nodes(root, &opts.lens, &mut nodes);

    let (resolved, rejections) = resolve::resolve_signals(&signals);
    let (external, mut internal): (Vec<Link>, Vec<Link>) =
        resolved.into_iter().partition(|l| resolve::is_external_url(&l.target));
    reused_links.append(&mut internal);
    let internal = reused_links;
    let reserved = resolve::reserved_node_paths(&nodes);
    let mut links = resolve::dedupe_links(internal);
    let idx = resolve::build_indexes(&nodes);
    resolve::lift(&mut links, &idx, &opts.lens, &reserved);
    let mut links = resolve::prune_unresolved_code_triggers(links);
    let broken = resolve::broken_links(&links, &idx, &[root.as_path()]);
    recompute_counts(&mut nodes, &links, &external, &cached_paths);

    let mut issues = kernel_issues;
    let orphans = orphan_sidecars(root, Some(&ignore));
    // Project-local plugin analyzers run with the built-ins, ordered by
    // phase: score ops fold together before any detect analyzer reads
    // links, then detect, then aggregate.
    let plugin_score = if plugin_rt.is_empty() {
        Default::default()
    } else {
        plugin_rt.run_phase(&["score"], &nodes, &links, root)
    };
    let (score_issues, detect_issues) = analyze::run(analyze::Ctx {
        nodes: &nodes,
        links: &mut links,
        reserved_paths: &reserved,
        broken: &broken,
        signals: &signals,
        rejections: &rejections,
        orphan_sidecars: &orphans,
        ignored_references: &opts.ignored_references,
        plugin_ops: &plugin_score.ops,
    });
    let plugin_rest = if plugin_rt.is_empty() {
        Default::default()
    } else {
        plugin_rt.run_phase(&["detect", "aggregate"], &nodes, &links, root)
    };
    let plugin_issues = |out: crate::plugins::runtime::PhaseOutput| {
        out.issues.into_iter().filter(|(q, i)| !analyze::plugin_issue_suppressed(&nodes, q, i)).map(|(_, i)| i).collect::<Vec<_>>()
    };
    issues.extend(score_issues);
    issues.extend(plugin_issues(plugin_score));
    issues.extend(detect_issues);
    issues.extend(plugin_issues(plugin_rest));
    drop(plugin_rt);

    let renames = match prior {
        Some(p) => detect_renames(p, &nodes, &mut issues, &ignore),
        None => Vec::new(),
    };

    let result = ScanResult {
        schema_version: 1,
        scanned_at: now_ms(),
        roots: vec![".".into()],
        providers: PROVIDERS.iter().map(|p| p.id).collect(),
        scanned_by: ScannedBy { name: "agent-skill-map", version: env!("CARGO_PKG_VERSION"), spec_version: SPEC_VERSION },
        tokenizer: "cl100k_base",
        active_provider: Some(opts.lens.clone()),
        scan_ceiling: opts.max_scan,
        scan_truncated: truncated,
        max_render_nodes: opts.max_nodes,
        oversized_files: oversized.into_iter().map(|o| OversizedFile { path: o.path, bytes: o.bytes }).collect(),
        stats: Stats {
            files_walked,
            files_skipped: 0,
            files_oversized: 0,
            nodes_count: nodes.len() as u64,
            links_count: links.len() as u64,
            issues_count: issues.len() as u64,
            duration_ms: started.elapsed().as_millis() as u64,
        },
        nodes,
        links,
        issues,
    }
    .with_oversized_count();
    Outcome { result, renames, extractor_runs }
}

/// `renames.ts`: body-hash match → silent high-confidence rename; a unique
/// frontmatter-hash match → `auto-rename-medium`; several → ambiguous;
/// any other deleted path → `orphan` history.
fn detect_renames(prior: &ScanResult, current: &[Node], issues: &mut Vec<Issue>, ignore: &ignore::gitignore::Gitignore) -> Vec<Rename> {
    let prior_by: std::collections::HashMap<&str, &Node> = prior.nodes.iter().map(|n| (n.path.as_str(), n)).collect();
    let cur_by: std::collections::HashMap<&str, &Node> = current.iter().map(|n| (n.path.as_str(), n)).collect();
    let mut deleted: Vec<&str> = prior_by.keys().copied().filter(|p| !cur_by.contains_key(p)).collect();
    let mut added: Vec<&str> = cur_by.keys().copied().filter(|p| !prior_by.contains_key(p)).collect();
    deleted.sort_unstable();
    added.sort_unstable();
    let mut taken_from: HashSet<&str> = HashSet::new();
    let mut taken_to: HashSet<&str> = HashSet::new();
    let mut ops = Vec::new();
    for &from in &deleted {
        if let Some(&to) = added.iter().find(|&&to| !taken_to.contains(to) && cur_by[to].body_hash == prior_by[from].body_hash) {
            ops.push(Rename { from: from.into(), to: to.into() });
            taken_from.insert(from);
            taken_to.insert(to);
        }
    }
    let candidates: Vec<(&str, Vec<&str>)> = added
        .iter()
        .filter(|to| !taken_to.contains(**to))
        .map(|&to| {
            let m: Vec<&str> = deleted
                .iter()
                .copied()
                .filter(|from| !taken_from.contains(from) && prior_by[from].frontmatter_hash == cur_by[to].frontmatter_hash)
                .collect();
            (to, m)
        })
        .filter(|(_, m)| !m.is_empty())
        .collect();
    for (to, m) in &candidates {
        let remaining: Vec<&str> = m.iter().copied().filter(|f| !taken_from.contains(f)).collect();
        if remaining.len() == 1 {
            let from = remaining[0];
            issues.push(
                Issue::new("auto-rename-medium", "warn", vec![to.to_string()], format!("Auto-rename (medium confidence): {from} → {to}"))
                    .data(json!({ "from": from, "to": to, "confidence": 0.6 })),
            );
            ops.push(Rename { from: from.into(), to: to.to_string() });
            taken_from.insert(from);
            taken_to.insert(to);
        }
    }
    for (to, m) in &candidates {
        if taken_to.contains(to) {
            continue;
        }
        let remaining: Vec<&str> = m.iter().copied().filter(|f| !taken_from.contains(f)).collect();
        if remaining.len() > 1 {
            issues.push(
                Issue::new(
                    "auto-rename-ambiguous",
                    "warn",
                    vec![to.to_string()],
                    format!(
                        "Auto-rename ambiguous: {to} matches {} prior frontmatters; pick one with `sm orphans undo-rename {to} --from <old.path>`.",
                        remaining.len()
                    ),
                )
                .data(json!({ "to": to, "candidates": remaining })),
            );
        }
    }
    for &from in &deleted {
        if taken_from.contains(from) || ignore.matched_path_or_any_parents(from, false).is_ignore() {
            continue;
        }
        issues.push(
            Issue::new("orphan", "info", vec![from.into()], format!("Orphan history: {from} was deleted; no rename match found."))
                .data(json!({ "path": from })),
        );
    }
    ops
}

impl ScanResult {
    fn with_oversized_count(mut self) -> Self {
        self.stats.files_oversized = self.oversized_files.len() as u64;
        self
    }
}

/// How the prior owner may claim a walked file (skill-map's mtime gate).
#[derive(Clone, Copy, PartialEq)]
enum Gate {
    /// Fresh classification only (watcher's changed files).
    Off,
    /// The prior owner claims the file when its mtime did not move.
    Mtime,
    /// The prior owner claims the file unconditionally (re-injected node).
    Forced,
}

struct BuildCtx<'a> {
    prior_nodes: &'a HashMap<&'a str, &'a Node>,
    cached_tokens: &'a HashMap<&'a str, (&'a str, &'a str, Triple)>,
    cache_on: bool,
    lens: &'a str,
    tokenize: bool,
}

/// Classify and parse one walked file. An untouched file (mtime gate) is
/// claimed by the provider that owned it last time with its prior kind, so
/// it can land earlier in walk order than a fresh claim would.
fn build_entry(e: &walk::Entry, p: &provider::Provider, rule: &provider::ReadRule, gate: Gate, c: &BuildCtx) -> Option<Built> {
    let owner = c
        .prior_nodes
        .get(e.rel.as_str())
        .filter(|pn| c.cache_on && (gate == Gate::Forced || (gate == Gate::Mtime && pn.modified_at_ms == Some(e.mtime_ms))))
        .and_then(|pn| {
            let o = provider::get(&pn.provider).filter(|o| !o.gated || o.id == c.lens)?;
            Some((o.id, pn.kind.as_str()))
        });
    let (pid, kind) = match owner {
        Some(o) => o,
        None => (p.id, (p.classify)(&e.rel)?),
    };
    let text = std::fs::read_to_string(&e.abs).ok()?;
    let cached = c.cached_tokens.get(e.rel.as_str()).copied();
    Some(build_node(&e.rel, kind, pid, rule, &text, e.mtime_ms, c.tokenize, cached))
}

/// The body the scan hashed for `rel` under `provider` (submit-time drift
/// check): `Err` names why the file could not be read.
pub fn read_body(root: &Path, rel: &str, provider_id: &str) -> Result<String, String> {
    let text = std::fs::read_to_string(root.join(rel)).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("node {rel} is not on disk (deleted since the last scan?)"),
        _ => format!("cannot read {rel}: {e}"),
    })?;
    let p = provider::get(provider_id).unwrap_or(&PROVIDERS[PROVIDERS.len() - 1]);
    let rule = p.read.iter().find(|r| rel.ends_with(r.extension)).or(p.read.first());
    Ok(match rule.map(|r| (r.parser, r.body_field)) {
        Some((Parser::Toml, field)) => toml::from_str::<toml::Table>(&text)
            .ok()
            .and_then(|t| serde_json::to_value(t).ok())
            .and_then(|v| field.and_then(|f| v.get(f)).and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default(),
        _ => frontmatter::parse(&text).body.to_string(),
    })
}

pub fn sha256_pub(s: &str) -> String {
    sha256_hex(s)
}

#[allow(clippy::too_many_arguments)]
fn build_node(
    rel: &str,
    kind: &str,
    provider: &str,
    rule: &provider::ReadRule,
    text: &str,
    mtime_ms: i64,
    tokenize: bool,
    cached: Option<(&str, &str, Triple)>,
) -> Built {
    let (raw, body, body_line_offset, fm, declared, parse_error) = match rule.parser {
        Parser::FrontmatterYaml => {
            let p = frontmatter::parse(text);
            (p.raw.to_string(), p.body.to_string(), p.body_line_offset, p.value, p.declared, p.parse_error)
        }
        // TOML (Codex agents): the whole file is the "frontmatter"; the body is
        // the declared body field. Not yet verified against the oracle.
        Parser::Toml => {
            let fm = toml::from_str::<toml::Table>(text)
                .ok()
                .and_then(|t| serde_json::to_value(t).ok())
                .unwrap_or(Value::Object(Map::new()));
            let body = rule
                .body_field
                .and_then(|f| fm.get(f))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            (text.to_string(), body, 0, fm, true, None)
        }
    };
    let issues = diagnose::node_issues(&diagnose::NodeText {
        path: rel,
        provider,
        kind,
        declared,
        raw: &raw,
        body: &body,
        body_line_offset,
        frontmatter: &fm,
        parse_error: parse_error.as_deref(),
    });
    let fm_canonical = match &fm {
        Value::Object(m) if m.is_empty() && !raw.is_empty() => raw.clone(),
        v => canon_yaml::dump(v),
    };
    let bytes = Triple { frontmatter: raw.len() as u64, body: body.len() as u64, total: (raw.len() + body.len()) as u64 };
    let (body_hash, frontmatter_hash) = (sha256_hex(&body), sha256_hex(&fm_canonical));
    // A cache-reused node keeps its still-accurate prior counts even under
    // --no-tokens; only re-extracted nodes honour the flag.
    let tokens = match cached {
        Some((b, f, t)) if b == body_hash && f == frontmatter_hash => Some(t),
        _ => tokenize.then(|| {
            let (f, b) = (tokens::count(&raw), tokens::count(&body));
            Triple { frontmatter: f, body: b, total: f + b }
        }),
    };
    let node = Node {
        path: rel.to_string(),
        kind: kind.to_string(),
        provider: provider.to_string(),
        body_hash,
        frontmatter_hash,
        bytes,
        links_out_count: 0,
        links_in_count: 0,
        external_refs_count: 0,
        r#virtual: None,
        derived_from: None,
        frontmatter: Some(fm),
        modified_at_ms: Some(mtime_ms),
        tokens,
        sidecar: None,
        external_refs: None,
        loaded: false,
    };
    Built { node, body, body_line_offset, issues }
}

const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn virtual_node(path: String, kind: &str, provider: &str, derived_from: Vec<String>, frontmatter: Value) -> Node {
    Node {
        path,
        kind: kind.to_string(),
        provider: provider.to_string(),
        body_hash: ZERO_HASH.into(),
        frontmatter_hash: ZERO_HASH.into(),
        bytes: Triple::default(),
        links_out_count: 0,
        links_in_count: 0,
        external_refs_count: 0,
        r#virtual: Some(true),
        derived_from: Some(derived_from),
        frontmatter: Some(frontmatter),
        modified_at_ms: None,
        tokens: None,
        sidecar: None,
        external_refs: None,
        loaded: false,
    }
}

fn sidecar_overlay(root: &Path, node: &Node) -> (Value, Option<Issue>) {
    let sm = match node.path.strip_suffix(".md") {
        Some(stem) => format!("{stem}.sm"),
        None => format!("{}.sm", node.path),
    };
    let sm_abs = root.join(&sm);
    let Ok(text) = std::fs::read_to_string(&sm_abs) else { return (json!({ "present": false }), None) };
    let invalid = |message: String| {
        let issue = Issue::new("invalid-sidecar", "error", vec![node.path.clone()], message)
            .data(json!({ "sidecarPath": node.path }));
        (json!({ "present": true, "status": null, "annotations": null, "root": null }), Some(issue))
    };
    let shown = sm_abs.display();
    let parsed = match frontmatter::load(&text) {
        Ok(Some(Value::Object(m))) => m,
        Ok(_) => return invalid(format!("sidecar root must be a YAML mapping at {shown}")),
        Err(e) => return invalid(format!("malformed YAML in {shown}: {e}")),
    };
    let root_value = Value::Object(parsed.clone());
    let errors = validate::errors(validate::SIDECAR, &root_value);
    if !errors.is_empty() {
        let text: Vec<String> =
            errors.iter().map(|(p, m)| format!("{} {m}", if p.is_empty() { "(root)" } else { p })).collect();
        return invalid(format!("sidecar schema validation failed at {shown}: {}", text.join("; ")));
    }
    let identity = parsed.get("identity");
    let stored = |k: &str| identity.and_then(|i| i.get(k)).and_then(Value::as_str).unwrap_or("");
    let body_drift = stored("bodyHash") != node.body_hash;
    let fm_drift = stored("frontmatterHash") != node.frontmatter_hash;
    let status = match (body_drift, fm_drift) {
        (true, true) => "stale-both",
        (true, false) => "stale-body",
        (false, true) => "stale-frontmatter",
        (false, false) => "fresh",
    };
    let annotations = match parsed.get("annotations") {
        Some(Value::Object(a)) if !a.is_empty() => Value::Object(a.clone()),
        _ => Value::Null,
    };
    (json!({ "present": true, "status": status, "annotations": annotations, "root": root_value }), None)
}

/// Config-side MCP servers of the active lens (`mcp-config-nodes.ts`).
fn apply_config_mcp_nodes(root: &Path, lens: &str, nodes: &mut Vec<Node>) {
    let Some(p) = provider::get(lens) else { return };
    for (source, dialect) in p.mcp_sources {
        let Ok(text) = std::fs::read_to_string(root.join(source)) else { continue };
        let doc: Option<Value> = match dialect {
            McpDialect::Json => serde_json::from_str(&text).ok(),
            McpDialect::Toml => toml::from_str::<toml::Table>(&text).ok().and_then(|t| serde_json::to_value(t).ok()),
        };
        let Some(Value::Object(doc)) = doc else { continue };
        let Some(Value::Object(servers)) = doc.get("mcpServers").or(doc.get("mcp_servers")).or(doc.get("mcp")) else { continue };
        let mut seen = HashSet::new();
        for (name, raw) in servers {
            let server = name.trim().to_lowercase();
            if server.is_empty() || !seen.insert(server.clone()) {
                continue;
            }
            if raw.get("enabled") == Some(&Value::Bool(false)) {
                continue;
            }
            let fm = mcp_descriptor(&server, raw);
            let path = format!("mcp://{server}");
            if let Some(existing) = nodes.iter_mut().find(|n| n.path == path) {
                let mut merged = existing.frontmatter.take().and_then(|v| v.as_object().cloned()).unwrap_or_default();
                for (k, v) in fm {
                    merged.insert(k, v);
                }
                existing.frontmatter = Some(Value::Object(merged));
                let mut derived = vec![source.to_string()];
                for d in existing.derived_from.take().unwrap_or_default() {
                    if !derived.contains(&d) {
                        derived.push(d);
                    }
                }
                existing.derived_from = Some(derived);
                existing.r#virtual = Some(true);
            } else {
                nodes.push(virtual_node(path, "mcp", p.id, vec![source.to_string()], Value::Object(fm)));
            }
        }
    }
}

fn mcp_descriptor(server: &str, raw: &Value) -> Map<String, Value> {
    let mut fm = Map::new();
    fm.insert("name".into(), json!(server));
    let s = |k: &str| raw.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
    let (url, command) = (s("url"), s("command"));
    let transport = match s("type").map(str::to_lowercase).as_deref() {
        Some("stdio" | "local") => Some("stdio"),
        Some("http" | "streamable-http" | "sse" | "remote") => Some("http"),
        _ if url.is_some() => Some("http"),
        _ if command.is_some() => Some("stdio"),
        _ => None,
    };
    if let Some(t) = transport {
        fm.insert("transport".into(), json!(t));
    }
    if let Some(c) = command {
        fm.insert("command".into(), json!(c));
    }
    if let Some(args) = raw.get("args").and_then(Value::as_array) {
        let a: Vec<&Value> = args.iter().filter(|v| v.is_string()).collect();
        if !a.is_empty() {
            fm.insert("args".into(), json!(a));
        }
    }
    if let Some(env) = raw.get("env").and_then(Value::as_object) {
        let e: Map<String, Value> = env.iter().filter(|(_, v)| v.is_string()).map(|(k, v)| (k.clone(), v.clone())).collect();
        if !e.is_empty() {
            fm.insert("env".into(), Value::Object(e));
        }
    }
    if let Some(u) = url {
        fm.insert("url".into(), json!(u));
    }
    let tools = ["tools", "tools_provided", "toolsProvided"].iter().find_map(|k| raw.get(*k).and_then(Value::as_array));
    if let Some(t) = tools {
        let t: Vec<&Value> = t.iter().filter(|v| v.is_string()).collect();
        if !t.is_empty() {
            fm.insert("toolsProvided".into(), json!(t));
        }
    }
    fm
}

/// `.sm` files with neither a sibling `<stem>` nor `<stem>.md` file
/// (`discover-orphans.ts`): depth-first, byte-ordered, ignore-filtered.
/// `.sm` files whose `<stem>` / `<stem>.md` sibling is missing, as root-relative
/// paths in walk order. `ignore` = the scan's ignore chain (`None` walks
/// everything, as `sm sidecars prune` does).
pub fn orphan_sidecars(root: &Path, ignore: Option<&ignore::gitignore::Gitignore>) -> Vec<String> {
    fn visit(root: &Path, dir: &Path, ignore: Option<&ignore::gitignore::Gitignore>, out: &mut Vec<String>) {
        let Ok(read) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = read.filter_map(Result::ok).collect();
        entries.sort_by(|a, b| a.file_name().as_encoded_bytes().cmp(b.file_name().as_encoded_bytes()));
        for e in entries {
            let full = e.path();
            let rel = full.strip_prefix(root).unwrap_or(&full).to_string_lossy().replace('\\', "/");
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() || ignore.is_some_and(|ig| ig.matched_path_or_any_parents(&rel, ft.is_dir()).is_ignore()) {
                continue;
            }
            if ft.is_dir() {
                visit(root, &full, ignore, out);
            } else if ft.is_file() && rel.ends_with(".sm") {
                let stem = &full.to_string_lossy()[..full.to_string_lossy().len() - 3];
                let is_file = |p: String| std::fs::metadata(p).is_ok_and(|m| m.is_file());
                if !is_file(stem.to_string()) && !is_file(format!("{stem}.md")) {
                    out.push(rel);
                }
            }
        }
    }
    let mut out = Vec::new();
    visit(root, root, ignore, &mut out);
    out
}

fn recompute_counts(nodes: &mut [Node], links: &[Link], external: &[Link], cached: &HashSet<String>) {
    for n in nodes.iter_mut() {
        n.links_out_count = 0;
        n.links_in_count = 0;
    }
    for link in links {
        if let Some(n) = nodes.iter_mut().find(|n| n.path == link.source) {
            n.links_out_count += 1;
        }
        let target = link.resolved_target.as_deref().unwrap_or(&link.target);
        if let Some(n) = nodes.iter_mut().find(|n| n.path == target) {
            n.links_in_count += 1;
        }
    }
    for link in external {
        let Some(n) = nodes.iter_mut().find(|n| n.path == link.source && !cached.contains(&n.path)) else { continue };
        n.external_refs_count += 1;
        let r = ExternalRef {
            url: link.target.clone(),
            line: link.location.as_ref().map_or(1, |l| l.line),
            original_trigger: link.trigger.as_ref().map_or_else(|| link.raw.clone(), |t| t.original_trigger.clone()),
        };
        n.external_refs.get_or_insert_with(Vec::new).push(r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scrub(mut v: Value) -> Value {
        v["scannedAt"] = Value::Null;
        v["scannedBy"] = Value::Null;
        v["stats"]["durationMs"] = Value::Null;
        for n in v["nodes"].as_array_mut().unwrap() {
            if let Some(o) = n.as_object_mut() {
                o.remove("modifiedAtMs");
                // Known divergence: gpt-tokenizer's `\s` includes U+FEFF and
                // splits a BOM differently from tiktoken-rs (one token apart).
                if o.get("path").and_then(Value::as_str) == Some(".claude/skills/bom/SKILL.md") {
                    o.remove("tokens");
                }
            }
        }
        v
    }

    /// First difference per section between our scan and the original's
    /// golden for one fixture; empty when they match.
    pub fn diff_fixture(name: &str) -> Vec<String> {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("conformance");
        let golden: Value =
            serde_json::from_str(&std::fs::read_to_string(base.join(format!("golden/{name}/scan.json"))).unwrap()).unwrap();
        let opts = ScanOpts {
            root: base.join(format!("fixtures/{name}")),
            lens: golden["activeProvider"].as_str().unwrap().to_string(),
            ignore: vec![],
            respect_gitignore: false,
            max_scan: 5000,
            max_nodes: 256,
            max_file_size: 1 << 20,
            tokenize: true,
            follow_external_symlinks: false,
            changed: None,
            ignored_references: Vec::new(),
        };
        // Absolute paths in messages (invalid-sidecar) are recorded as `<ROOT>`.
        let ours_text = crate::jsjson::to_string(&run(&opts)).replace(&opts.root.display().to_string(), "<ROOT>");
        let ours = scrub(serde_json::from_str(&ours_text).unwrap());
        let golden = scrub(golden);
        let mut out = Vec::new();
        for key in ["nodes", "links", "issues"] {
            let (a, b) = (ours[key].as_array().unwrap(), golden[key].as_array().unwrap());
            for i in 0..a.len().max(b.len()) {
                let (x, y) = (a.get(i).map(crate::jsjson::to_string), b.get(i).map(crate::jsjson::to_string));
                if x != y {
                    out.push(format!("{name}.{key}[{i}] (ours {} vs golden {})\n  ours:   {}\n  golden: {}", a.len(), b.len(),
                        x.unwrap_or_default().chars().take(600).collect::<String>(), y.unwrap_or_default().chars().take(600).collect::<String>()));
                    break;
                }
            }
        }
        for key in ["stats", "providers", "roots", "activeProvider"] {
            if crate::jsjson::to_string(&ours[key]) != crate::jsjson::to_string(&golden[key]) {
                out.push(format!("{name}.{key}: ours {} vs golden {}", ours[key], golden[key]));
            }
        }
        out
    }

    #[test]
    fn fixtures_match_oracle() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("conformance/golden");
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("scan.json").exists())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let failures: Vec<String> = names.iter().flat_map(|n| diff_fixture(n)).collect();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
