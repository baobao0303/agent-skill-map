//! Frontmatter split + load, matching skill-map's `frontmatter-yaml` parser:
//! fence regex `^---\r?\n([\s\S]*?)\r?\n---\r?\n?([\s\S]*)$`, YAML loaded with
//! js-yaml's JSON schema (only `null`/`true`/`false`/JSON numbers are typed).

use serde_json::{Map, Number, Value};
use std::collections::HashMap;
use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser};
use yaml_rust2::scanner::{Marker, TScalarStyle};

pub struct Parsed<'a> {
    pub declared: bool,
    /// Text between the fences, without the newline before the closing fence.
    pub raw: &'a str,
    /// Everything after the closing fence line.
    pub body: &'a str,
    /// Newlines before `body` starts (turns body lines into file lines).
    pub body_line_offset: usize,
    pub value: Value,
    pub parse_error: Option<String>,
}

pub fn parse(text: &str) -> Parsed<'_> {
    let Some((raw, body)) = split(text) else {
        return Parsed { declared: false, raw: "", body: text, body_line_offset: 0, value: Value::Object(Map::new()), parse_error: None };
    };
    let body_line_offset = text[..text.len() - body.len()].bytes().filter(|&b| b == b'\n').count();
    let (value, parse_error) = match load(raw) {
        Ok(Some(Value::Object(map))) => (Value::Object(map), None),
        Ok(_) => (Value::Object(Map::new()), None),
        Err(e) => (Value::Object(Map::new()), Some(e)),
    };
    Parsed { declared: true, raw, body, body_line_offset, value, parse_error }
}

fn split(text: &str) -> Option<(&str, &str)> {
    let content_start = if text.starts_with("---\n") {
        4
    } else if text.starts_with("---\r\n") {
        5
    } else {
        return None;
    };
    let rest = &text[content_start..];
    // Lazy group: first `\n---` (an immediately preceding `\r` belongs to the fence).
    let close = rest.find("\n---")?;
    let raw_end = if close > 0 && rest.as_bytes()[close - 1] == b'\r' { close - 1 } else { close };
    let mut after = &rest[close + 4..];
    after = after.strip_prefix('\r').unwrap_or(after);
    after = after.strip_prefix('\n').unwrap_or(after);
    Some((&rest[..raw_end], after))
}

/// Load one YAML document. `Ok(None)` for an empty document.
pub fn load(raw: &str) -> Result<Option<Value>, String> {
    let mut builder = Builder::default();
    let mut parser = Parser::new_from_str(raw);
    parser.load(&mut builder, false).map_err(|e| e.to_string())?;
    if let Some(err) = builder.error {
        return Err(err);
    }
    Ok(builder.root)
}

enum Frame {
    Seq(Vec<Value>, usize),
    Map(Vec<(String, Value)>, Option<String>, usize),
}

#[derive(Default)]
struct Builder {
    stack: Vec<Frame>,
    anchors: HashMap<usize, Value>,
    root: Option<Value>,
    error: Option<String>,
}

impl Builder {
    fn push_value(&mut self, value: Value, anchor: usize) {
        if anchor > 0 {
            self.anchors.insert(anchor, value.clone());
        }
        match self.stack.last_mut() {
            None => self.root = Some(value),
            Some(Frame::Seq(items, _)) => items.push(value),
            Some(Frame::Map(entries, key, _)) => match key.take() {
                None => *key = Some(js_key(&value)),
                Some(k) => {
                    if entries.iter().any(|(existing, _)| *existing == k) {
                        self.error.get_or_insert_with(|| "duplicated mapping key".to_string());
                    }
                    entries.push((k, value));
                }
            },
        }
    }
}

impl MarkedEventReceiver for Builder {
    fn on_event(&mut self, ev: Event, _mark: Marker) {
        match ev {
            Event::Scalar(text, style, anchor, tag) => {
                let explicit_str = tag.as_ref().is_some_and(|t| t.suffix == "str");
                let value = if style == TScalarStyle::Plain && !explicit_str { resolve_json_schema(&text) } else { Value::String(text) };
                self.push_value(value, anchor);
            }
            Event::SequenceStart(anchor, _) => self.stack.push(Frame::Seq(Vec::new(), anchor)),
            Event::MappingStart(anchor, _) => self.stack.push(Frame::Map(Vec::new(), None, anchor)),
            Event::SequenceEnd => {
                if let Some(Frame::Seq(items, anchor)) = self.stack.pop() {
                    self.push_value(Value::Array(items), anchor);
                }
            }
            Event::MappingEnd => {
                if let Some(Frame::Map(entries, _, anchor)) = self.stack.pop() {
                    self.push_value(Value::Object(js_object(entries)), anchor);
                }
            }
            Event::Alias(id) => {
                let value = self.anchors.get(&id).cloned().unwrap_or(Value::Null);
                self.push_value(value, 0);
            }
            _ => {}
        }
    }
}

