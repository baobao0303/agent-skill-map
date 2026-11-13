//! Deterministic analyzers (spec_doc §12.2). Messages are copied verbatim
//! from skill-map's `*.texts.ts`. Order: the `score` phase (name-reserved,
//! reference-broken) folds its confidence deltas first, then `detect`
//! analyzers run in registration order; their issues are emitted in
//! registration order overall.
//!
//! Not ported: `schema-violation` re-validates the pipeline's own nodes and
//! links against node/link schemas, which the typed model here guarantees;
//! `observed-*` need the session journal (phase 6); `issue-counter`,
//! `link-counter` and `annotation-stale` only emit UI contributions.

use crate::extract::Signal;
use crate::format::locale_compare;
use crate::model::{Issue, Link, Node};
use crate::norm::normalize_trigger;
use crate::provider::{self, Identifier};
use crate::resolve::Rejection;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub struct Ctx<'a> {
    pub nodes: &'a [Node],
    pub links: &'a mut [Link],
    pub reserved_paths: &'a [String],
    pub broken: &'a [usize],
    pub signals: &'a [Signal],
    pub rejections: &'a [Rejection],
    pub orphan_sidecars: &'a [String],
}

const RESERVED_PENALTY: f64 = 0.9;
const BROKEN_PENALTY: f64 = 0.75;

pub fn run(ctx: Ctx) -> Vec<Issue> {
    // score phase: compute, then fold before any detect analyzer reads links.
    let mut deltas = vec![0.0f64; ctx.links.len()];
    let reserved = name_reserved(&ctx, &mut deltas);
    let broken = reference_broken(&ctx, &mut deltas);
    for (link, delta) in ctx.links.iter_mut().zip(deltas) {
        link.confidence = (link.confidence + delta).clamp(0.0, 1.0);
    }
    let ctx = &ctx;
    // registration order (alphabetical by id)
    let mut issues = Vec::new();
    issues.extend(annotation_field_unknown(ctx));
    issues.extend(annotation_orphan(ctx));
    issues.extend(extractor_collision(ctx));
    issues.extend(link_kind_conflict(ctx));
    issues.extend(link_self_loop(ctx));
    issues.extend(name_collision(ctx));
    issues.extend(name_mismatch(ctx));
    issues.extend(reserved);
    issues.extend(node_stability(ctx));
    issues.extend(broken);
    issues.extend(reference_redundant(ctx));
    // The original sorts analyzers by phase before running them, so score
    // issues come first.
    let (score, detect): (Vec<Issue>, Vec<Issue>) =
        issues.into_iter().partition(|i| i.analyzer_id == "name-reserved" || i.analyzer_id == "reference-broken");
    // Operator dismissals apply to every analyzer: an issue whose
    // `data.target` matches an issueSuppressions entry of one of its nodes.
    score
        .into_iter()
        .chain(detect)
        .filter(|i| {
            let Some(target) = i.get("data").and_then(|d| d.get("target")).and_then(Value::as_str) else { return true };
            !i.node_ids.iter().any(|n| is_suppressed(ctx, n, &i.analyzer_id, target))
        })
        .collect()
}

/// `formatFinding`: optional `` `subject`:\n`` head, optional `L<n>, …: ` prefix.
fn format_finding(subject: Option<&str>, lines: &[u64], body: &str) -> String {
    let head = subject.map(|s| format!("`{s}`:\n")).unwrap_or_default();
    let loc = if lines.is_empty() {
        String::new()
    } else {
        format!("L{}: ", lines.iter().map(u64::to_string).collect::<Vec<_>>().join(", "))
    };
    format!("{head}{loc}{body}")
}

fn link_lines(link: &Link) -> Vec<u64> {
    let mut lines: BTreeSet<u64> = link.occurrences.iter().filter_map(|o| o.location.as_ref().map(|l| l.line)).collect();
    if lines.is_empty()
        && let Some(l) = &link.location
    {
        lines.insert(l.line);
    }
    lines.into_iter().collect()
}

