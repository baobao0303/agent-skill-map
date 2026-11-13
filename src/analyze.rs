//! Deterministic analyzers (spec_doc §12.2). Messages are copied verbatim
//! from skill-map's `*.texts.ts`. Phase order: score → detect → aggregate;
//! within a phase, registration order.

use crate::model::{Fix, Issue, Link, Node};
use serde_json::json;
use std::collections::BTreeSet;

pub struct Ctx<'a> {
    pub nodes: &'a [Node],
    pub links: &'a mut [Link],
    pub reserved_paths: &'a [String],
    pub broken: &'a [usize],
}

const RESERVED_PENALTY: f64 = 0.9;
const BROKEN_PENALTY: f64 = 0.75;

pub fn run(ctx: Ctx) -> Vec<Issue> {
    let mut deltas = vec![0.0f64; ctx.links.len()];
    let mut issues = Vec::new();
    // score phase
    name_reserved(&ctx, &mut deltas, &mut issues);
    reference_broken(&ctx, &mut deltas, &mut issues);
    for (link, delta) in ctx.links.iter_mut().zip(deltas) {
        link.confidence = (link.confidence + delta).clamp(0.0, 1.0);
    }
    issues
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

fn name_reserved(ctx: &Ctx, deltas: &mut [f64], issues: &mut Vec<Issue>) {
    if ctx.reserved_paths.is_empty() {
        return;
    }
    for path in ctx.reserved_paths {
        let Some(node) = ctx.nodes.iter().find(|n| &n.path == path) else { continue };
        issues.push(Issue {
            analyzer_id: "name-reserved".into(),
            severity: "warn",
            node_ids: vec![node.path.clone()],
            message: format_finding(
                None,
                &[],
                &format!("Reserved name; this {} name is shadowed by the {} runtime built-in", node.kind, node.provider),
            ),
            fix: Some(Fix { summary: "Rename the file or its frontmatter.name.".into() }),
            data: Some(json!({ "provider": node.provider, "kind": node.kind, "surface": "target" })),
        });
    }
    for (i, link) in ctx.links.iter().enumerate() {
        let Some(rp) = &link.resolved_target else { continue };
        if !ctx.reserved_paths.contains(rp) {
            continue;
        }
        let Some(node) = ctx.nodes.iter().find(|n| &n.path == rp) else { continue };
        deltas[i] -= RESERVED_PENALTY;
        issues.push(Issue {
            analyzer_id: "name-reserved".into(),
            severity: "warn",
            node_ids: vec![link.source.clone()],
            message: format_finding(
                Some(&link.target),
                &link_lines(link),
                &format!(
                    "Reserved name; resolves to the {} built-in ({} `{}`), the built-in shadows this edge",
                    node.provider, node.kind, node.path
                ),
            ),
            fix: Some(Fix { summary: "Rename the target file or its frontmatter.name.".into() }),
            data: Some(json!({
                "target": link.target, "kind": link.kind, "surface": "source",
                "reservedPath": node.path, "reservedProvider": node.provider, "reservedKind": node.kind,
            })),
        });
    }
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
    let has_ext_tail = raw.rsplit_once('.').is_some_and(|(_, ext)| !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric()));
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

fn reference_broken(ctx: &Ctx, deltas: &mut [f64], issues: &mut Vec<Issue>) {
    for (i, link) in ctx.links.iter().enumerate() {
        if !ctx.broken.contains(&i) {
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
        let body = if code_shaped {
            format!("Broken {kind_label}; target not found, and the token looks like a code identifier or npm package rather than a reference")
        } else {
            format!("Broken {kind_label}; target not found in the graph or on disk")
        };
        let fix = if code_shaped {
            "Dismiss this issue if the token is intentional prose, or fix the name / remove the mention."
        } else {
            "Fix the path or name, remove the broken link, or add its folder under Folders for link validation."
        };
        issues.push(Issue {
            analyzer_id: "reference-broken".into(),
            severity: if code_shaped { "warn" } else { "error" },
            node_ids: vec![link.source.clone()],
            message: format_finding(Some(&link.target), &link_lines(link), &body),
            fix: Some(Fix { summary: fix.into() }),
            data: Some(json!({
                "target": link.target,
                "kind": link.kind,
                "trigger": link.trigger.as_ref().map(|t| t.normalized_trigger.clone()),
            })),
        });
    }
}