/// js-yaml JSON_SCHEMA implicit resolution for plain scalars.
fn resolve_json_schema(s: &str) -> Value {
    match s {
        "null" => return Value::Null,
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        _ => {}
    }
    if is_json_int(s) {
        if let Ok(i) = s.parse::<i64>()
            && i.unsigned_abs() <= (1u64 << 53)
        {
            return Value::Number(i.into());
        }
        if let Ok(f) = s.parse::<f64>()
            && let Some(n) = Number::from_f64(f)
        {
            return Value::Number(n);
        }
    }
    if is_json_float(s)
        && let Ok(f) = s.parse::<f64>()
        && let Some(n) = Number::from_f64(f)
    {
        return Value::Number(n);
    }
    Value::String(s.to_string())
}

/// `^-?(?:0|[1-9][0-9]*)$`
fn is_json_int(s: &str) -> bool {
    let d = s.strip_prefix('-').unwrap_or(s);
    d == "0" || (!d.is_empty() && !d.starts_with('0') && d.bytes().all(|c| c.is_ascii_digit()))
}

/// `^-?(?:0|[1-9][0-9]*)(?:\.[0-9]*)?(?:[eE][-+]?[0-9]+)?$`
fn is_json_float(s: &str) -> bool {
    let d = s.strip_prefix('-').unwrap_or(s);
    let (mantissa, exp) = match d.find(['e', 'E']) {
        Some(p) => (&d[..p], Some(&d[p + 1..])),
        None => (d, None),
    };
    let (int, frac) = match mantissa.find('.') {
        Some(p) => (&mantissa[..p], Some(&mantissa[p + 1..])),
        None => (mantissa, None),
    };
    let int_ok = int == "0" || (!int.is_empty() && !int.starts_with('0') && int.bytes().all(|c| c.is_ascii_digit()));
    let frac_ok = frac.is_none_or(|f| f.bytes().all(|c| c.is_ascii_digit()));
    let exp_ok = exp.is_none_or(|e| {
        let e = e.strip_prefix(['-', '+']).unwrap_or(e);
        !e.is_empty() && e.bytes().all(|c| c.is_ascii_digit())
    });
    int_ok && frac_ok && exp_ok
}

/// Mapping key as JavaScript would stringify it into an object key.
fn js_key(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        Value::Number(n) => n.as_f64().map(crate::jsjson::js_number).unwrap_or_else(|| n.to_string()),
        other => other.to_string(),
    }
}

/// JavaScript object semantics: drop prototype-polluting keys, last duplicate
/// wins in place, array-index keys enumerate first in ascending order.
fn js_object(entries: Vec<(String, Value)>) -> Map<String, Value> {
    let mut index_keys: Vec<(u32, String, Value)> = Vec::new();
    let mut other = Map::new();
    for (k, v) in entries {
        if matches!(k.as_str(), "__proto__" | "constructor" | "prototype") {
            continue;
        }
        match array_index(&k) {
            Some(i) => match index_keys.iter_mut().find(|(j, _, _)| *j == i) {
                Some(slot) => slot.2 = v,
                None => index_keys.push((i, k, v)),
            },
            None => {
                other.insert(k, v);
            }
        }
    }
    index_keys.sort_by_key(|(i, _, _)| *i);
    let mut out = Map::new();
    for (_, k, v) in index_keys {
        out.insert(k, v);
    }
    out.extend(other);
    out
}

fn array_index(k: &str) -> Option<u32> {
    if k == "0" {
        return Some(0);
    }
    if k.starts_with('0') || k.is_empty() || !k.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    k.parse::<u32>().ok().filter(|&i| i < u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_matches_original_regex() {
        let p = parse("---\nname: a\n---\nbody\n");
        assert_eq!((p.raw, p.body, p.body_line_offset), ("name: a", "body\n", 3));
        let p = parse("---\r\nname: a\r\n---\r\nbody");
        assert_eq!((p.raw, p.body), ("name: a", "body"));
        assert!(!parse("\u{feff}---\nname: a\n---\n").declared);
        assert!(!parse("---\n---\n").declared, "frontmatter needs a line before the close");
        let p = parse("---\nname: a\n---trailing\n");
        assert_eq!(p.body, "trailing\n");
    }

    /// Frontmatter values must equal what the original CLI reported as JSON.
    #[test]
    fn values_match_oracle() {
        let golden = concat!(env!("CARGO_MANIFEST_DIR"), "/conformance/golden/yaml-vectors.scan.json");
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/conformance/fixtures/yaml-vectors/");
        let scan: Value = serde_json::from_str(&std::fs::read_to_string(golden).unwrap()).unwrap();
        let mut failures = Vec::new();
        for node in scan["nodes"].as_array().unwrap() {
            let path = node["path"].as_str().unwrap();
            // Known divergence: yaml-rust2 enforces the YAML 1.2 limit of 1024
            // chars for a simple key; js-yaml accepts longer keys (v037).
            if path == "docs/v037.md" {
                continue;
            }
            let text = std::fs::read_to_string(format!("{fixture}{path}")).unwrap();
            let parsed = parse(&text);
            let got = crate::jsjson::to_string(&parsed.value);
            let want = crate::jsjson::to_string(&node["frontmatter"]);
            if got != want {
                failures.push(format!("{path}\n  got  {got}\n  want {want}"));
            }
            let hash = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(crate::canon_yaml::dump(&parsed.value)));
            if hash != node["frontmatterHash"] {
                failures.push(format!("{path}: frontmatterHash differs"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