fn name_reserved(ctx: &Ctx, deltas: &mut [f64]) -> Vec<Issue> {
    let mut issues = Vec::new();
    for path in ctx.reserved_paths {
        let Some(node) = ctx.nodes.iter().find(|n| &n.path == path) else { continue };
        let body = format!("Reserved name; this {} name is shadowed by the {} runtime built-in", node.kind, node.provider);
        issues.push(
            Issue::new("name-reserved", "warn", vec![node.path.clone()], format_finding(None, &[], &body))
                .fix("Rename the file or its frontmatter.name.")
                .data(json!({ "provider": node.provider, "kind": node.kind, "surface": "target" })),
        );
    }
    for (i, link) in ctx.links.iter().enumerate() {
        let Some(rp) = &link.resolved_target else { continue };
        if !ctx.reserved_paths.contains(rp) {
            continue;
        }
        let Some(node) = ctx.nodes.iter().find(|n| &n.path == rp) else { continue };
        deltas[i] -= RESERVED_PENALTY;
        let body = format!(
            "Reserved name; resolves to the {} built-in ({} `{}`), the built-in shadows this edge",
            node.provider, node.kind, node.path
        );
        issues.push(
            Issue::new("name-reserved", "warn", vec![link.source.clone()], format_finding(Some(&link.target), &link_lines(link), &body))
                .fix("Rename the target file or its frontmatter.name.")
                .data(json!({
                    "target": link.target, "kind": link.kind, "surface": "source",
                    "reservedPath": node.path, "reservedProvider": node.provider, "reservedKind": node.kind,
                })),
        );
    }
    issues
}

/// `isCodeShapedAtToken` on the link target.
fn is_code_shaped_at_token(raw: &str) -> bool {
    let Some(bare) = raw.strip_prefix('@') else { return false };
    if bare.is_empty() {
        return false;
    }
    if !bare.contains('/') {
        return bare.chars().any(|c| c.is_ascii_uppercase());
    }
    let has_ext_tail =
        raw.rsplit_once('.').is_some_and(|(_, ext)| !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric()));
    if has_ext_tail {
        return false;
    }
    let seg = |s: &str| {
        let mut c = s.chars();
        c.next().is_some_and(|f| f.is_ascii_lowercase() || f.is_ascii_digit())
            && c.all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || matches!(x, '.' | '_' | '-'))
    };
    matches!(bare.split_once('/'), Some((scope, name)) if seg(scope) && seg(name))
}

/// Operator dismissals: `annotations.issueSuppressions` entries on the node's sidecar.
fn is_suppressed(ctx: &Ctx, node_path: &str, analyzer: &str, value: &str) -> bool {
    let Some(node) = ctx.nodes.iter().find(|n| n.path == node_path) else { return false };
    let entries = node
        .sidecar
        .as_ref()
        .and_then(|s| s.get("annotations"))
        .and_then(|a| a.get("issueSuppressions"))
        .and_then(Value::as_array);
    entries.into_iter().flatten().any(|e| {
        let a = e.get("analyzer").and_then(Value::as_str).unwrap_or("");
        let v = e.get("value").and_then(Value::as_str).unwrap_or("");
        v == value && (a == analyzer || a == format!("core/{analyzer}"))
    })
}

