//! A small JSON Schema (2020-12) validator over the spec schemas the plugin
//! loader needs (extension manifests, view-slot payloads). It reports errors
//! the way AJV does with `allErrors: true` (same messages, instance paths and
//! keyword order), and `format_errors` ports skill-map's `formatAjvErrors`,
//! so rejection messages match the original byte for byte.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

const BASE: &str = "https://skill-map.ai/spec/v1/";

/// Embedded schemas, keyed by their absolute `$id`.
fn registry() -> &'static HashMap<String, Value> {
    static REG: OnceLock<HashMap<String, Value>> = OnceLock::new();
    REG.get_or_init(|| {
        let raw: &[&str] = &[
            include_str!("../schema/spec/schemas/view-slots.schema.json"),
            include_str!("../schema/spec/schemas/input-types.schema.json"),
            include_str!("../schema/spec/schemas/extensions/base.schema.json"),
            include_str!("../schema/spec/schemas/extensions/analyzer.schema.json"),
            include_str!("../schema/spec/schemas/extensions/extractor.schema.json"),
            include_str!("../schema/spec/schemas/extensions/action.schema.json"),
            include_str!("../schema/spec/schemas/extensions/formatter.schema.json"),
            include_str!("../schema/spec/schemas/extensions/hook.schema.json"),
        ];
        raw.iter()
            .map(|t| {
                let v: Value = serde_json::from_str(t).expect("embedded schema parses");
                let id = v.get("$id").and_then(Value::as_str).expect("embedded schema has $id").to_string();
                (id, v)
            })
            .collect()
    })
}

/// One AJV-shaped error.
#[derive(Debug, Clone)]
pub struct SchemaError {
    pub instance_path: String,
    pub keyword: &'static str,
    pub message: String,
}

/// Which object properties a (sub)schema evaluated, for `unevaluatedProperties`.
#[derive(Default, Clone)]
struct Evaluated {
    all: bool,
    props: HashSet<String>,
}

impl Evaluated {
    fn merge(&mut self, other: Evaluated) {
        self.all |= other.all;
        self.props.extend(other.props);
    }
}

struct Ctx<'a> {
    reg: &'a HashMap<String, Value>,
}

/// Validate `data` against the schema `$id#pointer` (pointer may be empty).
pub fn validate(schema_ref: &str, data: &Value) -> Vec<SchemaError> {
    let ctx = Ctx { reg: registry() };
    let Some((schema, base)) = ctx.resolve(BASE, schema_ref) else {
        return vec![SchemaError { instance_path: String::new(), keyword: "$ref", message: format!("can't resolve reference {schema_ref}") }];
    };
    let mut errors = Vec::new();
    ctx.check(schema, &base, data, "", &mut errors);
    errors
}

/// The payload schema of a known view slot, `None` for an unknown slot.
pub fn slot_payload_ref(slot: &str) -> Option<String> {
    let vs = registry().get(&format!("{BASE}view-slots.schema.json"))?;
    vs.pointer(&format!("/$defs/payloads/{}", escape_pointer(slot)))?;
    Some(format!("{BASE}view-slots.schema.json#/$defs/payloads/{}", escape_pointer(slot)))
}

pub fn extension_ref(kind: &str) -> String {
    format!("{BASE}extensions/{kind}.schema.json")
}

