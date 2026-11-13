//! Minimal JSON Schema (draft 2020-12) validator for the keywords skill-map's
//! frontmatter and sidecar schemas use, producing AJV's error wording
//! (`<instancePath> <message>`), grouped and deduped the way skill-map's
//! `formatAjvErrors` does. Type-specific keywords only run when the data has
//! that type, as in AJV.

use fancy_regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::LazyLock;

macro_rules! schemas {
    ($($file:literal),* $(,)?) => {
        [$(include_str!(concat!("schema/json/", $file))),*]
    };
}

static REGISTRY: LazyLock<HashMap<String, Value>> = LazyLock::new(|| {
    schemas![
        "frontmatter-base.schema.json",
        "sidecar.schema.json",
        "annotations.schema.json",
        "agent-skills-skill.schema.json",
        "antigravity-workflow.schema.json",
        "claude-agent.schema.json",
        "claude-command.schema.json",
        "claude-skill-base.schema.json",
        "claude-skill.schema.json",
        "codex-agent.schema.json",
        "markdown-markdown.schema.json",
        "opencode-agent.schema.json",
        "opencode-command.schema.json",
    ]
    .iter()
    .map(|text| {
        let v: Value = serde_json::from_str(text).expect("bundled schema");
        (v["$id"].as_str().expect("$id").to_string(), v)
    })
    .collect()
});

pub const SIDECAR: &str = "https://skill-map.ai/spec/v1/sidecar.schema.json";

/// Frontmatter schema `$id` of a provider kind (spec_doc §7.3).
pub fn kind_schema(provider: &str, kind: &str) -> Option<&'static str> {
    Some(match (provider, kind) {
        ("claude", "agent") => "https://skill-map.ai/providers/claude/v1/frontmatter/agent.schema.json",
        ("claude", "command") => "https://skill-map.ai/providers/claude/v1/frontmatter/command.schema.json",
        ("claude", "skill" | "mcp") => "https://skill-map.ai/providers/claude/v1/frontmatter/skill.schema.json",
        ("agent-skills" | "antigravity" | "codex" | "opencode", "skill") => {
            "https://skill-map.ai/providers/agent-skills/v1/frontmatter/skill.schema.json"
        }
        ("antigravity", "workflow") => "https://skill-map.ai/providers/antigravity/v1/frontmatter/workflow.schema.json",
        ("codex", "agent") => "https://skill-map.ai/providers/codex/v1/frontmatter/agent.schema.json",
        ("opencode", "agent") => "https://skill-map.ai/providers/opencode/v1/frontmatter/agent.schema.json",
        ("opencode", "command") => "https://skill-map.ai/providers/opencode/v1/frontmatter/command.schema.json",
        ("markdown", "markdown") => "https://skill-map.ai/providers/core/v1/frontmatter/markdown.schema.json",
        _ => return None,
    })
}

pub fn schema(id: &str) -> Option<&'static Value> {
    REGISTRY.get(id)
}

/// Raw `(instancePath, message)` errors in evaluation order.
pub fn errors(schema_id: &str, data: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(s) = REGISTRY.get(schema_id) {
        eval(s, schema_id, data, "", &mut out);
    }
    out
}

/// skill-map's `formatAjvErrors`: group by path (first seen), dedupe, join.
pub fn format(errors: &[(String, String)]) -> String {
    let mut paths: Vec<&str> = Vec::new();
    for (p, _) in errors {
        if !paths.contains(&p.as_str()) {
            paths.push(p);
        }
    }
    let mut parts: Vec<String> = Vec::new();
    for p in paths {
        let shown = if p.is_empty() { "(root)" } else { p };
        for (_, m) in errors.iter().filter(|(q, _)| q == p) {
            let line = format!("{shown} {m}");
            if !parts.contains(&line) {
                parts.push(line);
            }
        }
    }
    parts.join("; ")
}

fn resolve<'a>(base: &str, reference: &str) -> Option<(&'a Value, String)> {
    let (doc, fragment) = match reference.split_once('#') {
        Some(("", frag)) => (base.to_string(), frag),
        Some((doc, frag)) => (join_id(base, doc), frag),
        None => (join_id(base, reference), ""),
    };
    let mut node = REGISTRY.get(&doc)?;
    for seg in fragment.split('/').filter(|s| !s.is_empty()) {
        node = node.get(seg)?;
    }
    Some((node, doc))
}