fn reference_broken(ctx: &Ctx, deltas: &mut [f64]) -> Vec<Issue> {
    let mut issues = Vec::new();
    for (i, link) in ctx.links.iter().enumerate() {
        if !ctx.broken.contains(&i) || is_suppressed(ctx, &link.source, "reference-broken", &link.target) {
            continue;
        }
        deltas[i] -= BROKEN_PENALTY;
        let code_shaped = link.trigger.as_ref().is_some_and(|t| t.normalized_trigger.starts_with('@'))
            && is_code_shaped_at_token(&link.target);
        let kind_label = match link.kind {
            "references" => "reference".to_string(),
            "mentions" => "mention".to_string(),
            "invokes" => "invocation".to_string(),
            "points" => "pointer".to_string(),
            other => format!("{other} link"),
        };
        let (body, fix) = if code_shaped {
            (
                format!("Broken {kind_label}; target not found, and the token looks like a code identifier or npm package rather than a reference"),
                "Dismiss this issue if the token is intentional prose, or fix the name / remove the mention.",
            )
        } else {
            (
                format!("Broken {kind_label}; target not found in the graph or on disk"),
                "Fix the path or name, remove the broken link, or add its folder under Folders for link validation.",
            )
        };
        issues.push(
            Issue::new(
                "reference-broken",
                if code_shaped { "warn" } else { "error" },
                vec![link.source.clone()],
                format_finding(Some(&link.target), &link_lines(link), &body),
            )
            .fix(fix)
            .data(json!({
                "target": link.target,
                "kind": link.kind,
                "trigger": link.trigger.as_ref().map(|t| t.normalized_trigger.clone()),
            })),
        );
    }
    issues
}

const ANNOTATION_KEYS: &[&str] = &[
    "authors", "docsUrl", "issueSuppressions", "license", "source", "sourceVersion", "stability", "suppressions", "tags", "version",
];

fn annotation_field_unknown(ctx: &Ctx) -> Vec<Issue> {
    let mut issues = Vec::new();
    for node in ctx.nodes {
        let Some(root) = node.sidecar.as_ref().and_then(|s| s.get("root")).and_then(Value::as_object) else { continue };
        if let Some(Value::Object(ann)) = root.get("annotations") {
            for key in ann.keys().filter(|k| !ANNOTATION_KEYS.contains(&k.as_str())) {
                issues.push(
                    Issue::new(
                        "annotation-field-unknown",
                        "warn",
                        vec![node.path.clone()],
                        format_finding(Some(key), &[], "Unknown sidecar key; not in the annotations catalog"),
                    )
                    .data(json!({ "surface": "annotations", "key": key })),
                );
            }
        }
        for key in root.keys().filter(|k| !["identity", "annotations", "settings", "audit"].contains(&k.as_str())) {
            issues.push(
                Issue::new(
                    "annotation-field-unknown",
                    "warn",
                    vec![node.path.clone()],
                    format_finding(
                        Some(key),
                        &[],
                        "Unknown top-level sidecar key; not a reserved block, plugin namespace, or root contribution",
                    ),
                )
                .data(json!({ "surface": "root", "key": key })),
            );
        }
    }
    issues
}

fn annotation_orphan(ctx: &Ctx) -> Vec<Issue> {
    ctx.orphan_sidecars
        .iter()
        .map(|rel| {
            let md = match rel.strip_suffix(".sm") {
                Some(stem) => format!("{stem}.md"),
                None => format!("{rel}.md"),
            };
            Issue::new(
                "annotation-orphan",
                "warn",
                vec![md.clone()],
                format_finding(Some(rel), &[], "Orphan sidecar; no matching markdown node"),
            )
            .fix("Run `sm sidecars prune` to remove orphan sidecars.")
            .data(json!({ "sidecarPath": rel, "expectedMdPath": md }))
        })
        .collect()
}

fn extractor_collision(ctx: &Ctx) -> Vec<Issue> {
    ctx.rejections
        .iter()
        .map(|r| {
            let s = &ctx.signals[r.signal];
            let c = &s.candidates[0];
            let range = s.range.as_ref();
            let loser_range = range.map_or("unknown".to_string(), |r| format!("{}-{}", r.start, r.end));
            let body = format!(
                "Overlap collision; {} (at {loser_range}) lost to {} (at {}-{}) by {}, only the winning edge persists",
                c.extractor, r.winner_extractor, r.winner_range.0, r.winner_range.1, r.reason
            );
            let lines: Vec<u64> = range.map(|r| r.line).into_iter().collect();
            Issue::new("extractor-collision", "warn", vec![s.source.clone()], format_finding(Some(&s.raw), &lines, &body))
                .fix("Rephrase one of the overlapping tokens, or accept the winner.")
                .data(json!({
                    "loser": {
                        "extractorId": c.extractor,
                        "raw": s.raw,
                        "range": range.map(|r| json!({ "start": r.start, "end": r.end, "line": r.line })),
                        "candidate": { "kind": c.kind, "target": c.target, "confidence": c.confidence },
                    },
                    "winner": { "extractorId": r.winner_extractor, "range": { "start": r.winner_range.0, "end": r.winner_range.1 } },
                    "reason": r.reason,
                }))
        })
        .collect()
}

