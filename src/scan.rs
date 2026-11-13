//! One scan: walk → classify → extract → resolve → analyze (spec_doc §8).

use crate::extract::{self, NodeInput};
use crate::model::{ExternalRef, Issue, Link, Node, OversizedFile, ScanResult, ScannedBy, Stats, Triple};
use crate::provider::{self, McpDialect, PROVIDERS, Parser};
use crate::walk::{self, IgnoreLayers, WalkOpts};
use crate::{analyze, canon_yaml, diagnose, frontmatter, resolve, tokens, validate};
use rayon::prelude::*;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
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
}

fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

struct Built {
    node: Node,
    body: String,
    body_line_offset: usize,
    issues: Vec<Issue>,
}

pub fn run(opts: &ScanOpts) -> ScanResult {
    let started = std::time::Instant::now();
    let root = &opts.root;
    let read_opt = |name: &str| std::fs::read_to_string(root.join(name)).ok();
    let gitignore = if opts.respect_gitignore { read_opt(".gitignore") } else { None };
    let skillmapignore = read_opt(".skillmapignore");
    let ignore = walk::build_ignore(
        root,
        &IgnoreLayers { gitignore: gitignore.as_deref(), config: &opts.ignore, skillmapignore: skillmapignore.as_deref() },
    );

    let mut nodes: Vec<Node> = Vec::new();
    let mut bodies: Vec<(String, usize)> = Vec::new(); // per real node: body, body_line_offset
    let mut node_issues: Vec<Vec<Issue>> = Vec::new(); // kernel-stamped, per real node
    let mut claimed: HashSet<String> = HashSet::new();
    let mut oversized = Vec::new();
    let mut files_walked = 0u64;
    let mut truncated = false;

    'providers: for p in PROVIDERS.iter().filter(|p| !p.gated || p.id == opts.lens) {
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
                .map(|e| {
                    if claimed.contains(&e.rel) {
                        return None;
                    }
                    let kind = (p.classify)(&e.rel)?;
                    let text = std::fs::read_to_string(&e.abs).ok()?;
                    Some(build_node(&e.rel, kind, p.id, rule, &text, e.mtime_ms, opts.tokenize))
                })
                .collect();
            for b in built.into_iter().flatten() {
                if nodes.len() as u64 >= opts.max_scan {
                    truncated = true;
                    break 'providers;
                }
                claimed.insert(b.node.path.clone());
                bodies.push((b.body, b.body_line_offset));
                node_issues.push(b.issues);
                nodes.push(b.node);
            }
        }
    }

    // Sidecar overlays (real nodes only).
    for (node, issues) in nodes.iter_mut().zip(node_issues.iter_mut()) {
        let (overlay, issue) = sidecar_overlay(root, node);
        node.sidecar = Some(overlay);
        issues.extend(issue);
    }
    let kernel_issues: Vec<Issue> = node_issues.into_iter().flatten().collect();

    // Extraction, in node order; virtual nodes land right after their source.
    let outputs: Vec<extract::Output> = nodes
        .par_iter()
        .zip(bodies.par_iter())
        .map(|(n, (body, off))| {
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
    apply_config_mcp_nodes(root, &opts.lens, &mut nodes);

    let (resolved, rejections) = resolve::resolve_signals(&signals);
    let (external, internal): (Vec<Link>, Vec<Link>) =
        resolved.into_iter().partition(|l| resolve::is_external_url(&l.target));
    let reserved = resolve::reserved_node_paths(&nodes);
    let mut links = resolve::dedupe_links(internal);
    let idx = resolve::build_indexes(&nodes);
    resolve::lift(&mut links, &idx, &opts.lens, &reserved);
    let mut links = resolve::prune_unresolved_code_triggers(links);
    let broken = resolve::broken_links(&links, &idx, &[root.as_path()]);
    recompute_counts(&mut nodes, &links, &external);

    let mut issues = kernel_issues;
    let orphans = orphan_sidecars(root, &ignore);
    issues.extend(analyze::run(analyze::Ctx {
        nodes: &nodes,
        links: &mut links,
        reserved_paths: &reserved,
        broken: &broken,
        signals: &signals,
        rejections: &rejections,
        orphan_sidecars: &orphans,
    }));

    ScanResult {
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
    .with_oversized_count()
}

impl ScanResult {
    fn with_oversized_count(mut self) -> Self {
        self.stats.files_oversized = self.oversized_files.len() as u64;
        self
    }
}

fn build_node(rel: &str, kind: &str, provider: &str, rule: &provider::ReadRule, text: &str, mtime_ms: i64, tokenize: bool) -> Built {
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
    let tokens = tokenize.then(|| {
        let (f, b) = (tokens::count(&raw), tokens::count(&body));
        Triple { frontmatter: f, body: b, total: f + b }
    });
    let node = Node {
        path: rel.to_string(),
        kind: kind.to_string(),
        provider: provider.to_string(),
        body_hash: sha256_hex(&body),
        frontmatter_hash: sha256_hex(&fm_canonical),
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
fn orphan_sidecars(root: &Path, ignore: &ignore::gitignore::Gitignore) -> Vec<String> {
    fn visit(root: &Path, dir: &Path, ignore: &ignore::gitignore::Gitignore, out: &mut Vec<String>) {
        let Ok(read) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = read.filter_map(Result::ok).collect();
        entries.sort_by(|a, b| a.file_name().as_encoded_bytes().cmp(b.file_name().as_encoded_bytes()));
        for e in entries {
            let full = e.path();
            let rel = full.strip_prefix(root).unwrap_or(&full).to_string_lossy().replace('\\', "/");
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() || ignore.matched_path_or_any_parents(&rel, ft.is_dir()).is_ignore() {
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

fn recompute_counts(nodes: &mut [Node], links: &[Link], external: &[Link]) {
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
        let Some(n) = nodes.iter_mut().find(|n| n.path == link.source) else { continue };
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
