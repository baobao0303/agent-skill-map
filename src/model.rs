//! Wire shapes of the scan result (spec/schemas/*.schema.json). Field order
//! follows the key order the original CLI emits.

use serde::Serialize;
use serde_json::Value;

#[derive(Serialize, Clone, Copy, Default)]
pub struct Triple {
    pub frontmatter: u64,
    pub body: u64,
    pub total: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub path: String,
    pub kind: String,
    pub provider: String,
    pub body_hash: String,
    pub frontmatter_hash: String,
    pub bytes: Triple,
    pub links_out_count: u64,
    pub links_in_count: u64,
    pub external_refs_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#virtual: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frontmatter: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<Triple>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidecar: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_refs: Option<Vec<ExternalRef>>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ExternalRef {
    pub url: String,
    pub line: u64,
    pub original_trigger: String,
}

#[derive(Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Trigger {
    pub original_trigger: String,
    pub normalized_trigger: String,
}

#[derive(Serialize, Clone)]
pub struct Location {
    pub line: u64,
    pub offset: u64,
}

#[derive(Serialize, Clone)]
pub struct OccLocation {
    pub line: u64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Occurrence {
    pub extractor: String,
    pub original_trigger: String,
    pub location: Option<OccLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<&'static str>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    pub source: String,
    pub target: String,
    pub kind: &'static str,
    pub confidence: f64,
    pub sources: Vec<String>,
    pub raw: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<Trigger>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    pub occurrences: Vec<Occurrence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_target: Option<String>,
}

/// An analyzer or kernel finding. `detail`, `fix` and `data` live in an
/// ordered tail because the original emits them in a per-analyzer order
/// (most give `fix` before `data`, `name-mismatch` the reverse).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    pub analyzer_id: String,
    pub severity: &'static str,
    pub node_ids: Vec<String>,
    pub message: String,
    #[serde(flatten)]
    pub tail: serde_json::Map<String, Value>,
}

impl Issue {
    pub fn new(analyzer_id: &str, severity: &'static str, node_ids: Vec<String>, message: String) -> Self {
        Issue { analyzer_id: analyzer_id.into(), severity, node_ids, message, tail: Default::default() }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.tail.insert("detail".into(), Value::String(detail.into()));
        self
    }

    pub fn fix(mut self, summary: &str) -> Self {
        self.tail.insert("fix".into(), serde_json::json!({ "summary": summary }));
        self
    }

    pub fn data(mut self, data: Value) -> Self {
        self.tail.insert("data".into(), data);
        self
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.tail.get(key)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScannedBy {
    pub name: &'static str,
    pub version: &'static str,
    pub spec_version: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OversizedFile {
    pub path: String,
    pub bytes: u64,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub files_walked: u64,
    pub files_skipped: u64,
    pub files_oversized: u64,
    pub nodes_count: u64,
    pub links_count: u64,
    pub issues_count: u64,
    pub duration_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    pub schema_version: u32,
    pub scanned_at: i64,
    pub roots: Vec<String>,
    pub providers: Vec<&'static str>,
    pub scanned_by: ScannedBy,
    pub tokenizer: &'static str,
    pub active_provider: Option<String>,
    pub scan_ceiling: u64,
    pub scan_truncated: bool,
    pub max_render_nodes: u64,
    pub oversized_files: Vec<OversizedFile>,
    pub nodes: Vec<Node>,
    pub links: Vec<Link>,
    pub issues: Vec<Issue>,
    pub stats: Stats,
}