fn link_kind_conflict(ctx: &Ctx) -> Vec<Issue> {
    let mut groups: Vec<((&str, &str), Vec<&Link>)> = Vec::new();
    for l in ctx.links.iter().filter(|l| l.kind != "points") {
        match groups.iter_mut().find(|(k, _)| *k == (l.source.as_str(), l.target.as_str())) {
            Some((_, g)) => g.push(l),
            None => groups.push(((&l.source, &l.target), vec![l])),
        }
    }
    let mut issues = Vec::new();
    for ((source, target), links) in groups {
        let mut variants: Vec<(&str, Vec<String>, f64)> = Vec::new();
        for l in &links {
            match variants.iter_mut().find(|(k, _, _)| *k == l.kind) {
                Some((_, sources, conf)) => {
                    for s in &l.sources {
                        if !sources.contains(s) {
                            sources.push(s.clone());
                        }
                    }
                    *conf = conf.max(l.confidence);
                }
                None => variants.push((l.kind, l.sources.clone(), l.confidence)),
            }
        }
        if links.len() < 2 || variants.len() < 2 {
            continue;
        }
        for v in variants.iter_mut() {
            v.1.sort();
        }
        variants.sort_by(|a, b| locale_compare(a.0, b.0));
        let kind_list = variants.iter().map(|v| v.0).collect::<Vec<_>>().join(" / ");
        let body = format!("Conflicting link kind; detectors disagree ({kind_list})");
        let data_variants: Vec<Value> =
            variants.iter().map(|(k, s, c)| json!({ "kind": k, "sources": s, "confidence": c })).collect();
        issues.push(
            Issue::new("link-kind-conflict", "warn", vec![source.to_string(), target.to_string()], format_finding(Some(target), &[], &body))
                .fix("Remove one of the conflicting sources to settle on a single kind, or ignore the conflict deliberately.")
                .data(json!({ "source": source, "target": target, "variants": data_variants })),
        );
    }
    issues
}

fn link_self_loop(ctx: &Ctx) -> Vec<Issue> {
    ctx.links
        .iter()
        .filter(|l| l.source == l.target || l.resolved_target.as_deref() == Some(l.source.as_str()))
        .filter(|l| l.occurrences.is_empty() || !l.occurrences.iter().all(|o| o.context.is_some()))
        .map(|l| {
            let subject = l.trigger.as_ref().map_or(l.target.as_str(), |t| t.original_trigger.as_str());
            Issue::new(
                "link-self-loop",
                "warn",
                vec![l.source.clone()],
                format_finding(Some(subject), &link_lines(l), "Self-reference; the skill/command invokes itself, potential loop"),
            )
            .fix("Remove the token or ignore the self-reference deliberately.")
            .data(json!({
                "target": l.target,
                "resolvedTarget": l.resolved_target.clone().unwrap_or_else(|| l.target.clone()),
                "kind": l.kind,
            }))
        })
        .collect()
}

fn frontmatter_name(node: &Node) -> Option<&str> {
    node.frontmatter.as_ref()?.get("name")?.as_str().filter(|s| !s.is_empty())
}

fn path_identifier(node: &Node, source: Identifier) -> Option<String> {
    let base = node.path.rsplit('/').next().unwrap_or("");
    let raw = match source {
        Identifier::FilenameBasename => match base.rfind('.') {
            Some(i) if i > 0 => base[..i].to_string(),
            _ => base.to_string(),
        },
        Identifier::Dirname => {
            let dir = crate::extract::posix_dirname(&node.path);
            if dir == "." || dir == "/" {
                return None;
            }
            dir.rsplit('/').next().unwrap_or("").to_string()
        }
        Identifier::FrontmatterName => frontmatter_name(node)?.to_string(),
    };
    (!raw.is_empty()).then_some(raw)
}