fn escape_pointer(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

fn unescape_pointer(s: &str) -> String {
    s.replace("~1", "/").replace("~0", "~")
}

/// Join a relative reference onto a base URL (path part only).
fn join_url(base: &str, rel: &str) -> String {
    if rel.contains("://") {
        return rel.to_string();
    }
    let base = base.split('#').next().unwrap_or(base);
    let mut segs: Vec<&str> = base.split('/').collect();
    segs.pop(); // drop the file name
    for part in rel.split('/') {
        match part {
            ".." => {
                segs.pop();
            }
            "." | "" => {}
            p => segs.push(p),
        }
    }
    segs.join("/")
}

impl<'a> Ctx<'a> {
    /// Resolve `reference` against `base`, returning the target schema and
    /// the base URL that applies inside it.
    fn resolve(&self, base: &str, reference: &str) -> Option<(&'a Value, String)> {
        let (file, pointer) = match reference.split_once('#') {
            Some((f, p)) => (f, p),
            None => (reference, ""),
        };
        let doc_url = if file.is_empty() { base.split('#').next().unwrap_or(base).to_string() } else { join_url(base, file) };
        let doc = self.reg.get(&doc_url)?;
        let target = if pointer.is_empty() {
            doc
        } else {
            let decoded: Vec<String> = pointer.trim_start_matches('/').split('/').map(unescape_pointer).collect();
            let mut cur = doc;
            for seg in decoded {
                cur = match cur {
                    Value::Object(m) => m.get(&seg)?,
                    Value::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
                    _ => return None,
                };
            }
            cur
        };
        Some((target, doc_url))
    }

    fn check(&self, schema: &Value, base: &str, data: &Value, path: &str, errors: &mut Vec<SchemaError>) -> Evaluated {
        let mut ev = Evaluated::default();
        let s = match schema {
            Value::Bool(true) => return ev,
            Value::Bool(false) => {
                errors.push(err(path, "false schema", "boolean schema is false".into()));
                return ev;
            }
            Value::Object(m) => m,
            _ => return ev,
        };
        let base_owned;
        let base = match s.get("$id").and_then(Value::as_str) {
            Some(id) => {
                base_owned = join_url(base, id);
                base_owned.as_str()
            }
            None => base,
        };

        // `type` is checked first; typed keyword groups only apply to data
        // of their type.
        if let Some(t) = s.get("type") {
            let types: Vec<&str> = match t {
                Value::String(x) => vec![x.as_str()],
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                _ => vec![],
            };
            if !types.is_empty() && !types.iter().any(|t| type_matches(t, data)) {
                errors.push(err(path, "type", format!("must be {}", types.join(","))));
            }
        }

        // Untyped group: $ref, const, enum, then the applicators.
        if let Some(r) = s.get("$ref").and_then(Value::as_str) {
            match self.resolve(base, r) {
                Some((target, tbase)) => ev.merge(self.check(target, &tbase, data, path, errors)),
                None => errors.push(err(path, "$ref", format!("can't resolve reference {r}"))),
            }
        }
        if let Some(c) = s.get("const") {
            if !json_eq(c, data) {
                errors.push(err(path, "const", "must be equal to constant".into()));
            }
        }
        if let Some(Value::Array(opts)) = s.get("enum") {
            if !opts.iter().any(|o| json_eq(o, data)) {
                errors.push(err(path, "enum", "must be equal to one of the allowed values".into()));
            }
        }
        if let Some(n) = s.get("not") {
            let mut scratch = Vec::new();
            self.check(n, base, data, path, &mut scratch);
            if scratch.is_empty() {
                errors.push(err(path, "not", "must NOT be valid".into()));
            }
        }
        if let Some(Value::Array(branches)) = s.get("anyOf") {
            let before = errors.len();
            let mut any = false;
            for b in branches {
                let mut local = Vec::new();
                let e = self.check(b, base, data, path, &mut local);
                if local.is_empty() {
                    any = true;
                    ev.merge(e);
                } else {
                    errors.extend(local);
                }
            }
            if any {
                errors.truncate(before);
            } else {
                errors.push(err(path, "anyOf", "must match a schema in anyOf".into()));
            }
        }
        if let Some(Value::Array(branches)) = s.get("oneOf") {
            let before = errors.len();
            let mut passing = 0usize;
            let mut pass_ev = Evaluated::default();
            for b in branches {
                let mut local = Vec::new();
                let e = self.check(b, base, data, path, &mut local);
                if local.is_empty() {
                    passing += 1;
                    pass_ev = e;
                } else {
                    errors.extend(local);
                }
            }
            if passing == 1 {
                errors.truncate(before);
                ev.merge(pass_ev);
            } else {
                errors.push(err(path, "oneOf", "must match exactly one schema in oneOf".into()));
            }
        }
        if let Some(Value::Array(parts)) = s.get("allOf") {
            for p in parts {
                ev.merge(self.check(p, base, data, path, errors));
            }
        }
        if let Some(cond) = s.get("if") {
            let mut scratch = Vec::new();
            let cond_ev = self.check(cond, base, data, path, &mut scratch);
            let (branch, name) = if scratch.is_empty() { (s.get("then"), "then") } else { (s.get("else"), "else") };
            if scratch.is_empty() {
                ev.merge(cond_ev);
            }
            if let Some(b) = branch {
                let before = errors.len();
                let e = self.check(b, base, data, path, errors);
                if errors.len() > before {
                    errors.push(err(path, "if", format!("must match \"{name}\" schema")));
                } else {
                    ev.merge(e);
                }
            }
        }

        match data {
            Value::Number(n) => self.check_number(s, n.as_f64().unwrap_or(0.0), path, errors),
            Value::String(text) => self.check_string(s, text, path, errors),
            Value::Array(items) => self.check_array(s, base, items, path, errors),
            Value::Object(obj) => {
                let own = self.check_object(s, base, obj, path, errors);
                ev.merge(own);
                if let Some(u) = s.get("unevaluatedProperties") {
                    if !ev.all {
                        for (k, v) in obj {
                            if ev.props.contains(k) {
                                continue;
                            }
                            if u == &Value::Bool(false) {
                                errors.push(err(path, "unevaluatedProperties", "must NOT have unevaluated properties".into()));
                            } else {
                                self.check(u, base, v, &child(path, k), errors);
                            }
                        }
                    }
                    ev.all = true;
                }
            }
            _ => {}
        }
        ev
    }

    fn check_number(&self, s: &serde_json::Map<String, Value>, n: f64, path: &str, errors: &mut Vec<SchemaError>) {
        if let Some(m) = s.get("maximum").and_then(Value::as_f64) {
            if n > m {
                errors.push(err(path, "maximum", format!("must be <= {}", js_num(m))));
            }
        }
        if let Some(m) = s.get("minimum").and_then(Value::as_f64) {
            if n < m {
                errors.push(err(path, "minimum", format!("must be >= {}", js_num(m))));
            }
        }
        if let Some(m) = s.get("exclusiveMaximum").and_then(Value::as_f64) {
            if n >= m {
                errors.push(err(path, "exclusiveMaximum", format!("must be < {}", js_num(m))));
            }
        }
        if let Some(m) = s.get("exclusiveMinimum").and_then(Value::as_f64) {
            if n <= m {
                errors.push(err(path, "exclusiveMinimum", format!("must be > {}", js_num(m))));
            }
        }
    }

    fn check_string(&self, s: &serde_json::Map<String, Value>, text: &str, path: &str, errors: &mut Vec<SchemaError>) {
        let len = text.chars().count() as f64;
        if let Some(m) = s.get("maxLength").and_then(Value::as_f64) {
            if len > m {
                errors.push(err(path, "maxLength", format!("must NOT have more than {} characters", js_num(m))));
            }
        }
        if let Some(m) = s.get("minLength").and_then(Value::as_f64) {
            if len < m {
                errors.push(err(path, "minLength", format!("must NOT have fewer than {} characters", js_num(m))));
            }
        }
        if let Some(p) = s.get("pattern").and_then(Value::as_str) {
            if !pattern_matches(p, text) {
                errors.push(err(path, "pattern", format!("must match pattern \"{p}\"")));
            }
        }
    }

    fn check_array(&self, s: &serde_json::Map<String, Value>, base: &str, items: &[Value], path: &str, errors: &mut Vec<SchemaError>) {
        let len = items.len() as f64;
        if let Some(m) = s.get("maxItems").and_then(Value::as_f64) {
            if len > m {
                errors.push(err(path, "maxItems", format!("must NOT have more than {} items", js_num(m))));
            }
        }
        if let Some(m) = s.get("minItems").and_then(Value::as_f64) {
            if len < m {
                errors.push(err(path, "minItems", format!("must NOT have fewer than {} items", js_num(m))));
            }
        }
        if s.get("uniqueItems") == Some(&Value::Bool(true)) {
            'outer: for i in (0..items.len()).rev() {
                for j in (0..i).rev() {
                    if json_eq(&items[i], &items[j]) {
                        errors.push(err(path, "uniqueItems", format!("must NOT have duplicate items (items ## {j} and {i} are identical)")));
                        break 'outer;
                    }
                }
            }
        }
        let prefix = match s.get("prefixItems") {
            Some(Value::Array(p)) => {
                for (i, (sch, v)) in p.iter().zip(items).enumerate() {
                    self.check(sch, base, v, &format!("{path}/{i}"), errors);
                }
                p.len()
            }
            _ => 0,
        };
        if let Some(it) = s.get("items") {
            for (i, v) in items.iter().enumerate().skip(prefix) {
                self.check(it, base, v, &format!("{path}/{i}"), errors);
            }
        }
    }

    fn check_object(&self, s: &serde_json::Map<String, Value>, base: &str, obj: &serde_json::Map<String, Value>, path: &str, errors: &mut Vec<SchemaError>) -> Evaluated {
        let mut ev = Evaluated::default();
        if let Some(Value::Array(req)) = s.get("required") {
            for r in req.iter().filter_map(Value::as_str) {
                if !obj.contains_key(r) {
                    errors.push(err(path, "required", format!("must have required property '{r}'")));
                }
            }
        }
        if let Some(pn) = s.get("propertyNames") {
            for k in obj.keys() {
                let before = errors.len();
                self.check(pn, base, &Value::String(k.clone()), path, errors);
                if errors.len() > before {
                    errors.push(err(path, "propertyNames", "property name must be valid".into()));
                }
            }
        }
        let props = s.get("properties").and_then(Value::as_object);
        let pattern_props = s.get("patternProperties").and_then(Value::as_object);
        if let Some(ap) = s.get("additionalProperties") {
            for (k, v) in obj {
                let declared = props.is_some_and(|p| p.contains_key(k))
                    || pattern_props.is_some_and(|pp| pp.keys().any(|p| pattern_matches(p, k)));
                if declared {
                    continue;
                }
                if ap == &Value::Bool(false) {
                    errors.push(err(path, "additionalProperties", "must NOT have additional properties".into()));
                } else {
                    self.check(ap, base, v, &child(path, k), errors);
                }
            }
            ev.all = true;
        }
        if let Some(p) = props {
            for (k, sch) in p {
                if let Some(v) = obj.get(k) {
                    self.check(sch, base, v, &child(path, k), errors);
                    ev.props.insert(k.clone());
                }
            }
        }
        if let Some(pp) = pattern_props {
            for (pat, sch) in pp {
                for (k, v) in obj {
                    if pattern_matches(pat, k) {
                        self.check(sch, base, v, &child(path, k), errors);
                        ev.props.insert(k.clone());
                    }
                }
            }
        }
        ev
    }
}

