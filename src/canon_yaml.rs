//! Port of js-yaml 5 `dump(value, { sortKeys: true, lineWidth: -1, noRefs: true,
//! schema: CORE_SCHEMA })`, the canonical form skill-map hashes for
//! `frontmatterHash` and the sidecar/settings cache keys. Must stay
//! byte-identical with js-yaml; see conformance vectors in `tests/`.

use crate::jsjson::js_number;
use serde_json::Value;
use std::cmp::Ordering;

const INDENT: usize = 2;

pub fn dump(value: &Value) -> String {
    // js-yaml's `...` document-end marker never fires here: nodes built from
    // plain JS values never carry the literal/folded style flag it checks.
    let mut out = write_node(0, value, true, true, false);
    out.push('\n');
    out
}

fn write_node(level: usize, value: &Value, block: bool, compact: bool, iskey: bool) -> String {
    match value {
        Value::Object(map) if block && !map.is_empty() => write_block_mapping(level, map, compact),
        Value::Object(map) => write_flow_mapping(level, map),
        Value::Array(items) if block && !items.is_empty() => write_block_sequence(level, items, compact),
        Value::Array(items) => write_flow_sequence(level, items),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => represent_number(n),
        Value::String(s) => write_string(level, s, iskey, block),
    }
}

fn next_line(level: usize) -> String {
    format!("\n{}", " ".repeat(INDENT * level))
}

fn sorted_entries(map: &serde_json::Map<String, Value>) -> Vec<(&String, &Value)> {
    let mut items: Vec<_> = map.iter().collect();
    items.sort_by(|a, b| js_cmp(a.0, b.0));
    items
}

/// JavaScript `<` on strings: UTF-16 code unit order.
fn js_cmp(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

fn write_block_sequence(level: usize, items: &[Value], compact: bool) -> String {
    let mut result = String::new();
    for item in items {
        let text = write_node(level + 1, item, true, true, false);
        if !compact || !result.is_empty() {
            result.push_str(&next_line(level));
        }
        result.push_str(if text.is_empty() || text.starts_with('\n') { "-" } else { "- " });
        result.push_str(&text);
    }
    result
}

fn write_block_mapping(level: usize, map: &serde_json::Map<String, Value>, compact: bool) -> String {
    let mut result = String::new();
    for (key, value) in sorted_entries(map) {
        let mut pair = String::new();
        if !compact || !result.is_empty() {
            pair.push_str(&next_line(level));
        }
        let key_text = write_string(level + 1, key, true, true);
        let explicit = key.contains('\n') || key_text.encode_utf16().count() > 1024;
        if explicit {
            pair.push_str(if key_text.starts_with('\n') { "?" } else { "? " });
        }
        pair.push_str(&key_text);
        if explicit {
            pair.push_str(&next_line(level));
        }
        let value_text = write_node(level + 1, value, true, explicit, false);
        pair.push_str(if value_text.is_empty() || value_text.starts_with('\n') { ":" } else { ": " });
        pair.push_str(&value_text);
        result.push_str(&pair);
    }
    result
}

fn write_flow_sequence(level: usize, items: &[Value]) -> String {
    let parts: Vec<String> = items.iter().map(|v| write_node(level, v, false, false, false)).collect();
    format!("[{}]", parts.join(", "))
}

fn write_flow_mapping(level: usize, map: &serde_json::Map<String, Value>) -> String {
    let parts: Vec<String> = sorted_entries(map)
        .into_iter()
        .map(|(k, v)| {
            let key_text = write_string(level, k, true, false);
            let explicit = key_text.encode_utf16().count() > 1024;
            let value_text = write_node(level, v, false, false, false);
            let sep = if value_text.is_empty() { "" } else { " " };
            format!("{}{key_text}:{sep}{value_text}", if explicit { "? " } else { "" })
        })
        .collect();
    format!("{{{}}}", parts.join(", "))
}

fn represent_number(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return js_number(u as f64);
    }
    let f = n.as_f64().unwrap_or(0.0);
    if f == 0.0 && f.is_sign_negative() {
        return "-0.0".to_string();
    }
    let s = js_number(f);
    // js-yaml float: `1e+21` → `1.e+21`
    let digits_then_e = s.trim_start_matches(['-', '+']);
    if let Some(pos) = digits_then_e.find('e')
        && pos > 0
        && digits_then_e[..pos].bytes().all(|b| b.is_ascii_digit())
    {
        return s.replacen('e', ".e", 1);
    }
    s
}

