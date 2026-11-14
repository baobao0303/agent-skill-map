use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

#[derive(clap::Args, Debug, Clone)]
pub struct ServeArgs {
    #[arg(long, default_value_t = 0)]
    pub port: u16,
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
    #[arg(long)]
    pub no_open: bool,
    #[arg(long)]
    pub no_watcher: bool,
    #[arg(long)]
    pub dev: bool,
}

struct CleanupGuard {
    path: PathBuf,
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn run(
    _cli: &crate::cli::Cli,
    scope: &crate::scope::Scope,
    args: &ServeArgs,
) -> Result<u8, crate::cli::CliError> {
    let listener = TcpListener::bind((args.host.as_str(), args.port)).map_err(|e| {
        crate::cli::CliError::Operational(format!("Failed to bind to {}:{}: {e}", args.host, args.port))
    })?;

    let bound_port = listener.local_addr().map_err(|e| {
        crate::cli::CliError::Operational(format!("Failed to get local address: {e}"))
    })?.port();

    let mut token_bytes = [0u8; 16];
    getrandom::fill(&mut token_bytes).map_err(|e| {
        crate::cli::CliError::Operational(format!("Failed to get OS randomness: {e}"))
    })?;
    let token = hex::encode(token_bytes);

    let _ = std::fs::create_dir_all(scope.dir());

    let pid = std::process::id();
    let tmp_path = scope.dir().join(format!("serve.json.tmp.{pid}"));
    let final_path = scope.dir().join("serve.json");

    let serve_info = json!({
        "schemaVersion": 1,
        "host": args.host,
        "port": bound_port,
        "pid": pid,
        "scopeRoot": scope.root.to_string_lossy(),
        "startedAt": crate::scope::iso_now(),
        "smVersion": "1.12.7",
        "token": token
    });

    std::fs::write(&tmp_path, serde_json::to_string_pretty(&serve_info).unwrap_or_default()).map_err(|e| {
        crate::cli::CliError::Operational(format!("Failed to write serve.json temporary file: {e}"))
    })?;
    std::fs::rename(&tmp_path, &final_path).map_err(|e| {
        crate::cli::CliError::Operational(format!("Failed to rename serve.json: {e}"))
    })?;

    let _cleanup_guard = CleanupGuard {
        path: final_path.clone(),
    };

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    let clean_path = final_path.clone();
    let _ = ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
        let _ = std::fs::remove_file(&clean_path);
    });

    eprintln!(
        "sm serve: listening on http://{}:{bound_port} (db=.skill-map/catalog.db)",
        args.host
    );
    eprintln!(
        "sm serve: visit http://{}:{bound_port}/ in your browser. Press Ctrl+C to stop.",
        args.host
    );

    listener.set_nonblocking(true).map_err(|e| {
        crate::cli::CliError::Operational(format!("Failed to set non-blocking on listener: {e}"))
    })?;

    let db_path = scope.db_path();
    let scope_root = scope.root.clone();
    let dev_cors = args.dev;
    let s_token = token.clone();

    while running.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let db_p = db_path.clone();
                let s_root = scope_root.clone();
                let tok = s_token.clone();
                std::thread::spawn(move || {
                    handle_connection(stream, &db_p, &s_root, &tok, dev_cors);
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => {
                eprintln!("sm serve: accept error: {e}");
                break;
            }
        }
    }

    Ok(0)
}

fn handle_connection(
    mut stream: TcpStream,
    db_path: &Path,
    scope_root: &Path,
    server_token: &str,
    dev_cors: bool,
) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(5)));

    let mut reader = BufReader::new(&mut stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() || request_line.is_empty() {
        return;
    }

    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return;
    }
    let method = parts[0];
    let uri = parts[1];

    // Read remaining headers until blank line
    let mut headers = HashMap::new();
    let mut header_line = String::new();
    while let Ok(n) = reader.read_line(&mut header_line) {
        if n == 0 || header_line == "\r\n" || header_line == "\n" {
            break;
        }
        if let Some((k, v)) = header_line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
        header_line.clear();
    }

    let stream = reader.into_inner();

    if method == "OPTIONS" {
        send_options_response(stream, dev_cors);
        return;
    }

    let path = uri.split('?').next().unwrap_or(uri);

    // Enforce token for mutating API operations
    if (method == "POST" || method == "PUT" || method == "DELETE") && path.starts_with("/api/") {
        let presented = headers.get("x-skill-map-token");
        let token_ok = match presented {
            Some(t) => t == server_token,
            None => false,
        };
        if !token_ok {
            send_json_response(
                stream,
                403,
                "Forbidden",
                &json!({
                    "ok": false,
                    "error": {
                        "code": "forbidden",
                        "message": "Invalid or missing x-skill-map-token header."
                    }
                }),
                dev_cors,
            );
            return;
        }
    }

    if method == "GET" {
        if path == "/api/health" {
            send_json_response(stream, 200, "OK", &json!({"ok": true, "status": "healthy"}), dev_cors);
        } else if path == "/api/nodes" {
            let resp = build_nodes_response(db_path);
            send_json_response(stream, 200, "OK", &resp, dev_cors);
        } else if let Some(rest) = path.strip_prefix("/api/nodes/") {
            if let Some(b64) = rest.strip_suffix("/prob-extensions") {
                handle_prob_extensions(stream, db_path, scope_root, b64, dev_cors);
            } else {
                handle_single_node(stream, db_path, scope_root, rest, dev_cors);
            }
        } else if path == "/api/links" {
            let resp = build_links_response(db_path);
            send_json_response(stream, 200, "OK", &resp, dev_cors);
        } else if path == "/api/issues" {
            let resp = build_issues_response(db_path);
            send_json_response(stream, 200, "OK", &resp, dev_cors);
        } else if path == "/api/config" || path == "/api/config/resolution" {
            let resp = build_config_response(scope_root);
            send_json_response(stream, 200, "OK", &resp, dev_cors);
        } else if path == "/api/plugins" {
            let resp = build_plugins_response(scope_root);
            send_json_response(stream, 200, "OK", &resp, dev_cors);
        } else if path == "/api/jobs" {
            let resp = build_jobs_response(db_path);
            send_json_response(stream, 200, "OK", &resp, dev_cors);
        } else if path == "/api/mcp/status" {
            send_json_response(
                stream,
                200,
                "OK",
                &json!({
                    "schemaVersion": "1",
                    "enabled": false,
                    "serverUrl": "http://127.0.0.1/mcp"
                }),
                dev_cors,
            );
        } else {
            send_json_response(stream, 404, "Not Found", &json!({"ok": false, "error": "not found"}), dev_cors);
        }
    } else {
        send_json_response(stream, 404, "Not Found", &json!({"ok": false, "error": "not found"}), dev_cors);
    }
}

