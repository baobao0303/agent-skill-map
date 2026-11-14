//! The `.skill-map/` project scope: provisioning, settings, operations log.

use serde_json::{Map, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DIR: &str = ".skill-map";
const LOCK_FILE: &str = "scope.lock.json";
const GRANT_NAMESPACE: &str = "local-config";
const SCOPE_GITIGNORE: &str = include_str!("../schema/scope.gitignore");

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

    pub fn write_settings(&self, settings: &Map<String, Value>) -> std::io::Result<()> {
        write_json(&self.dir().join("settings.json"), &Value::Object(settings.clone()))
    }

    /// `.skill-map/settings.local.json` (gitignored, per checkout).
    pub fn settings_local(&self) -> Map<String, Value> {
        read_object(&self.dir().join("settings.local.json"))
    }

    /// `allowSidecarWriters` (team policy, committed settings; default true).
    pub fn sidecar_writers_allowed(&self) -> bool {
        self.settings().get("allowSidecarWriters") != Some(&Value::Bool(false))
    }

    /// A project-local-only key counts only when its grant, minted against
    /// this checkout's `.skill-map/` inode + birth time, verifies.
    pub fn local_key(&self, key: &str) -> Option<Value> {
        let value = self.settings_local().get(key).cloned()?;
        let lock = read_object(&self.dir().join(LOCK_FILE));
        let recorded = lock.get("namespaces")?.get(GRANT_NAMESPACE)?.get(key)?.get("grant")?.as_str()?.to_string();
        if self.grant(key, &value).as_deref() == Some(recorded.as_str()) {
            Some(value)
        } else {
            eprintln!(
                "config: '{key}' in settings.local.json was not granted in this copy of the project and was ignored. Re-apply it with `sm config set {key} <value>` to record consent here."
            );
            None
        }
    }

    /// Write a project-local-only key and record its grant in `scope.lock.json`.
    pub fn grant_local_key(&self, key: &str, value: Value) -> std::io::Result<()> {
        let path = self.dir().join("settings.local.json");
        let mut local = read_object(&path);
        local.insert(key.into(), value.clone());
        write_json(&path, &Value::Object(local))?;
        let Some(grant) = self.grant(key, &value) else {
            return Err(std::io::Error::other(format!(
                "cannot record consent for '{key}': this filesystem reports no creation time for .skill-map/"
            )));
        };
        let lock_path = self.dir().join(LOCK_FILE);
        let lock = read_object(&lock_path);
        let mut namespaces = lock.get("namespaces").and_then(Value::as_object).cloned().unwrap_or_default();
        let mut bucket = namespaces.get(GRANT_NAMESPACE).and_then(Value::as_object).cloned().unwrap_or_default();
        bucket.insert(key.into(), serde_json::json!({ "grant": grant, "grantedAt": crate::scan::now_ms() }));
        namespaces.insert(GRANT_NAMESPACE.into(), Value::Object(bucket));
        write_json(&lock_path, &serde_json::json!({ "v": 1, "namespaces": namespaces }))
    }

    /// `computeScopeGrant`: sha256 over length-prefixed parts.
    fn grant(&self, key: &str, value: &Value) -> Option<String> {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(self.dir()).ok().filter(|m| m.is_dir())?;
        let born = meta.created().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
        if born == 0 {
            return None;
        }
        let parts = [
            "skill-map/scope-grant/v1".to_string(),
            GRANT_NAMESPACE.to_string(),
            meta.ino().to_string(),
            born.to_string(),
            key.to_string(),
            crate::jsjson::to_string(value),
        ];
        let preimage: String = parts.iter().map(|p| format!("{}:{p}", p.encode_utf16().count())).collect();
        Some(hex::encode(Sha256::digest(preimage.as_bytes())))
    }

    /// Append one NDJSON record to `operations.log`.
    pub fn log_operation(&self, op: &str, channel: &str, outcome: &str, detail: &str) {
        self.log_entry(serde_json::json!({
            "op": op, "target": "*", "channel": channel, "outcome": outcome, "detail": detail,
        }));
    }

    /// Append `{ at, ...entry }`; silently skipped when `.skill-map/` is absent.
    pub fn log_entry(&self, entry: Value) {
        if !self.dir().is_dir() {
            return;
        }
        let mut obj = Map::new();
        obj.insert("at".into(), Value::String(iso_now()));
        if let Value::Object(m) = entry {
            obj.extend(m);
        }
        let line = crate::jsjson::to_string(&Value::Object(obj));
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.dir().join("operations.log")) {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// `new Date().toISOString()`.
pub fn iso_now() -> String {
    let at = time::OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        at.year(),
        at.month() as u8,
        at.day(),
        at.hour(),
        at.minute(),
        at.second(),
        at.millisecond()
    )
}

/// `writeFileAtomicExclusive`: a 0600 temp file beside the target, renamed over it.
pub fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).map_err(|e| std::io::Error::other(e.to_string()))?;
    let tmp = PathBuf::from(format!("{}.tmp.{}.{}", path.display(), std::process::id(), hex::encode(nonce)));
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600) // O_CREAT|O_EXCL never follows a planted symlink
            .open(&tmp)?;
        f.write_all(content.as_bytes())?;
        drop(f);
        std::fs::rename(&tmp, path)
    };
    write().inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// `JSON.stringify(value, null, 2) + "\n"`, written atomically.
pub fn write_json(path: &Path, value: &Value) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut text = crate::jsjson::to_string_pretty(value);
    text.push('\n');
    atomic_write(path, &text)
}

fn read_object(path: &Path) -> Map<String, Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}