// ---------- scalar strings ----------

#[derive(PartialEq)]
enum Style {
    Plain,
    Single,
    Literal,
    Double,
}

fn write_string(level: usize, s: &str, iskey: bool, inblock: bool) -> String {
    let indent = INDENT * level.max(1);
    let block_indent = if level == 0 { INDENT + 1 } else { INDENT };
    match resolve_style(s, iskey, inblock) {
        Style::Plain => s.to_string(),
        Style::Single => format!("'{}'", s.replace('\'', "''")),
        Style::Literal => format!(
            "|{}{}",
            block_header(s, block_indent),
            drop_ending_newline(&indent_string(s, indent))
        ),
        Style::Double => format!("\"{}\"", escape_string(s)),
    }
}

fn resolve_style(s: &str, iskey: bool, inblock: bool) -> Style {
    if s.is_empty() {
        return Style::Single; // "" resolves to null under CORE
    }
    let style = choose_style(s, iskey || !inblock, inblock);
    if style == Style::Plain && resolves_to_non_string(s) {
        return Style::Single;
    }
    style
}

fn choose_style(s: &str, single_line_only: bool, inblock: bool) -> Style {
    let cps: Vec<i64> = s.chars().map(|c| c as i64).collect();
    let mut plain = !starts_with_document_separator(s)
        && is_plain_safe_at_start(&cps, inblock)
        && is_plain_safe_last(*cps.last().unwrap());
    let mut has_line_break = false;
    let mut prev: i64 = -1;
    for &c in &cps {
        if c == 10 && !single_line_only {
            has_line_break = true;
        } else if !is_printable(c) {
            return Style::Double;
        }
        plain = plain && is_plain_safe(c, prev, inblock);
        prev = c;
    }
    if !has_line_break {
        return if plain { Style::Plain } else { Style::Single };
    }
    Style::Literal
}

fn starts_with_document_separator(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 3 || !(b[0] == b'-' || b[0] == b'.') || b[1] != b[0] || b[2] != b[0] {
        return false;
    }
    b.len() == 3 || matches!(b[3], b' ' | b'\t' | b'\r' | b'\n')
}

fn is_whitespace(c: i64) -> bool {
    c == 32 || c == 9
}

fn is_printable(c: i64) -> bool {
    (32..=126).contains(&c)
        || ((161..=55295).contains(&c) && c != 8232 && c != 8233)
        || ((57344..=65533).contains(&c) && c != 65279)
        || (65536..=1114111).contains(&c)
}

fn is_ns_char_or_whitespace(c: i64) -> bool {
    is_printable(c) && c != 65279 && c != 13 && c != 10
}

fn is_flow_indicator(c: i64) -> bool {
    matches!(c, 44 | 91 | 93 | 123 | 125)
}

fn is_plain_safe(c: i64, prev: i64, inblock: bool) -> bool {
    let c_ns_or_ws = is_ns_char_or_whitespace(c);
    let c_ns = c_ns_or_ws && !is_whitespace(c);
    ((if inblock { c_ns_or_ws } else { c_ns_or_ws && !is_flow_indicator(c) })
        && c != 35
        && !(prev == 58 && !c_ns))
        || (is_ns_char_or_whitespace(prev) && !is_whitespace(prev) && c == 35)
        || (prev == 58 && c_ns && (inblock || !is_flow_indicator(c)))
}