fn err(path: &str, keyword: &'static str, message: String) -> SchemaError {
    SchemaError { instance_path: path.to_string(), keyword, message }
}

fn child(path: &str, key: &str) -> String {
    format!("{path}/{}", escape_pointer(key))
}

fn type_matches(t: &str, v: &Value) -> bool {
    match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "number" => v.is_number(),
        "integer" => v.as_f64().is_some_and(|n| n.fract() == 0.0),
        _ => false,
    }
}

/// Deep equality with numbers compared by value (JSON semantics).
fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| json_eq(p, q)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| json_eq(v, w))),
        _ => a == b,
    }
}

fn js_num(n: f64) -> String {
    crate::jsjson::to_string(&serde_json::json!(n))
}

fn pattern_matches(pattern: &str, text: &str) -> bool {
    static CACHE: OnceLock<std::sync::Mutex<HashMap<String, Option<fancy_regex::Regex>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    let re = guard.entry(pattern.to_string()).or_insert_with(|| fancy_regex::Regex::new(pattern).ok());
    match re {
        Some(r) => r.is_match(text).unwrap_or(false),
        None => true,
    }
}

/// skill-map's `formatAjvErrors`: group by instance path (first-seen
/// order), collapse a multi-`const` branch failure into "is not a valid
/// value", dedupe, join with "; ".
pub fn format_errors(errors: &[SchemaError]) -> String {
    let mut order: Vec<String> = Vec::new();
    let mut by_path: HashMap<String, Vec<&SchemaError>> = HashMap::new();
    for e in errors {
        let p = if e.instance_path.is_empty() { "(root)".to_string() } else { e.instance_path.clone() };
        if !by_path.contains_key(&p) {
            order.push(p.clone());
        }
        by_path.entry(p).or_default().push(e);
    }
    let mut parts: Vec<String> = Vec::new();
    for p in &order {
        let errs = &by_path[p];
        let consts = errs.iter().filter(|e| e.keyword == "const").count();
        if consts >= 2 {
            parts.push(format!("{p} is not a valid value"));
            for e in errs.iter().filter(|e| e.keyword != "const" && e.keyword != "oneOf") {
                parts.push(format!("{p} {}", e.message));
            }
        } else {
            let mut seen = HashSet::new();
            for e in errs {
                let m = format!("{p} {}", e.message);
                if seen.insert(m.clone()) {
                    parts.push(m);
                }
            }
        }
    }
    let mut seen = HashSet::new();
    parts.retain(|m| seen.insert(m.clone()));
    parts.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload_errors(slot: &str, payload: Value) -> String {
        format_errors(&validate(&slot_payload_ref(slot).unwrap(), &payload))
    }

    #[test]
    fn key_values_missing_value_matches_ajv() {
        let e = payload_errors("inspector.body.panel.key-values", json!({ "pairs": [{ "key": "broken" }] }));
        assert_eq!(e, "/pairs/0 must have required property 'value'");
    }

    #[test]
    fn valid_payload_has_no_errors() {
        assert!(payload_errors("inspector.body.panel.breakdown", json!({ "bars": [{ "label": "len", "value": 1 }] })).is_empty());
    }

    #[test]
    fn unknown_slot_collapses_to_not_a_valid_value() {
        let manifest = json!({ "mode": "deterministic", "ui": { "badge": { "slot": "nope" } } });
        let e = format_errors(&validate(&extension_ref("analyzer"), &manifest));
        assert_eq!(e, "/ui/badge/slot is not a valid value");
    }

    #[test]
    fn fold_orders_set_delta_floor_ceil() {
        let op = |k: &str, v: f64| (String::new(), String::new(), k.to_string(), v);
        let f = crate::plugins::runtime::fold_confidence;
        assert_eq!(f(1.0, &[op("delta", -0.4), op("floor", 0.5)]), 0.6);
        assert_eq!(f(1.0, &[op("ceil", 0.5), op("delta", 0.25), op("set", 0.3)]), 0.5);
        assert_eq!(f(0.2, &[op("delta", -1.0)]), 0.0);
    }
}
