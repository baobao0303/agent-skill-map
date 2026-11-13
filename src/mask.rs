//! Code-region masking, ported from skill-map `kernel/util/strip-code-blocks.ts`.
//! skill-map does not use a CommonMark parser here: fences are found line by
//! line and inline spans by a backreference regex, so this module follows that
//! algorithm instead of pulldown-cmark. Every transform is same-length in
//! UTF-8 bytes (a blanked char becomes as many spaces as it had bytes), so
//! byte offsets in a masked string point at the same text in the original.

use crate::norm::is_js_space;
use fancy_regex::Regex;
use std::sync::LazyLock;

static INLINE_SPAN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(`{1,32})([\s\S]*?)\1").unwrap());
static HTML_COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<!--[\s\S]*?-->").unwrap());
static HTML_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"</?[a-zA-Z][a-zA-Z0-9-]*(?:\s(?:"[^"]*"|'[^']*'|[^"'>])*)?/?>"#).unwrap());

/// Replace every non-whitespace char with spaces, keeping byte length.
pub fn blank(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if is_js_space(c) {
            out.push(c);
        } else {
            out.extend(std::iter::repeat_n(' ', c.len_utf8()));
        }
    }
    out
}

/// `^ {0,3}(`{3,}|~{3,})` — returns the fence run.
fn fence_of(line: &str) -> Option<&str> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let ch = trimmed.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let run = trimmed.len() - trimmed.trim_start_matches(ch).len();
    (run >= 3).then(|| &trimmed[..run])
}

/// Blank fenced blocks. Returns the masked text and the 1-based line of a
/// fence left open at end of input (0 when balanced).
fn scan_fences(input: &str) -> (String, usize) {
    let mut out = Vec::new();
    let mut open: Option<&str> = None;
    let mut open_line = 0;
    for (i, line) in input.split('\n').enumerate() {
        if let Some(fence) = open {
            if let Some(close) = fence_of(line)
                && close.as_bytes()[0] == fence.as_bytes()[0]
                && close.len() >= fence.len()
            {
                open = None;
            }
            out.push(blank(line));
            continue;
        }
        if let Some(fence) = fence_of(line) {
            open = Some(fence);
            open_line = i + 1;
            out.push(blank(line));
            continue;
        }
        out.push(line.to_string());
    }
    (out.join("\n"), if open.is_some() { open_line } else { 0 })
}

fn strip_inline(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last = 0;
    let mut pos = 0;
    while pos <= input.len() {
        let Ok(Some(caps)) = INLINE_SPAN.captures_from_pos(input, pos) else { break };
        let whole = caps.get(0).unwrap();
        out.push_str(&input[last..whole.start()]);
        out.push_str(&blank(whole.as_str()));
        last = whole.end();
        pos = if whole.end() > whole.start() { whole.end() } else { whole.end() + 1 };
    }
    out.push_str(&input[last..]);
    out
}

fn replace_all_blank(re: &Regex, input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last = 0;
    for m in re.find_iter(input).flatten() {
        out.push_str(&input[last..m.start()]);
        out.push_str(&blank(m.as_str()));
        last = m.end();
    }
    out.push_str(&input[last..]);
    out
}

pub fn strip_code_blocks(input: &str) -> String {
    strip_inline(&scan_fences(input).0)
}

pub fn strip_html(input: &str) -> String {
    replace_all_blank(&HTML_TAG, &replace_all_blank(&HTML_COMMENT, input))
}

/// Prose domain: code regions and raw HTML blanked.
pub fn strip_code_and_html(input: &str) -> String {
    strip_html(&strip_code_blocks(input))
}

/// Code domain: everything outside fences and inline spans blanked.
pub fn extract_code_regions(input: &str) -> String {
    inverse_mask(input, &strip_code_blocks(input))
}

/// Fenced-block domain only.
pub fn extract_fenced_regions(input: &str) -> String {
    inverse_mask(input, &scan_fences(input).0)
}

/// Keep only chars the strip changed; blank the rest.
fn inverse_mask(input: &str, stripped: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for (i, c) in input.char_indices() {
        let n = c.len_utf8();
        if stripped.get(i..i + n) == Some(&input[i..i + n]) {
            if c == '\n' {
                out.push('\n');
            } else {
                out.extend(std::iter::repeat_n(' ', n));
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub enum Imbalance {
    Fence { line: usize, source_line: String },
    Inline { line: usize, source_line: String },
}

pub fn find_backtick_imbalance(body: &str) -> Option<Imbalance> {
    if body.is_empty() {
        return None;
    }
    let (stripped, open_line) = scan_fences(body);
    let source_line = |line: usize| body.split('\n').nth(line - 1).unwrap_or("").trim().to_string();
    if open_line > 0 {
        return Some(Imbalance::Fence { line: open_line, source_line: source_line(open_line) });
    }
    let survivor = strip_inline(&mask_escapes(&stripped)).find('`')?;
    let line = 1 + stripped.as_bytes()[..survivor].iter().filter(|&&b| b == b'\n').count();
    Some(Imbalance::Inline { line, source_line: source_line(line) })
}

/// `\\[^\n]` → two spaces (same byte length as the escape sequence).
fn mask_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(&next) = chars.peek()
            && next != '\n'
        {
            chars.next();
            out.extend(std::iter::repeat_n(' ', 1 + next.len_utf8()));
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_fences_and_inline_keeping_length() {
        let body = "see `/cmd` and\n```js\n/inside\n```\nafter [x](y.md)\n";
        let stripped = strip_code_blocks(body);
        assert_eq!(stripped.len(), body.len());
        assert!(!stripped.contains("/cmd") && !stripped.contains("/inside"));
        assert!(stripped.contains("[x](y.md)"));
        let code = extract_code_regions(body);
        assert_eq!(code.len(), body.len());
        assert!(code.contains("/cmd") && code.contains("/inside") && !code.contains("[x]"));
    }

    #[test]
    fn html_comment_and_tags() {
        let s = strip_html("a <!-- [x](old.md) --> b <img alt=\"[see](ref.md)\"> c");
        assert!(!s.contains("old.md") && !s.contains("ref.md"));
    }

    #[test]
    fn imbalance() {
        assert!(matches!(find_backtick_imbalance("x\n```\nopen"), Some(Imbalance::Fence { line: 2, .. })));
        assert!(matches!(find_backtick_imbalance("a `b\nc"), Some(Imbalance::Inline { line: 1, .. })));
        assert!(find_backtick_imbalance("a \\` b `c`").is_none());
        assert!(find_backtick_imbalance("ok `x` ok").is_none());
    }
}