fn source_label(source: Identifier) -> &'static str {
    match source {
        Identifier::FrontmatterName => "frontmatter.name",
        Identifier::FilenameBasename => "filename-basename",
        Identifier::Dirname => "dirname",
    }
}

fn name_collision(ctx: &Ctx) -> Vec<Issue> {
    // name → claims (path, kind, source), first-seen name order
    let mut by_name: Vec<(String, Vec<(String, String, Identifier)>)> = Vec::new();
    for node in ctx.nodes {
        let Some(kind) = provider::get(&node.provider).and_then(|p| p.kind(&node.kind)) else { continue };
        if !kind.identifiers.contains(&Identifier::FrontmatterName) {
            continue;
        }
        for &source in kind.identifiers {
            if source != Identifier::FrontmatterName && node.r#virtual == Some(true) {
                continue;
            }
            let Some(raw) = path_identifier(node, source) else { continue };
            let name = normalize_trigger(&raw);
            if name.is_empty() {
                continue;
            }
            let claim = (node.path.clone(), node.kind.clone(), source);
            match by_name.iter_mut().find(|(n, _)| *n == name) {
                Some((_, claims)) => claims.push(claim),
                None => by_name.push((name, vec![claim])),
            }
        }
    }
    let mut issues = Vec::new();
    for (name, claims) in by_name {
        let mut distinct: Vec<(String, String, Identifier)> = Vec::new();
        for c in claims {
            match distinct.iter_mut().find(|d| d.0 == c.0) {
                Some(d) if d.2 != Identifier::FrontmatterName && c.2 == Identifier::FrontmatterName => *d = c,
                Some(_) => {}
                None => distinct.push(c),
            }
        }
        distinct.sort_by(|a, b| locale_compare(&a.0, &b.0));
        if distinct.len() < 2 || !distinct.iter().any(|c| c.2 == Identifier::FrontmatterName) {
            continue;
        }
        let paths: Vec<String> = distinct.iter().map(|c| c.0.clone()).collect();
        let declared = distinct.iter().filter(|c| c.2 == Identifier::FrontmatterName).count();
        let is_error = declared >= 2;
        let body = if is_error {
            format!("Name collision; {} nodes declare the same name: {}", paths.len(), paths.join(", "))
        } else {
            format!(
                "Name shadowing; a declared name matches the file-derived handle of another node ({} claimants): {}",
                paths.len(),
                paths.join(", ")
            )
        };
        let claims: Vec<Value> =
            distinct.iter().map(|(p, k, s)| json!({ "path": p, "kind": k, "source": source_label(*s) })).collect();
        issues.push(
            Issue::new("name-collision", if is_error { "error" } else { "warn" }, paths.clone(), format_finding(Some(&name), &[], &body))
                .data(json!({ "name": name, "claims": claims })),
        );
    }
    issues
}

fn name_mismatch(ctx: &Ctx) -> Vec<Issue> {
    let mut issues = Vec::new();
    for node in ctx.nodes {
        let Some(kind) = provider::get(&node.provider).and_then(|p| p.kind(&node.kind)) else { continue };
        if !kind.identifier_mismatch_warn || !kind.identifiers.contains(&Identifier::FrontmatterName) || node.r#virtual == Some(true) {
            continue;
        }
        let Some(declared_name) = frontmatter_name(node) else { continue };
        let declared = normalize_trigger(declared_name);
        if declared.is_empty() {
            continue;
        }
        for &source in kind.identifiers.iter().filter(|s| **s != Identifier::FrontmatterName) {
            let Some(derived_name) = path_identifier(node, source) else { continue };
            let derived = normalize_trigger(&derived_name);
            if derived.is_empty() || derived == declared {
                continue;
            }
            let label = if source == Identifier::Dirname { "parent directory name" } else { "filename stem" };
            let body = format!(
                "Declared name differs from the {label} \"{derived_name}\"; the node answers to both names when references resolve"
            );
            issues.push(
                Issue::new("name-mismatch", "warn", vec![node.path.clone()], format_finding(Some(declared_name), &[], &body))
                    .data(json!({ "declaredName": declared_name, "derivedName": derived_name, "derivedSource": source_label(source) }))
                    .fix("Rename the file or folder to match the declared name, or align `name` with the path; keep the override if the dual identity is deliberate."),
            );
        }
    }
    issues
}

