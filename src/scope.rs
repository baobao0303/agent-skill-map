//! The `.skill-map/` project scope: provisioning, settings, operations log.

use serde_json::{Map, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DIR: &str = ".skill-map";
const SCOPE_GITIGNORE: &str = include_str!("schema/scope.gitignore");

pub struct Scope {
    pub root: PathBuf,
}

impl Scope {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn dir(&self) -> PathBuf {
        self.root.join(DIR)
    }

    pub fn db_path(&self) -> PathBuf {
        self.dir().join("skill-map.db")
    }

    pub fn backups_dir(&self) -> PathBuf {
        self.dir().join("backups")
    }

    /// Create `.skill-map/` and its `.gitignore` when missing.
    pub fn provision(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.dir())?;
        let gi = self.dir().join(".gitignore");
        if !gi.exists() {
            std::fs::write(gi, SCOPE_GITIGNORE)?;
        }
        Ok(())
    }

    pub fn settings(&self) -> Map<String, Value> {
        read_object(&self.dir().join("settings.json"))
    }

    pub fn local_settings(&self) -> Map<String, Value> {
        read_object(&self.dir().join("settings.local.json"))
    }

    pub fn write_settings(&self, settings: &Map<String, Value>) -> std::io::Result<()> {
        let mut text = crate::jsjson::to_string_pretty(settings);
        text.push('\n');
        std::fs::write(self.dir().join("settings.json"), text)
    }

    /// Append one NDJSON record to `operations.log`.
    pub fn log_operation(&self, op: &str, outcome: &str, detail: &str) {
        let at = time::OffsetDateTime::now_utc();
        let at = format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            at.year(),
            at.month() as u8,
            at.day(),
            at.hour(),
            at.minute(),
            at.second(),
            at.millisecond()
        );
        let line = crate::jsjson::to_string(&serde_json::json!({
            "at": at, "op": op, "target": "*", "channel": "cli", "outcome": outcome, "detail": detail,
        }));
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.dir().join("operations.log")) {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn read_object(path: &Path) -> Map<String, Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}