fn handle_prob_extensions(stream: &mut TcpStream, db_path: &Path, scope_root: &Path, path_b64: &str, dev_cors: bool) {
    let node_path = match decode_base64(path_b64) {
        Some(p) => p,
        None => {
            send_json_response(
                stream,
                404,
                "Not Found",
                &json!({
                    "ok": false,
                    "error": {
                        "code": "node-not-found",
                        "message": format!("Invalid base64 path: {path_b64}")
                    }
                }),
                dev_cors,
            );
            return;
        }
    };

    let node_exists = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()
    .and_then(|conn| {
        conn.query_row(
            "SELECT 1 FROM scan_nodes WHERE path = ?1",
            [&node_path],
            |_| Ok(true),
        )
        .ok()
    })
    .unwrap_or(false);

    if !node_exists {
        send_json_response(
            stream,
            404,
            "Not Found",
            &json!({
                "ok": false,
                "error": {
                    "code": "node-not-found",
                    "message": format!("Node not found: {node_path}")
                }
            }),
            dev_cors,
        );
        return;
    }

    let scope = crate::scope::Scope::new(scope_root.to_path_buf());
    let (config, _) = crate::config::load_config(&scope);
    let skill_actions_enabled = config
        .get("skillActions")
        .and_then(|s| s.get("enabled"))
        .and_then(|e| e.as_bool())
        .unwrap_or(false);

    let mut skills = Vec::new();
    if skill_actions_enabled {
        let skills_dir = scope_root.join(".skill-map").join(".agents").join("skills");
        if let Ok(entries) = std::fs::read_dir(&skills_dir) {
            for entry in entries.flatten() {
                if let Ok(ft) = entry.file_type() {
                    if ft.is_dir() {
                        let dir_name = entry.file_name().to_string_lossy().to_string();
                        let skill_file = entry.path().join("SKILL.md");
                        let path = if skill_file.is_file() {
                            Some(skill_file)
                        } else {
                            let alt = entry.path().join("skill.md");
                            if alt.is_file() { Some(alt) } else { None }
                        };

                        if let Some(fpath) = path {
                            if let Ok(content) = std::fs::read_to_string(&fpath) {
                                let parsed = crate::frontmatter::parse(&content);
                                let name = parsed
                                    .value
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(&dir_name)
                                    .to_string();
                                let description = parsed
                                    .value
                                    .get("description")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let version = parsed
                                    .value
                                    .get("version")
                                    .and_then(|v| v.as_str())
                                    .or_else(|| {
                                        parsed
                                            .value
                                            .get("metadata")
                                            .and_then(|m| m.get("version"))
                                            .and_then(|v| v.as_str())
                                    })
                                    .unwrap_or("0.0.0")
                                    .to_string();

                                skills.push(json!({
                                    "id": format!("skill:{dir_name}"),
                                    "name": name,
                                    "description": description,
                                    "version": version,
                                    "state": "idle",
                                    "jobId": Value::Null,
                                    "lastJudged": Value::Null
                                }));
                            }
                        }
                    }
                }
            }
        }
        skills.sort_by(|a, b| {
            a["name"].as_str().unwrap_or("").cmp(b["name"].as_str().unwrap_or(""))
        });
    }

    let envelope = json!({
        "schemaVersion": "1",
        "kind": "node.prob-extensions",
        "item": {
            "finders": [],
            "standalone": [],
            "issueFixers": [],
            "skills": skills
        },
        "kindRegistry": kind_registry(),
        "providerRegistry": provider_registry(),
        "contributionsRegistry": contributions_registry()
    });

    send_json_response(stream, 200, "OK", &envelope, dev_cors);
}

