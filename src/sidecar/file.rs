//! Sidecar (`<basename>.sm`) writes: skill-map's `FilesystemSidecarStore`.
//! A patch deep-merges into the current YAML mapping (`null` deletes a key),
//! the result must satisfy the sidecar schema, and it is written back as
//! canonical YAML (sorted keys) through an atomic 0600 temp-file rename.

use crate::{canon_yaml, frontmatter, scope, validate};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// `sidecarPathFor`: `x.md` → `x.sm`, anything else gains `.sm`.
pub fn path_for(md: &Path) -> PathBuf {
    let s = md.to_string_lossy();
    match s.strip_suffix(".md") {
        Some(stem) => PathBuf::from(format!("{stem}.sm")),
        None => PathBuf::from(format!("{s}.sm")),
    }
}

const FORBIDDEN_KEYS: [&str; 3] = ["__proto__", "constructor", "prototype"];

fn deep_merge(base: &Map<String, Value>, patch: &Map<String, Value>) -> Map<String, Value> {
    let mut out = base.clone();
    for (k, b) in patch {
        if FORBIDDEN_KEYS.contains(&k.as_str()) {
            continue;
        }
        match b {
            Value::Null => {
                out.remove(k);
            }
            Value::Object(bm) => {
                let empty = Map::new();
                let base_sub = out.get(k).and_then(Value::as_object).unwrap_or(&empty);
                let merged = deep_merge(base_sub, bm);
                out.insert(k.clone(), Value::Object(merged));
            }
            other => {
                out.insert(k.clone(), other.clone());
            }
        }
    }
    out
}

fn read_current(path: &Path) -> Result<Map<String, Value>, String> {
    let Ok(raw) = std::fs::read_to_string(path) else { return Ok(Map::new()) };
    match frontmatter::load(&raw).map_err(|e| e.to_string())? {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(m)) => Ok(m),
        Some(_) => Err(format!("sidecar at {} is not a YAML mapping; refusing to patch", path.display())),
    }
}

/// `readSidecarFor(...).parsed`: the sidecar root when it exists, parses as
/// a mapping and satisfies the schema; `None` otherwise.
pub fn read_valid(path: &Path) -> Option<Map<String, Value>> {
    let raw = std::fs::read_to_string(path).ok()?;
    let Ok(Some(Value::Object(m))) = frontmatter::load(&raw) else { return None };
    validate::errors(validate::SIDECAR, &Value::Object(m.clone())).is_empty().then_some(m)
}

/// The parsed `annotations` block (`None` when absent or empty).
pub fn annotations_of(root: Option<&Map<String, Value>>) -> Option<Value> {
    root?.get("annotations").filter(|a| a.as_object().is_some_and(|o| !o.is_empty())).cloned()
}

/// Merge `changes` into the sidecar at `path` and write it back.
pub fn apply_patch(path: &Path, changes: &Map<String, Value>) -> Result<(), String> {
    let merged = Value::Object(deep_merge(&read_current(path)?, changes));
    let errors = validate::errors(validate::SIDECAR, &merged);
    if !errors.is_empty() {
        let text: Vec<String> =
            errors.iter().map(|(p, m)| format!("{} {m}", if p.is_empty() { "(root)" } else { p })).collect();
        return Err(format!("sidecar patch produces a schema-invalid result at {}: {}", path.display(), text.join("; ")));
    }
    scope::atomic_write(path, &canon_yaml::dump(&merged)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn null_deletes_and_objects_merge() {
        let base = json!({ "a": { "x": 1, "y": 2 }, "b": 1 });
        let patch = json!({ "a": { "y": null, "z": 3 }, "b": null, "__proto__": { "p": 1 } });
        let out = super::deep_merge(base.as_object().unwrap(), patch.as_object().unwrap());
        assert_eq!(serde_json::Value::Object(out), json!({ "a": { "x": 1, "z": 3 } }));
    }
}
