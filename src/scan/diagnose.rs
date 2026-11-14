//! Kernel-stamped node issues (spec_doc §12.1), ported from skill-map
//! `kernel/orchestrator/{frontmatter,node-build}.ts`: frontmatter parse
//! errors, malformed-fence heuristics, schema validation, backtick balance.
//! Each node takes exactly one frontmatter lane.

use crate::mask::{self, Imbalance};
use crate::model::Issue;
use crate::validate;
use fancy_regex::Regex;
use serde_json::{Value, json};
use std::sync::LazyLock;

fn issue(id: &str, path: &str, message: String, data: Option<Value>) -> Issue {
    let i = Issue::new(id, "warn", vec![path.into()], message);
    match data {
        Some(d) => i.data(d),
        None => i,
    }
}

/// `sanitiseParseErrorMessage`: control chars and whitespace runs → one space.
fn sanitise(s: &str) -> String {
    s.split(|c: char| c.is_control() || c.is_whitespace()).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
}

static UNQUOTED_COLON_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^[ \t]*[^:\n]+:\s+[^'"\s][^\n]*:(\s|$)"#).unwrap());

const UNQUOTED_COLON_HINT: &str = "Hint: a ':' followed by a space (or ending the line) inside an unquoted value starts a new YAML key and breaks the whole block; wrap the value in quotes, e.g. description: \"use when: something\".";

/// Message for a frontmatter YAML that failed to load. The unquoted-colon
/// class is rebuilt exactly as js-yaml reports it (reason, position, code
/// snippet, then skill-map's hint); any other YAML error keeps the Rust
/// parser's wording (a documented divergence).
pub fn parse_error_message(raw: &str, rust_error: &str) -> String {
    let lines: Vec<&str> = raw.split('\n').collect();
    for (i, line) in lines.iter().enumerate() {
        if !UNQUOTED_COLON_LINE.is_match(line).unwrap_or(false) {
            continue;
        }
        let first = line.find(':').unwrap_or(0);
        let Some(col) = line[first + 1..].find(':').map(|c| first + 1 + c) else { continue };
        let snippet = snippet(&lines, i, col);
        return sanitise(&format!("bad indentation of a mapping entry ({}:{})\n\n{snippet} {UNQUOTED_COLON_HINT}", i + 1, col + 1));
    }
    sanitise(rust_error)
}

/// js-yaml `makeSnippet` (indent 1, 3 lines before, 2 after, 79 columns).
fn snippet(lines: &[&str], line: usize, col: usize) -> String {
    // `lineEnds.length` is the number of line breaks; lines after the mark
    // are shown only when they end with one.
    let breaks = lines.len().saturating_sub(1);
    let width = (line + 2).min(breaks).to_string().len();
    let row = |n: usize, text: &str| format!(" {:>width$} | {}", n + 1, text.replace('\t', "→"));
    let mut out = Vec::new();
    for n in line.saturating_sub(3)..line {
        out.push(row(n, lines[n]));
    }
    out.push(row(line, lines[line]));
    out.push(format!("{}^", "-".repeat(1 + width + 3 + col)));
    for n in line + 1..(line + 3).min(breaks) {
        out.push(row(n, lines[n]));
    }
    out.join("\n")
}

static BOM_FENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\x{FEFF}(?:[ \t]*\r?\n)*[ \t]*---\r?\n[\s\S]*?[A-Za-z0-9_-]+\s*:").unwrap());
static INDENTED_FENCE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[ \t]+---\r?\n[ \t]*[A-Za-z0-9_-]+\s*:").unwrap());
static BLANK_FIRST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:[ \t]*\r?\n)+[ \t]*---\r?\n[ \t]*[A-Za-z0-9_-]+\s*:").unwrap());
static OPEN_FENCE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^---\r?\n[ \t]*[A-Za-z0-9_-]+\s*:").unwrap());
static CLOSE_FENCE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\r?\n---(?:\r?\n|$)").unwrap());
static YAML_KEY_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]+\s*:").unwrap());

fn malformed_hint(body: &str) -> Option<&'static str> {
    if body.starts_with('\u{FEFF}') {
        let head: String = body.chars().take(4096).collect();
        if BOM_FENCE.is_match(&head).unwrap_or(false) {
            return Some("byte-order-mark");
        }
    }
    if INDENTED_FENCE.is_match(body).unwrap_or(false) {
        return Some("paste-with-indent");
    }
    if BLANK_FIRST.is_match(body).unwrap_or(false) {
        return Some("leading-blank-line");
    }
    if OPEN_FENCE.is_match(body).unwrap_or(false) && !CLOSE_FENCE.is_match(body).unwrap_or(false) {
        return Some("missing-close");
    }
    None
}

fn malformed_message(hint: &str, path: &str) -> String {
    match hint {
        "paste-with-indent" => format!("Frontmatter fence in {path} appears indented; YAML frontmatter MUST start with `---` at column 0. The file was scanned as body-only; the metadata block was silently lost. Move the `---` lines to the start of the line."),
        "byte-order-mark" => format!("Frontmatter fence in {path} is preceded by a UTF-8 byte-order mark (BOM); the file was scanned as body-only. Re-save the file as UTF-8 without BOM. The metadata block was silently lost."),
        "leading-blank-line" => format!("Frontmatter fence in {path} is preceded by one or more blank lines; YAML frontmatter MUST open with `---` on the very first line of the file. The file was scanned as body-only; the metadata block was silently lost. Delete the blank lines above the opening `---`."),
        _ => format!("Frontmatter in {path} opens with `---` but never closes (no matching `---` line at column 0 was found). The file was scanned as body-only and every metadata field was silently lost. Add a closing `---` line below the metadata block."),
    }
}