fn is_plain_safe_first(c: i64) -> bool {
    is_printable(c)
        && c != 65279
        && !is_whitespace(c)
        && !matches!(
            c,
            45 | 63 | 58 | 44 | 91 | 93 | 123 | 125 | 35 | 38 | 42 | 33 | 124 | 61 | 62 | 39 | 34 | 37 | 64 | 96
        )
}

fn is_plain_safe_at_start(cps: &[i64], inblock: bool) -> bool {
    let first = cps[0];
    if is_plain_safe_first(first) {
        return true;
    }
    if cps.len() > 1 && matches!(first, 45 | 63 | 58) {
        let second = cps[1];
        return !is_whitespace(second) && is_plain_safe(second, first, inblock);
    }
    false
}

fn is_plain_safe_last(c: i64) -> bool {
    !is_whitespace(c) && c != 58
}

/// CORE schema implicit resolvers, in schema order: null, bool, int, float.
fn resolves_to_non_string(s: &str) -> bool {
    matches!(s, "" | "~" | "null" | "Null" | "NULL")
        || matches!(s, "true" | "True" | "TRUE" | "false" | "False" | "FALSE")
        || is_core_int(s)
        || is_core_float(s)
}

fn is_core_int(s: &str) -> bool {
    let b = s.as_bytes();
    if let Some(rest) = s.strip_prefix("0o") {
        return !rest.is_empty() && rest.bytes().all(|c| (b'0'..=b'7').contains(&c));
    }
    if let Some(rest) = s.strip_prefix("0x") {
        return !rest.is_empty() && rest.bytes().all(|c| c.is_ascii_hexdigit());
    }
    let digits = if matches!(b.first(), Some(b'-' | b'+')) { &s[1..] } else { s };
    !digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit()) && parse_finite(digits)
}

fn parse_finite(digits: &str) -> bool {
    digits.parse::<f64>().map(f64::is_finite).unwrap_or(false)
}

/// `^(?:[-+]?[0-9]+(?:\.[0-9]*)?(?:[eE][-+]?[0-9]+)?|[-+]?\.[0-9]+(?:[eE][-+]?[0-9]+)?|[-+]?\.(?:inf|Inf|INF)|\.(?:nan|NaN|NAN))$`
/// plus a finite-or-special check.
fn is_core_float(s: &str) -> bool {
    let unsigned = s.strip_prefix(['-', '+']).unwrap_or(s);
    if matches!(unsigned, ".inf" | ".Inf" | ".INF") {
        return true;
    }
    if matches!(s, ".nan" | ".NaN" | ".NAN") {
        return true;
    }
    let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
        Some(p) => (&unsigned[..p], Some(&unsigned[p + 1..])),
        None => (unsigned, None),
    };
    if let Some(exp) = exponent {
        let e = exp.strip_prefix(['-', '+']).unwrap_or(exp);
        if e.is_empty() || !e.bytes().all(|c| c.is_ascii_digit()) {
            return false;
        }
    }
    let (int_part, frac_part) = match mantissa.find('.') {
        Some(p) => (&mantissa[..p], Some(&mantissa[p + 1..])),
        None => (mantissa, None),
    };
    let int_ok = int_part.bytes().all(|c| c.is_ascii_digit());
    let frac_ok = frac_part.is_none_or(|f| f.bytes().all(|c| c.is_ascii_digit()));
    let shape_ok = int_ok
        && frac_ok
        && if int_part.is_empty() {
            frac_part.is_some_and(|f| !f.is_empty())
        } else {
            true
        };
    if !shape_ok {
        return false;
    }
    unsigned.parse::<f64>().map(f64::is_finite).unwrap_or(false)
}

fn need_indent_indicator(s: &str) -> bool {
    s.trim_start_matches('\n').starts_with(' ')
}