fn stability(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str).filter(|s| matches!(*s, "experimental" | "stable" | "deprecated"))
}

fn node_stability(ctx: &Ctx) -> Vec<Issue> {
    ctx.nodes
        .iter()
        .filter(|n| {
            let from_annotations = n.sidecar.as_ref().and_then(|s| s.get("annotations")).and_then(|a| a.get("stability"));
            let legacy = n.frontmatter.as_ref().and_then(|f| f.get("metadata")).and_then(|m| m.get("stability"));
            stability(from_annotations).or(stability(legacy)) == Some("deprecated")
        })
        .map(|n| {
            Issue::new("node-stability", "warn", vec![n.path.clone()], format_finding(None, &[], "Marked deprecated; avoid using it"))
                .data(json!({ "stability": "deprecated" }))
        })
        .collect()
}

fn reference_redundant(ctx: &Ctx) -> Vec<Issue> {
    let mut groups: Vec<((&str, &str), Vec<&Link>)> = Vec::new();
    for l in ctx.links.iter() {
        let Some(resolved) = l.resolved_target.as_deref() else { continue };
        match groups.iter_mut().find(|(k, _)| *k == (l.source.as_str(), resolved)) {
            Some((_, g)) => g.push(l),
            None => groups.push(((&l.source, resolved), vec![l])),
        }
    }
    let mut issues = Vec::new();
    for ((source, resolved), links) in groups {
        let total: usize = links.iter().map(|l| l.occurrences.len().max(1)).sum();
        if total < 2 {
            continue;
        }
        // (kind, trigger, extractor, line)
        let mut flat: Vec<(&str, String, String, Option<u64>)> = Vec::new();
        for l in &links {
            if l.occurrences.is_empty() {
                let trigger = l.trigger.as_ref().map_or(l.target.clone(), |t| t.original_trigger.clone());
                flat.push((l.kind, trigger, l.sources.first().cloned().unwrap_or("unknown".into()), l.location.as_ref().map(|x| x.line)));
            } else {
                for o in &l.occurrences {
                    flat.push((l.kind, o.original_trigger.clone(), o.extractor.clone(), o.location.as_ref().map(|x| x.line)));
                }
            }
        }
        flat.sort_by(|a, b| a.3.unwrap_or(u64::MAX).cmp(&b.3.unwrap_or(u64::MAX)).then_with(|| locale_compare(&a.1, &b.1)));
        let mut by_trigger: Vec<(&str, Vec<String>)> = Vec::new();
        for (_, trigger, _, line) in &flat {
            let shown = line.map_or("?".to_string(), |l| l.to_string());
            match by_trigger.iter_mut().find(|(t, _)| *t == trigger.as_str()) {
                Some((_, lines)) => lines.push(shown),
                None => by_trigger.push((trigger, vec![shown])),
            }
        }
        let listed = by_trigger.iter().map(|(t, lines)| format!("`{t}` (L{})", lines.join(", "))).collect::<Vec<_>>().join(", ");
        let body = format!("Redundant links; the target is reached {} times: {listed}", flat.len());
        let occurrences: Vec<Value> =
            flat.iter().map(|(k, t, e, l)| json!({ "kind": k, "trigger": t, "line": l, "extractor": e })).collect();
        issues.push(
            Issue::new("reference-redundant", "info", vec![source.to_string()], format_finding(Some(resolved), &[], &body))
                .fix("Consolidate the links into one, or keep the overlap deliberately.")
                .data(json!({ "target": resolved, "resolvedTarget": resolved, "occurrences": occurrences })),
        );
    }
    issues
}