fn handle_single_node(stream: &mut TcpStream, db_path: &Path, _scope_root: &Path, path_b64: &str, dev_cors: bool) {
    let node_path = match decode_base64(path_b64) {
        Some(p) => p,
        None => {
            send_json_response(
                stream,
                404,
                "Not Found",
                &json!({
                    "ok": false,
                    "error": {
                        "code": "node-not-found",
                        "message": format!("Invalid base64 path: {path_b64}")
                    }
                }),
                dev_cors,
            );
            return;
        }
    };

    let conn = match rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(_) => {
            send_json_response(stream, 404, "Not Found", &json!({"ok": false, "error": "database error"}), dev_cors);
            return;
        }
    };

    let row = conn.query_row(
        "SELECT path, kind, provider, body_hash, frontmatter_hash, bytes_frontmatter, bytes_body, bytes_total, \
         links_out_count, links_in_count, external_refs_count, frontmatter_json, modified_at_ms, \
         tokens_frontmatter, tokens_body, tokens_total, sidecar_present, sidecar_status, annotations_json, sidecar_root_json \
         FROM scan_nodes WHERE path = ?1",
        [&node_path],
        |r| {
            let path: String = r.get(0)?;
            let kind: String = r.get(1)?;
            let provider: String = r.get(2)?;
            let body_hash: String = r.get(3)?;
            let frontmatter_hash: String = r.get(4)?;
            let bytes_fm: i64 = r.get(5)?;
            let bytes_body: i64 = r.get(6)?;
            let bytes_total: i64 = r.get(7)?;
            let links_out: i64 = r.get(8)?;
            let links_in: i64 = r.get(9)?;
            let ext_refs: i64 = r.get(10)?;
            let fm_json: Option<String> = r.get(11)?;
            let modified_at: i64 = r.get(12)?;
            let tok_fm: Option<i64> = r.get(13)?;
            let tok_body: Option<i64> = r.get(14)?;
            let tok_total: Option<i64> = r.get(15)?;
            let sidecar_present: bool = r.get(16)?;
            let sidecar_status: Option<String> = r.get(17)?;

            let frontmatter: Value = fm_json
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(Value::Null);

            let tokens = match (tok_fm, tok_body, tok_total) {
                (Some(f), Some(b), Some(t)) => json!({
                    "frontmatter": f,
                    "body": b,
                    "total": t
                }),
                _ => Value::Null,
            };

            let sidecar = json!({
                "present": sidecar_present,
                "status": sidecar_status.unwrap_or_else(|| "clean".into())
            });

            Ok(json!({
                "path": path,
                "kind": kind,
                "provider": provider,
                "bodyHash": body_hash,
                "frontmatterHash": frontmatter_hash,
                "bytes": {
                    "frontmatter": bytes_fm,
                    "body": bytes_body,
                    "total": bytes_total
                },
                "linksOutCount": links_out,
                "linksInCount": links_in,
                "externalRefsCount": ext_refs,
                "frontmatter": frontmatter,
                "sidecar": sidecar,
                "tokens": tokens,
                "modifiedAtMs": modified_at,
                "isFavorite": false,
                "contributions": [],
                "tags": []
            }))
        },
    );

    let decorated_node = match row {
        Ok(n) => n,
        Err(_) => {
            send_json_response(
                stream,
                404,
                "Not Found",
                &json!({
                    "ok": false,
                    "error": {
                        "code": "node-not-found",
                        "message": format!("Node not found: {node_path}")
                    }
                }),
                dev_cors,
            );
            return;
        }
    };

    let mut incoming_links = Vec::new();
    if let Ok(mut stmt) = conn.prepare("SELECT source, target, kind, confidence, resolved_target FROM scan_links WHERE resolved_target = ?1 OR target = ?1") {
        if let Ok(rows) = stmt.query_map([&node_path], |r| {
            Ok(json!({
                "source": r.get::<_, String>(0)?,
                "target": r.get::<_, String>(1)?,
                "kind": r.get::<_, String>(2)?,
                "confidence": r.get::<_, f64>(3)?,
                "resolvedTarget": r.get::<_, Option<String>>(4)?
            }))
        }) {
            for row in rows.flatten() {
                incoming_links.push(row);
            }
        }
    }

    let mut outgoing_links = Vec::new();
    if let Ok(mut stmt) = conn.prepare("SELECT source, target, kind, confidence, resolved_target FROM scan_links WHERE source = ?1") {
        if let Ok(rows) = stmt.query_map([&node_path], |r| {
            Ok(json!({
                "source": r.get::<_, String>(0)?,
                "target": r.get::<_, String>(1)?,
                "kind": r.get::<_, String>(2)?,
                "confidence": r.get::<_, f64>(3)?,
                "resolvedTarget": r.get::<_, Option<String>>(4)?
            }))
        }) {
            for row in rows.flatten() {
                outgoing_links.push(row);
            }
        }
    }

    let mut issues_list = Vec::new();
    let pat = format!("%\"{node_path}\"%");
    if let Ok(mut stmt) = conn.prepare("SELECT analyzer_id, severity, node_ids_json, message, line FROM scan_issues WHERE node_ids_json LIKE ?1") {
        if let Ok(rows) = stmt.query_map([&pat], |r| {
            let analyzer_id: String = r.get(0)?;
            let severity: String = r.get(1)?;
            let node_ids_json: String = r.get(2)?;
            let message: String = r.get(3)?;
            let line: Option<i64> = r.get(4)?;
            let node_ids: Value = serde_json::from_str(&node_ids_json).unwrap_or(json!([node_path]));
            let mut obj = json!({
                "analyzerId": analyzer_id,
                "severity": severity,
                "nodeIds": node_ids,
                "message": message
            });
            if let Some(l) = line {
                obj.as_object_mut().unwrap().insert("line".into(), json!(l));
            }
            Ok(obj)
        }) {
            for row in rows.flatten() {
                issues_list.push(row);
            }
        }
    }

    let resp = json!({
        "schemaVersion": "1",
        "kind": "node",
        "item": decorated_node,
        "links": {
            "incoming": incoming_links,
            "outgoing": outgoing_links
        },
        "issues": issues_list,
        "kindRegistry": kind_registry(),
        "providerRegistry": provider_registry(),
        "contributionsRegistry": contributions_registry()
    });

    send_json_response(stream, 200, "OK", &resp, dev_cors);
}