fn block_header(s: &str, indent_per_level: usize) -> String {
    let indicator = if need_indent_indicator(s) { indent_per_level.to_string() } else { String::new() };
    let clip = s.ends_with('\n');
    let chomp = if clip && (s.ends_with("\n\n") || s == "\n") {
        "+"
    } else if clip {
        ""
    } else {
        "-"
    };
    format!("{indicator}{chomp}\n")
}

fn indent_string(s: &str, spaces: usize) -> String {
    let pad = " ".repeat(spaces);
    let mut out = String::with_capacity(s.len() + spaces * 4);
    for line in s.split_inclusive('\n') {
        if !line.is_empty() && line != "\n" {
            out.push_str(&pad);
        }
        out.push_str(line);
    }
    out
}

fn drop_ending_newline(s: &str) -> &str {
    s.strip_suffix('\n').unwrap_or(s)
}

fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        let cp = c as i64;
        match cp {
            0 => out.push_str("\\0"),
            7 => out.push_str("\\a"),
            8 => out.push_str("\\b"),
            9 => out.push_str("\\t"),
            10 => out.push_str("\\n"),
            11 => out.push_str("\\v"),
            12 => out.push_str("\\f"),
            13 => out.push_str("\\r"),
            27 => out.push_str("\\e"),
            34 => out.push_str("\\\""),
            92 => out.push_str("\\\\"),
            133 => out.push_str("\\N"),
            160 => out.push_str("\\_"),
            8232 => out.push_str("\\L"),
            8233 => out.push_str("\\P"),
            _ if is_printable(cp) => out.push(c),
            _ if cp <= 255 => out.push_str(&format!("\\x{cp:02X}")),
            _ => out.push_str(&format!("\\u{cp:04X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::dump;
    use serde_json::json;

    #[test]
    fn content_editor_frontmatter() {
        let v = json!({
            "name": "content-editor",
            "description": "Writes and edits the portfolio's pages. Reads a brief, follows the\nstyle guide, and emits the HTML into public/.\n",
            "tools": ["Read", "Write"],
            "model": "sonnet"
        });
        assert_eq!(
            dump(&v),
            "description: |\n  Writes and edits the portfolio's pages. Reads a brief, follows the\n  style guide, and emits the HTML into public/.\nmodel: sonnet\nname: content-editor\ntools:\n  - Read\n  - Write\n"
        );
    }

    #[test]
    fn scalars_that_look_like_other_types_are_quoted() {
        assert_eq!(dump(&json!({"a": "true", "b": "1", "c": "", "d": "1.5", "e": "x: y", "f": "- x"})),
            "a: 'true'\nb: '1'\nc: ''\nd: '1.5'\ne: 'x: y'\nf: '- x'\n");
    }

    /// Every node's `frontmatterHash` from the original CLI must equal
    /// sha256(dump(frontmatter)) — vectors in conformance/fixtures/yaml-vectors.
    #[test]
    fn matches_oracle_frontmatter_hashes() {
        use sha2::{Digest, Sha256};
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/conformance/golden/yaml-vectors.scan.json");
        let scan: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let mut failures = Vec::new();
        for node in scan["nodes"].as_array().unwrap() {
            // v039 holds `-0.0`; JSON.stringify writes it as `0`, so the JSON copy
            // of its frontmatter cannot reproduce the hash. Covered by the
            // parser-level test once frontmatter parsing lands.
            if node["path"] == "docs/v039.md" {
                continue;
            }
            let text = dump(&node["frontmatter"]);
            let got = hex::encode(Sha256::digest(text.as_bytes()));
            if got != node["frontmatterHash"].as_str().unwrap() {
                failures.push(format!("{}:\n{text}", node["path"]));
            }
        }
        assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
    }

    #[test]
    fn empty_and_nested() {
        assert_eq!(dump(&json!({})), "{}\n");
        assert_eq!(dump(&json!({"a": {"b": [1, {"c": null}]}, "e": []})), "a:\n  b:\n    - 1\n    - c: null\ne: []\n");
    }
}
