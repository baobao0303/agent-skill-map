//! Extractors (spec_doc §9). Each one reads a node and emits Signals; the
//! resolver turns signals into links. Offsets in signals are UTF-16 code
//! units into the body (what JavaScript string indices are), lines are
//! file lines (body line + `body_line_offset`).

use crate::mask;
use crate::norm::normalize_trigger;
use fancy_regex::Regex;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::LazyLock;

pub struct NodeInput<'a> {
    pub path: &'a str,
    pub provider: &'static str,
    pub body: &'a str,
    pub body_line_offset: usize,
    pub frontmatter: &'a Value,
}

#[derive(Clone)]
pub struct Range {
    pub start: u64,
    pub end: u64,
    pub line: u64,
}

#[derive(Clone)]
pub struct Candidate {
    pub extractor: &'static str,
    pub kind: &'static str,
    pub target: String,
    pub confidence: f64,
    pub original_trigger: String,
    pub normalized_trigger: String,
}

#[derive(Clone)]
pub struct Signal {
    pub source: String,
    pub body_scope: bool,
    pub range: Option<Range>,
    pub raw: String,
    pub context: Option<&'static str>,
    pub candidates: Vec<Candidate>,
}

pub struct VirtualNode {
    pub path: String,
    pub kind: &'static str,
    pub provider: &'static str,
    pub derived_from: Vec<String>,
    pub frontmatter: Value,
}

#[derive(Default)]
pub struct Output {
    pub signals: Vec<Signal>,
    pub virtual_nodes: Vec<VirtualNode>,
}

/// Registration order (built-ins.ts); also the extractor rank used by the resolver.
pub const ORDER: &[&str] = &[
    "at-directive",
    "backtick-mention",
    "tools-counter",
    "backtick-dollar",
    "dollar-skill",
    "at-file",
    "backtick-path",
    "backtick-slash",
    "external-url-counter",
    "markdown-link",
    "mcp-tools",
    "slash-command",
];

/// Run every extractor whose lens gate admits `lens`, in registration order.
pub fn run(node: &NodeInput, lens: &str) -> Output {
    let mut out = Output::default();
    let prose = mask::strip_code_and_html(node.body);
    let ctx = Ctx { node, prose: &prose, line_starts: line_starts(&prose) };
    if lens == "claude" {
        at_directive(&ctx, &mut out);
    }
    external_url_counter(&ctx, &mut out);
    markdown_link(&ctx, &mut out);
    mcp_tools(node, &mut out);
    if matches!(lens, "claude" | "antigravity" | "opencode") {
        slash_command(&ctx, &mut out);
    }
    out
}

struct Ctx<'a> {
    node: &'a NodeInput<'a>,
    prose: &'a str,
    line_starts: Vec<usize>,
}

impl Ctx<'_> {
    fn range(&self, byte_start: usize, byte_len: usize) -> Range {
        let start = utf16_len(&self.prose[..byte_start]);
        let end = start + utf16_len(&self.prose[byte_start..byte_start + byte_len]);
        let body_line = self.line_starts.partition_point(|&s| s <= byte_start) as u64;
        Range { start, end, line: body_line + self.node.body_line_offset as u64 }
    }

    fn signal(&self, byte_start: usize, raw: &str, candidate: Candidate) -> Signal {
        Signal {
            source: self.node.path.to_string(),
            body_scope: true,
            range: Some(self.range(byte_start, raw.len())),
            raw: raw.to_string(),
            context: None,
            candidates: vec![candidate],
        }
    }
}

fn utf16_len(s: &str) -> u64 {
    s.encode_utf16().count() as u64
}

fn line_starts(s: &str) -> Vec<usize> {
    std::iter::once(0).chain(s.match_indices('\n').map(|(i, _)| i + 1)).collect()
}

// ---------- path helpers (Node's path.posix) ----------

pub fn posix_normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let absolute = path.starts_with('/');
    let trailing = path.ends_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|p| *p != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            s => parts.push(s),
        }
    }
    let mut out = parts.join("/");
    if out.is_empty() && !absolute {
        out.push('.');
    }
    if !out.is_empty() && trailing {
        out.push('/');
    }
    if absolute { format!("/{out}") } else { out }
}