fn build_links_response(db_path: &Path) -> Value {
    let conn = match rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(_) => {
            return json!({
                "schemaVersion": "1",
                "kind": "links",
                "items": [],
                "filters": {
                    "source": Value::Null,
                    "target": Value::Null,
                    "kind": Value::Null
                },
                "counts": {
                    "total": 0,
                    "returned": 0,
                    "page": { "offset": 0, "limit": 100 }
                },
                "kindRegistry": kind_registry(),
                "providerRegistry": provider_registry(),
                "contributionsRegistry": contributions_registry()
            });
        }
    };

    let mut items = Vec::new();
    if let Ok(mut stmt) = conn.prepare("SELECT source, target, kind, confidence, resolved_target FROM scan_links") {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok(json!({
                "source": r.get::<_, String>(0)?,
                "target": r.get::<_, String>(1)?,
                "kind": r.get::<_, String>(2)?,
                "confidence": r.get::<_, f64>(3)?,
                "resolvedTarget": r.get::<_, Option<String>>(4)?
            }))
        }) {
            for row in rows.flatten() {
                items.push(row);
            }
        }
    }

    let total = items.len();
    json!({
        "schemaVersion": "1",
        "kind": "links",
        "items": items,
        "filters": {
            "source": Value::Null,
            "target": Value::Null,
            "kind": Value::Null
        },
        "counts": {
            "total": total,
            "returned": total,
            "page": { "offset": 0, "limit": 100 }
        },
        "kindRegistry": kind_registry(),
        "providerRegistry": provider_registry(),
        "contributionsRegistry": contributions_registry()
    })
}

fn build_issues_response(db_path: &Path) -> Value {
    let conn = match rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(_) => {
            return json!({
                "schemaVersion": "1",
                "kind": "issues",
                "items": [],
                "filters": {
                    "severity": Value::Null,
                    "analyzer": Value::Null
                },
                "counts": {
                    "total": 0,
                    "returned": 0,
                    "page": { "offset": 0, "limit": 100 }
                },
                "kindRegistry": kind_registry(),
                "providerRegistry": provider_registry(),
                "contributionsRegistry": contributions_registry()
            });
        }
    };

    let mut items = Vec::new();
    if let Ok(mut stmt) = conn.prepare("SELECT analyzer_id, severity, node_ids_json, message, line FROM scan_issues") {
        if let Ok(rows) = stmt.query_map([], |r| {
            let analyzer_id: String = r.get(0)?;
            let severity: String = r.get(1)?;
            let node_ids_json: String = r.get(2)?;
            let message: String = r.get(3)?;
            let line: Option<i64> = r.get(4)?;
            let node_ids: Value = serde_json::from_str(&node_ids_json).unwrap_or(json!([]));
            let mut obj = json!({
                "analyzerId": analyzer_id,
                "severity": severity,
                "nodeIds": node_ids,
                "message": message
            });
            if let Some(l) = line {
                obj.as_object_mut().unwrap().insert("line".into(), json!(l));
            }
            Ok(obj)
        }) {
            for row in rows.flatten() {
                items.push(row);
            }
        }
    }

    let total = items.len();
    json!({
        "schemaVersion": "1",
        "kind": "issues",
        "items": items,
        "filters": {
            "severity": Value::Null,
            "analyzer": Value::Null
        },
        "counts": {
            "total": total,
            "returned": total,
            "page": { "offset": 0, "limit": 100 }
        },
        "kindRegistry": kind_registry(),
        "providerRegistry": provider_registry(),
        "contributionsRegistry": contributions_registry()
    })
}

fn build_config_response(scope_root: &Path) -> Value {
    let scope = crate::scope::Scope::new(scope_root.to_path_buf());
    let (config, _) = crate::config::load_config(&scope);
    json!({
        "schemaVersion": "1",
        "kind": "config",
        "value": config,
        "kindRegistry": kind_registry(),
        "providerRegistry": provider_registry(),
        "contributionsRegistry": contributions_registry()
    })
}

fn build_plugins_response(scope_root: &Path) -> Value {
    let scope = crate::scope::Scope::new(scope_root.to_path_buf());
    let (built_ins, drop_ins) = crate::plugins::manager::discover_plugins(&scope);
    let mut items = Vec::new();
    for p in built_ins.iter().chain(drop_ins.iter()) {
        let source = if p.is_user { "user" } else { "built-in" };
        items.push(json!({
            "id": p.id,
            "enabled": p.enabled,
            "status": p.status,
            "source": source,
            "extensions": p.extensions
        }));
    }
    let total = items.len();
    json!({
        "schemaVersion": "1",
        "kind": "plugins",
        "items": items,
        "filters": {
            "source": Value::Null,
            "enabled": Value::Null
        },
        "counts": {
            "total": total,
            "returned": total,
            "page": { "offset": 0, "limit": 100 }
        },
        "kindRegistry": kind_registry(),
        "providerRegistry": provider_registry(),
        "contributionsRegistry": contributions_registry()
    })
}