/// Inline `properties` of the schema and its `allOf` branches, plus the
/// two base fields (refs are not followed, as in the original).
fn schema_properties(schema: Option<&Value>) -> Vec<String> {
    let mut out = vec!["name".to_string(), "description".to_string()];
    let mut collect = |s: &Value| {
        if let Some(p) = s.get("properties").and_then(Value::as_object) {
            for k in p.keys() {
                if !out.contains(k) {
                    out.push(k.clone());
                }
            }
        }
    };
    if let Some(s) = schema {
        collect(s);
        for branch in s.get("allOf").and_then(Value::as_array).into_iter().flatten() {
            collect(branch);
        }
    }
    out
}

fn early_close_keys(body: &str, schema: Option<&Value>) -> Option<Vec<String>> {
    let lines: Vec<&str> = body.split('\n').take(41).collect();
    if !lines.first().is_some_and(|l| YAML_KEY_LINE.is_match(l).unwrap_or(false)) {
        return None;
    }
    let close = (1..lines.len()).find(|&i| i <= 40 && (lines[i] == "---" || lines[i] == "---\r"))?;
    let Ok(Some(Value::Object(doc))) = crate::frontmatter::load(&lines[..close].join("\n")) else { return None };
    let props = schema_properties(schema);
    let leaked: Vec<String> = doc.keys().filter(|k| props.contains(k)).cloned().collect();
    (!leaked.is_empty()).then_some(leaked)
}

pub struct NodeText<'a> {
    pub path: &'a str,
    pub provider: &'a str,
    pub kind: &'a str,
    pub declared: bool,
    pub raw: &'a str,
    pub body: &'a str,
    pub body_line_offset: usize,
    pub frontmatter: &'a Value,
    pub parse_error: Option<&'a str>,
}

/// Issues stamped while building one node, in the original's order:
/// parse error, then one shape/schema verdict, then backtick balance.
pub fn node_issues(n: &NodeText) -> Vec<Issue> {
    let mut out = Vec::new();
    let schema_id = validate::kind_schema(n.provider, n.kind);
    if let Some(e) = n.parse_error {
        out.push(issue("frontmatter-parse-error", n.path, parse_error_message(n.raw, e), None));
    } else if let Some(shape) = shape_issue(n, schema_id.and_then(validate::schema)) {
        out.push(shape);
    } else if let Some(id) = schema_id {
        let errors = validate::errors(id, n.frontmatter);
        if !errors.is_empty() {
            let text = validate::format(&errors);
            out.push(issue(
                "frontmatter-invalid",
                n.path,
                format!("Frontmatter for {} ({}) failed schema validation: {text}", n.path, n.kind),
                Some(json!({ "kind": n.kind, "errors": text })),
            ));
        }
    }
    if let Some(imbalance) = mask::find_backtick_imbalance(n.body) {
        let (kind, line, source) = match imbalance {
            Imbalance::Fence { line, source_line } => ("fence", line, source_line),
            Imbalance::Inline { line, source_line } => ("inline", line, source_line),
        };
        let line = line + n.body_line_offset;
        let message = if kind == "fence" {
            format!("Body of {} has an unclosed fenced code block opened at body line {line} (no matching closing ``` or ~~~). The code-strip policy then reads the rest of the file as code, so prose extractors stop emitting edges past it. Close the fence.", n.path)
        } else {
            format!("Body of {} has an unclosed inline backtick at body line {line} (the backtick run has no equal-length closer). Close the inline span with a matching backtick run, or escape a literal backtick with a backslash.", n.path)
        };
        let i = Issue::new("backtick-unbalanced", "warn", vec![n.path.into()], message)
            .detail(source)
            .data(json!({ "kind": kind, "line": line }));
        out.push(i);
    }
    out
}

fn shape_issue(n: &NodeText, schema: Option<&Value>) -> Option<Issue> {
    if !n.declared && n.raw.is_empty() {
        let hint = malformed_hint(n.body)?;
        return Some(issue("frontmatter-malformed", n.path, malformed_message(hint, n.path), Some(json!({ "hint": hint }))));
    }
    let keys = early_close_keys(n.body, schema)?;
    Some(issue(
        "frontmatter-malformed",
        n.path,
        format!("Frontmatter in {} appears to close early: a `---` line inside the metadata block ended it prematurely, and the fields below it ({}) parsed as body text. Remove the stray `---` or move those fields above it.", n.path, keys.join(", ")),
        Some(json!({ "hint": "early-close", "leakedKeys": keys })),
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn unquoted_colon_message_matches_js_yaml() {
        let raw = "name: broken\ndescription: use when: something happens";
        assert_eq!(
            super::parse_error_message(raw, "ignored"),
            "bad indentation of a mapping entry (2:22) 1 | name: broken 2 | description: use when: something happens --------------------------^ Hint: a ':' followed by a space (or ending the line) inside an unquoted value starts a new YAML key and breaks the whole block; wrap the value in quotes, e.g. description: \"use when: something\"."
        );
    }
}