pub fn posix_dirname(path: &str) -> &str {
    match path.trim_end_matches('/').rfind('/') {
        Some(0) => "/",
        Some(i) => &path[..i],
        None => ".",
    }
}

// ---------- extractors ----------

static AT_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[^A-Za-z0-9_@])(@(?=[0-9_\-./:]*[a-z])(?:(?:\.{1,2}/)+|/)?\.?[a-z0-9](?:[a-z0-9_\-./]*[a-z0-9_])?(?::[a-z0-9][a-z0-9_-]*)?)").unwrap()
});
static FILE_EXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\.(md|mdx|js|jsx|ts|tsx|json|yml|yaml|toml|txt|html|css|scss|less|py|rb|go|rs|java|c|cpp|h|hpp|sh|sql|svg|png|jpg|jpeg|gif|webp|pdf)$").unwrap()
});
static SLASH_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?<![A-Za-z0-9_/.:?#=&])(/(?=[0-9_-]*[a-z])[a-z0-9][a-z0-9_-]*(?::(?=[0-9_-]*[a-z])[a-z0-9][a-z0-9_-]*)?)").unwrap()
});
static MD_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?<!!)\[([^\]]*)\]\(([^)\s]+)(?:\s+"[^"]*")?\)"#).unwrap());
static URL_SCHEME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^[a-z][a-z0-9+.-]*:").unwrap());
static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s<>"'`)\]]+"#).unwrap());
static MCP_TOOL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^mcp__([a-z0-9][a-z0-9_-]*)__([a-z0-9_-]+)$").unwrap());

/// `classifyAtFileToken`: file-shaped `@` tokens belong to `at-file`.
pub fn is_at_file_token(bare: &str) -> bool {
    !bare.starts_with('/')
        && (bare.starts_with("./") || bare.starts_with("../") || FILE_EXT.is_match(bare).unwrap_or(false))
}

fn at_directive(ctx: &Ctx, out: &mut Output) {
    let mut seen = HashSet::new();
    for caps in AT_TOKEN.captures_iter(ctx.prose).flatten() {
        let m = caps.get(1).unwrap();
        let original = m.as_str();
        let bare = &original[1..];
        if bare.starts_with('/') || is_at_file_token(bare) {
            continue;
        }
        let normalized = normalize_trigger(original);
        if !seen.insert(normalized.clone()) {
            continue;
        }
        let candidate = Candidate {
            extractor: "at-directive",
            kind: "mentions",
            target: original.to_string(),
            confidence: 0.5,
            original_trigger: original.to_string(),
            normalized_trigger: normalized,
        };
        out.signals.push(ctx.signal(m.start(), original, candidate));
    }
}

fn slash_command(ctx: &Ctx, out: &mut Output) {
    let mut seen = HashSet::new();
    for caps in SLASH_TOKEN.captures_iter(ctx.prose).flatten() {
        let m = caps.get(1).unwrap();
        let next = ctx.prose[m.end()..].chars().next();
        if next.is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '-')) {
            continue;
        }
        let original = m.as_str();
        let normalized = normalize_trigger(original);
        if !seen.insert(normalized.clone()) {
            continue;
        }
        let candidate = Candidate {
            extractor: "slash-command",
            kind: "invokes",
            target: original.to_string(),
            confidence: 0.8,
            original_trigger: original.to_string(),
            normalized_trigger: normalized,
        };
        out.signals.push(ctx.signal(m.start(), original, candidate));
    }
}

fn markdown_link(ctx: &Ctx, out: &mut Output) {
    let mut seen = HashSet::new();
    let source_dir = posix_dirname(ctx.node.path);
    for caps in MD_LINK.captures_iter(ctx.prose).flatten() {
        let whole = caps.get(0).unwrap();
        let original = caps.get(2).unwrap().as_str();
        let Some(resolved) = resolve_link_target(source_dir, original) else { continue };
        if !seen.insert(resolved.clone()) {
            continue;
        }
        let candidate = Candidate {
            extractor: "markdown-link",
            kind: "references",
            target: resolved.clone(),
            confidence: 0.95,
            original_trigger: original.to_string(),
            normalized_trigger: resolved,
        };
        out.signals.push(ctx.signal(whole.start(), whole.as_str(), candidate));
    }
}