fn build_jobs_response(db_path: &Path) -> Value {
    let conn = match rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(_) => {
            return json!({
                "schemaVersion": "1",
                "kind": "jobs",
                "items": [],
                "filters": {
                    "status": Value::Null,
                    "nodeId": Value::Null
                },
                "counts": {
                    "total": 0,
                    "returned": 0,
                    "page": { "offset": 0, "limit": 100 }
                }
            });
        }
    };

    let mut items = Vec::new();
    if let Ok(mut stmt) = conn.prepare("SELECT id, extension_id, extension_version, extension_kind, node_id, content_hash, priority, status, failure_reason, runner, created_at, claimed_at, finished_at, submitted_by FROM state_jobs ORDER BY created_at DESC") {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "extensionId": r.get::<_, String>(1)?,
                "extensionVersion": r.get::<_, String>(2)?,
                "extensionKind": r.get::<_, String>(3)?,
                "nodeId": r.get::<_, String>(4)?,
                "contentHash": r.get::<_, String>(5)?,
                "priority": r.get::<_, i64>(6)?,
                "status": r.get::<_, String>(7)?,
                "failureReason": r.get::<_, Option<String>>(8)?,
                "runner": r.get::<_, String>(9)?,
                "createdAt": r.get::<_, i64>(10)?,
                "claimedAt": r.get::<_, Option<i64>>(11)?,
                "finishedAt": r.get::<_, Option<i64>>(12)?,
                "submittedBy": r.get::<_, Option<String>>(13)?
            }))
        }) {
            for row in rows.flatten() {
                items.push(row);
            }
        }
    }

    let total = items.len();
    json!({
        "schemaVersion": "1",
        "kind": "jobs",
        "items": items,
        "filters": {
            "status": Value::Null,
            "nodeId": Value::Null
        },
        "counts": {
            "total": total,
            "returned": total,
            "page": { "offset": 0, "limit": 100 }
        }
    })
}

