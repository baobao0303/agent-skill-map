//! Graph formatters (`sm graph --format ascii|json|mermaid|dot`), ported from
//! skill-map `plugins/core/formatters`. `ascii` sorts with JavaScript's
//! `localeCompare` (ICU root collation); mermaid/dot sort by UTF-16 code units.

use crate::jsjson;
use crate::model::{Issue, Link, Node, ScanResult};
use icu_collator::options::CollatorOptions;
use icu_collator::{Collator, CollatorBorrowed};
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::sync::LazyLock;

static COLLATOR: LazyLock<CollatorBorrowed<'static>> =
    LazyLock::new(|| Collator::try_new(Default::default(), CollatorOptions::default()).expect("root collator"));

pub fn locale_compare(a: &str, b: &str) -> Ordering {
    COLLATOR.compare(a, b)
}

fn code_unit_compare(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// `sanitizeForTerminal` (control, bidi and separator chars removed; ANSI
/// escape sequences are not expected in scan data and are dropped as controls).
pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let cp = c as u32;
        let control = matches!(cp, 0x00..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F | 0x7F..=0x9F);
        let bidi = matches!(cp, 0x2028 | 0x2029 | 0x200E | 0x200F | 0x202A..=0x202E | 0x2066..=0x2069);
        let bare_cr = c == '\r' && chars.peek() != Some(&'\n');
        if !(control || bidi || bare_cr) {
            out.push(c);
        }
    }
    out
}

fn single_line(text: &str) -> String {
    let s = sanitize(text);
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if matches!(c, '\t' | '\n' | '\r') {
            if !in_ws {
                out.push(' ');
            }
            in_ws = true;
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}

pub fn render(format: &str, scan: &ScanResult) -> Option<String> {
    Some(match format {
        "ascii" => ascii(&scan.nodes, &scan.links, &scan.issues),
        "json" => jsjson::to_string(scan),
        "mermaid" => mermaid(&scan.nodes, &scan.links, &scan.issues),
        "dot" => dot(&scan.nodes, &scan.links, &scan.issues),
        _ => return None,
    })
}

fn header(nodes: &[Node], links: &[Link], issues: &[Issue]) -> String {
    format!("skill-map graph: {} nodes, {} links, {} issues", nodes.len(), links.len(), issues.len())
}

fn ascii(nodes: &[Node], links: &[Link], issues: &[Issue]) -> String {
    let mut out = vec![header(nodes, links, issues), String::new()];
    let mut kinds: Vec<&str> = Vec::new();
    for n in nodes {
        if !kinds.contains(&n.kind.as_str()) {
            kinds.push(&n.kind);
        }
    }
    let known = ["agent", "command", "skill", "markdown"];
    let mut extra: Vec<&str> = kinds.iter().copied().filter(|k| !known.contains(k)).collect();
    extra.sort_by(|a, b| code_unit_compare(a, b));
    for kind in known.iter().copied().filter(|k| kinds.contains(k)).chain(extra) {
        let mut group: Vec<&Node> = nodes.iter().filter(|n| n.kind == kind).collect();
        group.sort_by(|a, b| locale_compare(&a.path, &b.path));
        out.push(format!("## {} ({})", sanitize(kind), group.len()));
        for n in group {
            let title = n.frontmatter.as_ref().and_then(|f| f.get("name")).and_then(|v| v.as_str()).filter(|s| !s.is_empty());
            out.push(match title {
                Some(t) => format!("- {}: \"{}\"", sanitize(&n.path), sanitize(t)),
                None => format!("- {}", sanitize(&n.path)),
            });
        }
        out.push(String::new());
    }
    if !links.is_empty() {
        out.push(format!("## links ({})", links.len()));
        let mut sorted: Vec<&Link> = links.iter().collect();
        sorted.sort_by(|a, b| {
            locale_compare(&format!("{}\0{}\0{}", a.source, a.kind, a.target), &format!("{}\0{}\0{}", b.source, b.kind, b.target))
        });
        for l in sorted {
            out.push(format!(
                "- {} --{}--> {}  [{}]",
                sanitize(&l.source),
                sanitize(l.kind),
                sanitize(&l.target),
                jsjson::js_number(l.confidence)
            ));
        }
        out.push(String::new());
    }
    if !issues.is_empty() {
        out.push(format!("## issues ({})", issues.len()));
        for i in issues {
            out.push(format!("- [{}] {}: {}", i.severity, sanitize(&i.analyzer_id), sanitize(&i.message)));
        }
        out.push(String::new());
    }
    out.join("\n")
}

/// Every node path plus every link endpoint, in code-unit order, with its kind.
fn node_rows<'a>(nodes: &'a [Node], links: &'a [Link]) -> Vec<(&'a str, Option<&'a str>)> {
    let mut paths: BTreeSet<Vec<u16>> = BTreeSet::new();
    let mut by_utf16 = std::collections::HashMap::new();
    for p in nodes.iter().map(|n| n.path.as_str()).chain(links.iter().flat_map(|l| [l.source.as_str(), l.target.as_str()])) {
        let key: Vec<u16> = p.encode_utf16().collect();
        by_utf16.entry(key.clone()).or_insert(p);
        paths.insert(key);
    }
    paths
        .into_iter()
        .map(|k| {
            let p = by_utf16[&k];
            (p, nodes.iter().find(|n| n.path == p).map(|n| n.kind.as_str()))
        })
        .collect()
}