fn resolve_link_target(source_dir: &str, raw: &str) -> Option<String> {
    let no_fragment = raw.split('#').next().unwrap_or("");
    let no_query = no_fragment.split('?').next().unwrap_or("");
    let trimmed = no_query.trim();
    if trimmed.is_empty() || URL_SCHEME.is_match(trimmed).unwrap_or(false) {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix('/') {
        let rooted = posix_normalize(rest);
        if rooted.is_empty() || rooted == "." || rooted == "/" || rooted.starts_with('/') || rooted.starts_with("..") {
            return None;
        }
        return Some(rooted);
    }
    let joined = if source_dir == "." { trimmed.to_string() } else { format!("{source_dir}/{trimmed}") };
    Some(posix_normalize(&joined))
}

fn external_url_counter(ctx: &Ctx, out: &mut Output) {
    let mut seen = HashSet::new();
    for m in URL.find_iter(ctx.prose).flatten() {
        let original = m.as_str().trim_end_matches(['.', ',', ';', ':', '!', '?']);
        if original.is_empty() {
            continue;
        }
        let Ok(mut url) = url::Url::parse(original) else { continue };
        url.set_fragment(None);
        let href = url.to_string();
        if !seen.insert(href.clone()) {
            continue;
        }
        let candidate = Candidate {
            extractor: "external-url-counter",
            kind: "references",
            target: href.clone(),
            confidence: 0.3,
            original_trigger: original.to_string(),
            normalized_trigger: href,
        };
        out.signals.push(ctx.signal(m.start(), original, candidate));
    }
}

fn mcp_tools(node: &NodeInput, out: &mut Output) {
    let Some(tools) = node.frontmatter.get("tools").and_then(Value::as_array) else { return };
    let mut servers: Vec<String> = Vec::new();
    for t in tools.iter().filter_map(Value::as_str) {
        if let Ok(Some(caps)) = MCP_TOOL.captures(t) {
            let server = caps.get(1).unwrap().as_str().trim().to_lowercase();
            if !servers.contains(&server) {
                servers.push(server);
            }
        }
    }
    for server in servers {
        let path = format!("mcp://{server}");
        out.virtual_nodes.push(VirtualNode {
            path: path.clone(),
            kind: "mcp",
            provider: node.provider,
            derived_from: vec![node.path.to_string()],
            frontmatter: json!({ "name": server }),
        });
        let raw = format!("mcp__{server}__*");
        out.signals.push(Signal {
            source: node.path.to_string(),
            body_scope: false,
            range: None,
            raw: raw.clone(),
            context: None,
            candidates: vec![Candidate {
                extractor: "mcp-tools",
                kind: "references",
                target: path.clone(),
                confidence: 0.85,
                original_trigger: raw,
                normalized_trigger: path,
            }],
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_paths() {
        assert_eq!(posix_normalize(".claude/agents/../../docs/STYLE.md"), "docs/STYLE.md");
        assert_eq!(posix_normalize("../x.md"), "../x.md");
        assert_eq!(posix_normalize("a/./b//c/"), "a/b/c/");
        assert_eq!(posix_normalize("a/.."), ".");
        assert_eq!(posix_dirname("AGENTS.md"), ".");
        assert_eq!(posix_dirname(".claude/agents/x.md"), ".claude/agents");
    }

    #[test]
    fn slash_and_at_tokens() {
        let fm = Value::Object(Default::default());
        let node = NodeInput { path: "a.md", provider: "claude", body: "run /deploy then @reviewer, not /api/v1 or `/code`\n", body_line_offset: 0, frontmatter: &fm };
        let out = run(&node, "claude");
        let targets: Vec<&str> = out.signals.iter().map(|s| s.candidates[0].target.as_str()).collect();
        assert_eq!(targets, ["@reviewer", "/deploy"]);
    }
}