fn build_nodes_response(db_path: &Path) -> Value {
    let conn = match rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(_) => {
            return json!({
                "schemaVersion": "1",
                "kind": "nodes",
                "items": [],
                "filters": {
                    "kind": Value::Null,
                    "hasIssues": Value::Null,
                    "path": Value::Null
                },
                "counts": {
                    "total": 0,
                    "returned": 0,
                    "page": {
                        "offset": 0,
                        "limit": 100
                    }
                },
                "kindRegistry": kind_registry(),
                "providerRegistry": provider_registry(),
                "contributionsRegistry": contributions_registry()
            });
        }
    };

    let mut favorites: HashSet<String> = HashSet::new();
    if let Ok(mut stmt) = conn.prepare("SELECT node_path FROM state_node_favorites") {
        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
            for row in rows.flatten() {
                favorites.insert(row);
            }
        }
    }

    let mut contributions_by_path: HashMap<String, Vec<Value>> = HashMap::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT plugin_id, extension_id, node_path, contribution_id, slot, payload_json, emitted_at \
         FROM scan_contributions ORDER BY plugin_id, extension_id, contribution_id",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| {
            let plugin_id: String = r.get(0)?;
            let extension_id: String = r.get(1)?;
            let node_path: String = r.get(2)?;
            let contribution_id: String = r.get(3)?;
            let slot: String = r.get(4)?;
            let payload_json: String = r.get(5)?;
            let emitted_at: i64 = r.get(6)?;
            let payload: Value = serde_json::from_str(&payload_json).unwrap_or(Value::Null);
            Ok((
                node_path.clone(),
                json!({
                    "pluginId": plugin_id,
                    "extensionId": extension_id,
                    "nodePath": node_path,
                    "contributionId": contribution_id,
                    "slot": slot,
                    "payload": payload,
                    "emittedAt": emitted_at
                }),
            ))
        }) {
            for row in rows.flatten() {
                contributions_by_path.entry(row.0).or_default().push(row.1);
            }
        }
    }

    let mut tags_by_path: HashMap<String, Vec<String>> = HashMap::new();
    if let Ok(mut stmt) = conn.prepare("SELECT node_path, tag FROM scan_node_tags ORDER BY tag ASC") {
        if let Ok(rows) = stmt.query_map([], |r| {
            let node_path: String = r.get(0)?;
            let tag: String = r.get(1)?;
            Ok((node_path, tag))
        }) {
            for row in rows.flatten() {
                tags_by_path.entry(row.0).or_default().push(row.1);
            }
        }
    }

    let mut items = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT path, kind, provider, body_hash, frontmatter_hash, bytes_frontmatter, bytes_body, bytes_total, \
         links_out_count, links_in_count, external_refs_count, frontmatter_json, modified_at_ms, \
         tokens_frontmatter, tokens_body, tokens_total, sidecar_present, sidecar_status, annotations_json, sidecar_root_json \
         FROM scan_nodes ORDER BY rowid",
    ) {
        let rows = stmt.query_map([], |r| {
            let path: String = r.get(0)?;
            let kind: String = r.get(1)?;
            let provider: String = r.get(2)?;
            let body_hash: String = r.get(3)?;
            let frontmatter_hash: String = r.get(4)?;
            let bytes_fm: u64 = r.get(5)?;
            let bytes_body: u64 = r.get(6)?;
            let bytes_total: u64 = r.get(7)?;
            let links_out: u64 = r.get(8)?;
            let links_in: u64 = r.get(9)?;
            let ext_refs: u64 = r.get(10)?;
            let fm_json: Option<String> = r.get(11)?;
            let modified_at: Option<i64> = r.get(12)?;
            let tokens_fm: Option<u64> = r.get(13)?;
            let tokens_body: Option<u64> = r.get(14)?;
            let tokens_total: Option<u64> = r.get(15)?;
            let sidecar_present: bool = r.get(16)?;
            let sidecar_status: Option<String> = r.get(17)?;
            let annotations_json: Option<String> = r.get(18)?;
            let sidecar_root_json: Option<String> = r.get(19)?;

            let frontmatter = fm_json.and_then(|s| serde_json::from_str::<Value>(&s).ok());
            let sidecar = if sidecar_present {
                json!({
                    "present": true,
                    "status": sidecar_status,
                    "annotations": annotations_json.and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or(Value::Null),
                    "root": sidecar_root_json.and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or(Value::Null),
                })
            } else {
                json!({
                    "present": false,
                    "status": Value::Null,
                    "annotations": Value::Null,
                    "root": Value::Null,
                })
            };
            let tokens = tokens_total.map(|tot| {
                json!({
                    "frontmatter": tokens_fm.unwrap_or(0),
                    "body": tokens_body.unwrap_or(0),
                    "total": tot
                })
            });

            Ok((
                path,
                kind,
                provider,
                body_hash,
                frontmatter_hash,
                bytes_fm,
                bytes_body,
                bytes_total,
                links_out,
                links_in,
                ext_refs,
                frontmatter,
                modified_at,
                tokens,
                sidecar,
            ))
        });

        if let Ok(rows) = rows {
            for r in rows.flatten() {
                let (
                    path,
                    kind,
                    provider,
                    body_hash,
                    frontmatter_hash,
                    bytes_fm,
                    bytes_body,
                    bytes_total,
                    links_out,
                    links_in,
                    ext_refs,
                    frontmatter,
                    modified_at,
                    tokens,
                    sidecar,
                ) = r;

                let is_favorite = favorites.contains(&path);
                let contribs = contributions_by_path.remove(&path).unwrap_or_default();
                let tags = tags_by_path.remove(&path).unwrap_or_default();

                items.push(json!({
                    "path": path,
                    "kind": kind,
                    "provider": provider,
                    "bodyHash": body_hash,
                    "frontmatterHash": frontmatter_hash,
                    "bytes": {
                        "frontmatter": bytes_fm,
                        "body": bytes_body,
                        "total": bytes_total
                    },
                    "linksOutCount": links_out,
                    "linksInCount": links_in,
                    "externalRefsCount": ext_refs,
                    "frontmatter": frontmatter,
                    "sidecar": sidecar,
                    "tokens": tokens,
                    "modifiedAtMs": modified_at,
                    "isFavorite": is_favorite,
                    "contributions": contribs,
                    "tags": tags
                }));
            }
        }
    }

    let total = items.len();
    json!({
        "schemaVersion": "1",
        "kind": "nodes",
        "items": items,
        "filters": {
            "kind": Value::Null,
            "hasIssues": Value::Null,
            "path": Value::Null
        },
        "counts": {
            "total": total,
            "returned": total,
            "page": {
                "offset": 0,
                "limit": 100
            }
        },
        "kindRegistry": kind_registry(),
        "providerRegistry": provider_registry(),
        "contributionsRegistry": contributions_registry()
    })
}