fn sorted_links(links: &[Link]) -> Vec<&Link> {
    let mut sorted: Vec<&Link> = links.iter().collect();
    sorted.sort_by(|a, b| {
        code_unit_compare(&format!("{}\0{}\0{}", a.source, a.kind, a.target), &format!("{}\0{}\0{}", b.source, b.kind, b.target))
    });
    sorted
}

const KIND_STYLES: [&str; 6] = [
    "fill:#e3f2fd,stroke:#1565c0,color:#0d47a1",
    "fill:#ede7f6,stroke:#5e35b1,color:#311b92",
    "fill:#e8f5e9,stroke:#2e7d32,color:#1b5e20",
    "fill:#fff3e0,stroke:#ef6c00,color:#e65100",
    "fill:#fce4ec,stroke:#ad1457,color:#880e4f",
    "fill:#e0f7fa,stroke:#00838f,color:#006064",
];

fn mermaid_label(text: &str) -> String {
    single_line(text).replace('#', "#35;").replace('"', "#quot;")
}

fn mermaid(nodes: &[Node], links: &[Link], issues: &[Issue]) -> String {
    let rows = node_rows(nodes, links);
    let id_of = |p: &str| rows.iter().position(|(r, _)| *r == p).map(|i| format!("n{i}"));
    let mut out = vec![
        "flowchart LR".to_string(),
        format!("%% skill-map graph: {} nodes, {} links, {} issues", nodes.len(), links.len(), issues.len()),
    ];
    if rows.is_empty() {
        out.push("%% no nodes to render; run `sm scan` to populate the graph".into());
    }
    for (i, (path, _)) in rows.iter().enumerate() {
        out.push(format!("  n{i}[\"{}\"]", mermaid_label(path)));
    }
    for l in sorted_links(links) {
        if let (Some(s), Some(t)) = (id_of(&l.source), id_of(&l.target)) {
            out.push(format!("  {s} -->|\"{}\"| {t}", mermaid_label(l.kind)));
        }
    }
    let mut kinds: Vec<&str> = rows.iter().filter_map(|(_, k)| *k).collect();
    kinds.sort_by(|a, b| code_unit_compare(a, b));
    kinds.dedup();
    let mut taken: Vec<String> = Vec::new();
    let mut names: Vec<(&str, String)> = Vec::new();
    for k in &kinds {
        let base = format!("kind_{}", single_line(k).chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect::<String>());
        let mut name = base.clone();
        let mut suffix = 2;
        while taken.contains(&name) {
            name = format!("{base}_{suffix}");
            suffix += 1;
        }
        taken.push(name.clone());
        names.push((k, name));
    }
    for (i, (_, name)) in names.iter().enumerate() {
        out.push(format!("  classDef {name} {};", KIND_STYLES[i % KIND_STYLES.len()]));
    }
    for (i, (_, kind)) in rows.iter().enumerate() {
        if let Some(k) = kind {
            let name = &names.iter().find(|(n, _)| n == k).unwrap().1;
            out.push(format!("  class n{i} {name};"));
        }
    }
    out.join("\n")
}

fn dot_escape(text: &str) -> String {
    single_line(text).replace('\\', "\\\\").replace('"', "\\\"")
}

fn dot(nodes: &[Node], links: &[Link], issues: &[Issue]) -> String {
    let mut out = vec![
        "digraph \"skill-map\" {".to_string(),
        format!("  // skill-map graph: {} nodes, {} links, {} issues", nodes.len(), links.len(), issues.len()),
        "  rankdir=\"LR\";".to_string(),
    ];
    for (path, kind) in node_rows(nodes, links) {
        let label = match kind {
            Some(k) => format!("{}\\n{}", dot_escape(path), dot_escape(k)),
            None => dot_escape(path),
        };
        out.push(format!("  \"{}\" [label=\"{label}\"];", dot_escape(path)));
    }
    for l in sorted_links(links) {
        out.push(format!("  \"{}\" -> \"{}\" [label=\"{}\"];", dot_escape(&l.source), dot_escape(&l.target), dot_escape(l.kind)));
    }
    out.push("}".into());
    out.join("\n")
}

#[cfg(test)]
mod tests {
    #[test]
    fn locale_compare_is_not_byte_order() {
        assert_eq!(super::locale_compare("alpha", "Zeta"), std::cmp::Ordering::Less);
        assert_eq!(super::locale_compare(".claude/x", "AGENTS.md"), std::cmp::Ordering::Less);
    }
}