fn join_id(base: &str, reference: &str) -> String {
    if reference.contains("://") {
        return reference.to_string();
    }
    match base.rfind('/') {
        Some(i) => format!("{}/{}", &base[..i], reference),
        None => reference.to_string(),
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.as_f64().is_some_and(|f| f.fract() == 0.0) => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn type_matches(expected: &str, v: &Value) -> bool {
    let actual = type_name(v);
    expected == actual || (expected == "number" && actual == "integer")
}

fn json_pointer_escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn eval(schema: &Value, base: &str, data: &Value, path: &str, out: &mut Vec<(String, String)>) {
    let Some(s) = schema.as_object() else { return };
    let mut err = |m: String| out.push((path.to_string(), m));

    if let Some(types) = s.get("type") {
        let list: Vec<&str> = match types {
            Value::String(t) => vec![t.as_str()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => vec![],
        };
        if !list.is_empty() && !list.iter().any(|t| type_matches(t, data)) {
            err(format!("must be {}", list.join(",")));
        }
    }
    if let Some(allowed) = s.get("enum").and_then(Value::as_array)
        && !allowed.contains(data)
    {
        err("must be equal to one of the allowed values".into());
    }
    if let Value::String(text) = data {
        let len = text.chars().count();
        if let Some(max) = s.get("maxLength").and_then(Value::as_u64)
            && len as u64 > max
        {
            err(format!("must NOT have more than {max} characters"));
        }
        if let Some(min) = s.get("minLength").and_then(Value::as_u64)
            && (len as u64) < min
        {
            err(format!("must NOT have fewer than {min} characters"));
        }
        if let Some(p) = s.get("pattern").and_then(Value::as_str)
            && let Ok(re) = Regex::new(p)
            && !re.is_match(text).unwrap_or(true)
        {
            err(format!("must match pattern \"{p}\""));
        }
        if let Some(f) = s.get("format").and_then(Value::as_str)
            && !format_ok(f, text)
        {
            err(format!("must match format \"{f}\""));
        }
    }
    if let Value::Number(n) = data
        && let (Some(min), Some(x)) = (s.get("minimum").and_then(Value::as_f64), n.as_f64())
        && x < min
    {
        err(format!("must be >= {}", crate::jsjson::js_number(min)));
    }
    drop(err);
    if let (Value::Array(items), Some(item_schema)) = (data, s.get("items")) {
        for (i, item) in items.iter().enumerate() {
            eval(item_schema, base, item, &format!("{path}/{i}"), out);
        }
    }
    if let Value::Object(obj) = data {
        if let Some(req) = s.get("required").and_then(Value::as_array) {
            for k in req.iter().filter_map(Value::as_str) {
                if !obj.contains_key(k) {
                    out.push((path.to_string(), format!("must have required property '{k}'")));
                }
            }
        }
        let props = s.get("properties").and_then(Value::as_object);
        if s.get("additionalProperties") == Some(&Value::Bool(false)) {
            for k in obj.keys() {
                if !props.is_some_and(|p| p.contains_key(k)) {
                    out.push((path.to_string(), "must NOT have additional properties".into()));
                }
            }
        }
        if let Some(props) = props {
            for (k, sub) in props {
                if let Some(v) = obj.get(k) {
                    eval(sub, base, v, &format!("{path}/{}", json_pointer_escape(k)), out);
                }
            }
        }
    }
    if let Some(r) = s.get("$ref").and_then(Value::as_str)
        && let Some((target, doc)) = resolve(base, r)
    {
        eval(target, &doc, data, path, out);
    }
    if let Some(all) = s.get("allOf").and_then(Value::as_array) {
        for sub in all {
            eval(sub, base, data, path, out);
        }
    }
    if let Some(one) = s.get("oneOf").and_then(Value::as_array) {
        let mut branch_errors = Vec::new();
        let mut passing = 0;
        for sub in one {
            let mut e = Vec::new();
            eval(sub, base, data, path, &mut e);
            if e.is_empty() {
                passing += 1;
            }
            branch_errors.extend(e);
        }
        if passing != 1 {
            if passing == 0 {
                out.extend(branch_errors);
            }
            out.push((path.to_string(), "must match exactly one schema in oneOf".into()));
        }
    }
}

fn format_ok(format: &str, text: &str) -> bool {
    static DATE_TIME: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\d{4}-\d{2}-\d{2}[Tt ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:[Zz]|[+-]\d{2}(?::?\d{2})?)$").unwrap()
    });
    match format {
        "date-time" => DATE_TIME.is_match(text).unwrap_or(false),
        "uri" => url::Url::parse(text).is_ok(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ajv_wording() {
        let id = kind_schema("claude", "skill").unwrap();
        assert_eq!(format(&errors(id, &json!({"name": 42, "description": ["x"]}))), "/name must be string; /description must be string");
        let e = errors(SIDECAR, &json!({"identity": {"path": "a.md", "bodyHash": "x", "frontmatterHash": "y"}}));
        assert_eq!(
            format(&e),
            "/identity/bodyHash must match pattern \"^[a-f0-9]{64}$\"; /identity/frontmatterHash must match pattern \"^[a-f0-9]{64}$\""
        );
        assert_eq!(format(&errors(SIDECAR, &json!({}))), "(root) must have required property 'identity'");
    }
}