fn send_json_response(mut stream: &TcpStream, status_code: u16, status_text: &str, body: &Value, dev_cors: bool) {
    let body_str = serde_json::to_string(body).unwrap_or_default();
    let cors_header = if dev_cors {
        "Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, POST, PUT, DELETE, OPTIONS\r\n\
         Access-Control-Allow-Headers: content-type, authorization, x-skill-map-token\r\n"
    } else {
        ""
    };
    let response = format!(
        "HTTP/1.1 {status_code} {status_text}\r\n\
         Content-Type: application/json; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         {cors_header}\
         Connection: close\r\n\
         \r\n\
         {body_str}",
        body_str.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn send_options_response(mut stream: &TcpStream, dev_cors: bool) {
    let cors_header = if dev_cors {
        "Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, POST, PUT, DELETE, OPTIONS\r\n\
         Access-Control-Allow-Headers: content-type, authorization, x-skill-map-token\r\n"
    } else {
        ""
    };
    let response = format!(
        "HTTP/1.1 204 No Content\r\n\
         {cors_header}\
         Connection: close\r\n\
         \r\n"
    );
    let _ = stream.write_all(response.as_bytes());
}

fn decode_base64(input: &str) -> Option<String> {
    let mut bytes = Vec::new();
    let mut buf: u32 = 0;
    let mut bits = 0;
    for &b in input.as_bytes() {
        if b == b'=' {
            break;
        }
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b' ' | b'\r' | b'\n' | b'\t' => continue,
            _ => return None,
        };
        buf = (buf << 6) | (val as u32);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    String::from_utf8(bytes).ok()
}

fn kind_registry() -> Value {
    json!({
        "agent": {
            "primaryProviderId": "claude",
            "providers": {
                "claude": {
                    "label": "Agents",
                    "color": "#3b82f6",
                    "colorDark": "#60a5fa",
                    "icon": { "kind": "pi", "id": "pi-user" }
                },
                "codex": {
                    "label": "Codex agents",
                    "color": "#22c55e",
                    "colorDark": "#4ade80",
                    "icon": { "kind": "pi", "id": "pi-bolt" }
                },
                "opencode": {
                    "label": "Agents",
                    "color": "#3b82f6",
                    "colorDark": "#60a5fa",
                    "icon": { "kind": "pi", "id": "pi-user" }
                }
            }
        },
        "command": {
            "primaryProviderId": "claude",
            "providers": {
                "claude": {
                    "label": "Commands",
                    "color": "#f59e0b",
                    "colorDark": "#fbbf24",
                    "icon": { "kind": "svg", "path": "M4 17 L10 11 L4 5 M12 19 L20 19" }
                },
                "opencode": {
                    "label": "Commands",
                    "color": "#f59e0b",
                    "colorDark": "#fbbf24",
                    "icon": { "kind": "svg", "path": "M4 17 L10 11 L4 5 M12 19 L20 19" }
                }
            }
        },
        "skill": {
            "primaryProviderId": "claude",
            "providers": {
                "claude": {
                    "label": "Skills",
                    "color": "#10b981",
                    "colorDark": "#34d399",
                    "icon": { "kind": "pi", "id": "pi-bolt" }
                },
                "antigravity": {
                    "label": "Skills",
                    "color": "#10b981",
                    "colorDark": "#34d399",
                    "icon": { "kind": "pi", "id": "pi-bolt" }
                },
                "codex": {
                    "label": "Skills",
                    "color": "#10b981",
                    "colorDark": "#34d399",
                    "icon": { "kind": "pi", "id": "pi-bolt" }
                },
                "opencode": {
                    "label": "Skills",
                    "color": "#10b981",
                    "colorDark": "#34d399",
                    "icon": { "kind": "pi", "id": "pi-bolt" }
                },
                "agent-skills": {
                    "label": "Skills",
                    "color": "#10b981",
                    "colorDark": "#34d399",
                    "icon": { "kind": "pi", "id": "pi-bolt" }
                }
            }
        },
        "mcp": {
            "primaryProviderId": "claude",
            "providers": {
                "claude": {
                    "label": "MCP servers",
                    "color": "#8b5cf6",
                    "colorDark": "#a78bfa",
                    "icon": { "kind": "pi", "id": "pi-server" }
                }
            }
        },
        "workflow": {
            "primaryProviderId": "antigravity",
            "providers": {
                "antigravity": {
                    "label": "Workflows",
                    "color": "#f59e0b",
                    "colorDark": "#fbbf24",
                    "icon": { "kind": "pi", "id": "pi-sitemap" }
                }
            }
        },
        "markdown": {
            "primaryProviderId": "markdown",
            "providers": {
                "markdown": {
                    "label": "Markdown",
                    "color": "#5b908c",
                    "colorDark": "#9bbcb8",
                    "icon": {
                        "kind": "svg",
                        "path": "M14 2 H6 a2 2 0 0 0 -2 2 V20 a2 2 0 0 0 2 2 H18 a2 2 0 0 0 2 -2 V8 L14 2 M14 2 V8 H20 M16 13 H8 M16 17 H8 M10 9 H8"
                    }
                }
            }
        }
    })
}

fn provider_registry() -> Value {
    json!({
        "claude": {
            "label": "Anthropic's Claude",
            "color": "#cc785c",
            "isLens": true,
            "colorDark": "#e89270",
            "invocationSigil": "/",
            "mcpRegister": {
                "kind": "command",
                "command": {
                    "template": "claude mcp add --transport http --scope local skill-map {{url}}"
                }
            }
        },
        "antigravity": {
            "label": "Google's Antigravity",
            "color": "#7c3aed",
            "isLens": true,
            "colorDark": "#a78bfa",
            "invocationSigil": "/",
            "mcpRegister": {
                "kind": "config",
                "config": {
                    "target": "~/.gemini/config/mcp_config.json",
                    "document": {
                        "mcpServers": {
                            "skill-map": {
                                "serverUrl": "{{url}}"
                            }
                        }
                    }
                }
            }
        },
        "codex": {
            "label": "OpenAI's Codex",
            "color": "#22c55e",
            "isLens": true,
            "colorDark": "#4ade80",
            "invocationSigil": "$",
            "bodyField": "developer_instructions",
            "mcpRegister": {
                "kind": "command",
                "command": {
                    "template": "codex mcp add skill-map --url {{url}}"
                }
            }
        },
        "opencode": {
            "label": "OpenCode",
            "color": "#0891b2",
            "isLens": true,
            "colorDark": "#22d3ee",
            "invocationSigil": "/",
            "mcpRegister": {
                "kind": "config",
                "config": {
                    "target": "~/.config/opencode/opencode.json",
                    "document": {
                        "$schema": "https://opencode.ai/config.json",
                        "mcp": {
                            "skill-map": {
                                "type": "remote",
                                "url": "{{url}}",
                                "enabled": true
                            }
                        }
                    }
                }
            }
        },
        "agent-skills": {
            "label": "Standard: Agent skills",
            "color": "#64748b",
            "isLens": true,
            "colorDark": "#94a3b8"
        },
        "markdown": {
            "label": "Markdown",
            "color": "#9ca3af",
            "isLens": false,
            "colorDark": "#6b7280",
            "hideChip": true
        }
    })
}

fn contributions_registry() -> Value {
    json!({
        "claude/tools-counter/count": {
            "pluginId": "claude",
            "extensionId": "tools-counter",
            "contributionId": "count",
            "slot": "card.footer.left",
            "emitWhenEmpty": false,
            "label": "tools",
            "icon": "pi-wrench",
            "priority": 40
        },
        "core/external-url-counter/count": {
            "pluginId": "core",
            "extensionId": "external-url-counter",
            "contributionId": "count",
            "slot": "card.footer.left",
            "emitWhenEmpty": false,
            "label": "urls",
            "icon": "pi-link",
            "priority": 30
        },
        "core/annotation-stale/staleIcon": {
            "pluginId": "core",
            "extensionId": "annotation-stale",
            "contributionId": "staleIcon",
            "slot": "card.footer.right",
            "emitWhenEmpty": true,
            "icon": "pi-clock",
            "priority": 20
        },
        "core/annotation-stale/staleBadge": {
            "pluginId": "core",
            "extensionId": "annotation-stale",
            "contributionId": "staleBadge",
            "slot": "inspector.header.badge",
            "emitWhenEmpty": false,
            "priority": 20
        },
        "core/issue-counter/warnCount": {
            "pluginId": "core",
            "extensionId": "issue-counter",
            "contributionId": "warnCount",
            "slot": "card.footer.right",
            "emitWhenEmpty": false,
            "icon": "pi-exclamation-triangle",
            "priority": 30
        },
        "core/issue-counter/errorCount": {
            "pluginId": "core",
            "extensionId": "issue-counter",
            "contributionId": "errorCount",
            "slot": "card.footer.right",
            "emitWhenEmpty": false,
            "icon": "pi-times-circle",
            "priority": 40
        },
        "core/link-counter/linksIn": {
            "pluginId": "core",
            "extensionId": "link-counter",
            "contributionId": "linksIn",
            "slot": "card.footer.left",
            "emitWhenEmpty": false,
            "label": "incoming links",
            "icon": "pi-download",
            "priority": 10
        },
        "core/link-counter/linksOut": {
            "pluginId": "core",
            "extensionId": "link-counter",
            "contributionId": "linksOut",
            "slot": "card.footer.left",
            "emitWhenEmpty": false,
            "label": "outgoing links",
            "icon": "pi-upload",
            "priority": 20
        },
        "core/node-stability/experimental": {
            "pluginId": "core",
            "extensionId": "node-stability",
            "contributionId": "experimental",
            "slot": "card.footer.right",
            "emitWhenEmpty": true,
            "label": "experimental",
            "icon": "fa-solid fa-flask",
            "priority": 10
        },
        "core/node-stability/deprecated": {
            "pluginId": "core",
            "extensionId": "node-stability",
            "contributionId": "deprecated",
            "slot": "card.footer.right",
            "emitWhenEmpty": true,
            "label": "deprecated",
            "icon": "pi-ban",
            "priority": 10
        },
        "core/ai-summarizer-action/summarySurface": {
            "pluginId": "core",
            "extensionId": "ai-summarizer-action",
            "contributionId": "summarySurface",
            "slot": "inspector.surface.summary",
            "emitWhenEmpty": false,
            "priority": 10
        },
        "core/ai-tagger-action/autoTagSurface": {
            "pluginId": "core",
            "extensionId": "ai-tagger-action",
            "contributionId": "autoTagSurface",
            "slot": "inspector.surface.auto-tag",
            "emitWhenEmpty": false,
            "priority": 10
        },
        "core/node-bump/bumpButton": {
            "pluginId": "core",
            "extensionId": "node-bump",
            "contributionId": "bumpButton",
            "slot": "inspector.surface.version",
            "emitWhenEmpty": false,
            "priority": 10
        },
        "core/node-set-stability/setStabilityButton": {
            "pluginId": "core",
            "extensionId": "node-set-stability",
            "contributionId": "setStabilityButton",
            "slot": "inspector.surface.stability",
            "emitWhenEmpty": false,
            "priority": 15
        },
        "core/node-set-tags/editTagsButton": {
            "pluginId": "core",
            "extensionId": "node-set-tags",
            "contributionId": "editTagsButton",
            "slot": "inspector.surface.tags",
            "emitWhenEmpty": false,
            "priority": 14
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_base64() {
        assert_eq!(decode_base64("bm90ZXMubWQ").as_deref(), Some("notes.md"));
        assert_eq!(decode_base64("bm90ZXMubWQ=").as_deref(), Some("notes.md"));
        assert_eq!(decode_base64("aGVsbG8gd29ybGQ=").as_deref(), Some("hello world"));
        assert_eq!(decode_base64("").as_deref(), Some(""));
    }

    #[test]
    fn test_registries_structure() {
        let kr = kind_registry();
        assert_eq!(kr["agent"]["primaryProviderId"], "claude");
        assert_eq!(kr["skill"]["primaryProviderId"], "claude");
        assert_eq!(kr["markdown"]["primaryProviderId"], "markdown");

        let pr = provider_registry();
        assert_eq!(pr["claude"]["label"], "Anthropic's Claude");
        assert_eq!(pr["antigravity"]["label"], "Google's Antigravity");
        assert_eq!(pr["markdown"]["isLens"], false);

        let cr = contributions_registry();
        assert!(cr.get("claude/tools-counter/count").is_some());
        assert!(cr.get("core/link-counter/linksIn").is_some());
    }
}

