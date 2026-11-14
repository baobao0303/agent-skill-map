//! The `sm enrich` command: refreshes extractors and enrichment actions for node(s).

use crate::cli::{Cli, CliError};
use crate::scope::Scope;
use clap::Args;
use rusqlite::params;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Args, Debug, Clone)]
pub struct EnrichArgs {
    pub node: Option<String>,
    #[arg(long)]
    pub stale: bool,
    #[arg(long)]
    pub no_plugins: bool,
}

pub fn run(cli: &Cli, scope: &Scope, args: &EnrichArgs) -> Result<u8, CliError> {
    let started = std::time::Instant::now();
    let db_path = cli.global.db.clone().unwrap_or_else(|| scope.db_path());

    if !db_path.exists() {
        if cli.global.json {
            println!(
                "{}",
                json!({
                    "ok": false,
                    "error": {
                        "code": "db-missing",
                        "message": "Project database not found. Run `sm init` before `sm enrich`."
                    }
                })
            );
            return Err(CliError::Raw("".into(), 5));
        }
        return Err(CliError::Raw(
            "✕  Project database not found. Run `sm init` before `sm enrich`.".into(),
            5,
        ));
    }

    if args.node.is_none() && !args.stale {
        return Err(CliError::Raw(
            "✕  Pass one or more <node.path> arguments, or --stale.\n   Examples: `sm enrich docs/guide.md` (explicit node), `sm enrich --stale` (every stale row).".into(),
            2,
        ));
    }

    let conn = rusqlite::Connection::open(&db_path).map_err(|e| CliError::Operational(e.to_string()))?;

    let target_node = args.node.clone().unwrap_or_default();
    if !target_node.is_empty() {
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM scan_nodes WHERE path = ?1",
                params![target_node],
                |_| Ok(true),
            )
            .unwrap_or(false);

        if !exists {
            if cli.global.json {
                println!(
                    "{}",
                    json!({
                        "ok": false,
                        "error": {
                            "code": "node-not-found",
                            "message": format!("Node not found: {target_node}")
                        }
                    })
                );
                return Err(CliError::Raw("".into(), 5));
            }
            return Err(CliError::Raw(
                format!("✕  Node not found: {target_node}\n   Run `sm list` to see every indexed node path."),
                5,
            ));
        }
    }

    let (cfg, _) = crate::config::load_config(scope);
    let allow_network = cfg.get("allowNetworkActions").and_then(Value::as_bool).unwrap_or(false);

    let github_enabled = crate::bump::extension_enabled(scope, "github", "enrichment", false);

    let mut refreshed_count = 0;

    if !target_node.is_empty() && allow_network && github_enabled {
        if let Some(res) = try_github_enrichment(scope, &conn, &target_node, &cfg)? {
            refreshed_count = res;
        }
    }

    scope.log_operation("enrich", "cli", "ok", &format!("refreshed={refreshed_count}"));
    let elapsed_ms = started.elapsed().as_millis() as u64;

    if cli.global.json {
        let nodes_breakdown = if target_node.is_empty() {
            vec![]
        } else {
            vec![json!({
                "path": target_node,
                "enrichments": refreshed_count
            })]
        };

        let report = json!({
            "ok": true,
            "kind": "enrich.report",
            "refreshed": refreshed_count,
            "nodes": nodes_breakdown,
            "elapsedMs": elapsed_ms,
        });
        println!("{}", serde_json::to_string(&report).unwrap());
        return Ok(0);
    }

    if refreshed_count > 0 {
        println!("✓  Refreshed {} enrichment on {}", refreshed_count, target_node);
    } else if !target_node.is_empty() {
        println!("Nothing to refresh for {target_node}.");
    } else {
        println!("No stale enrichments found.");
    }

    Ok(0)
}

fn try_github_enrichment(
    scope: &Scope,
    conn: &rusqlite::Connection,
    node_path: &str,
    cfg: &Value,
) -> Result<Option<usize>, CliError> {
    let sidecar_path = scope.root.join(format!("{node_path}.sm")).with_extension("sm");
    let actual_sidecar_path = if sidecar_path.is_file() {
        sidecar_path
    } else {
        let p = scope.root.join(node_path);
        let mut sm = p.clone();
        sm.set_extension("sm");
        sm
    };

    let sidecar_val = std::fs::read_to_string(&actual_sidecar_path)
        .ok()
        .and_then(|t| yaml_rust2::YamlLoader::load_from_str(&t).ok())
        .and_then(|docs| docs.into_iter().next());

    let (source, version) = match sidecar_val {
        Some(doc) => {
            let annotations = &doc["annotations"];
            let s = annotations["source"].as_str().unwrap_or("").to_string();
            let v = annotations["sourceVersion"].as_str().unwrap_or("").to_string();
            (s, v)
        }
        None => ("".to_string(), "".to_string()),
    };

    if source.is_empty() || version.is_empty() {
        return Ok(None);
    }

    let parsed_ref = parse_github_source(&source);
    let Some((owner, repo, remote_path)) = parsed_ref else {
        return Ok(None);
    };

    let api_base_val = crate::config::get_dot_path(cfg, "plugins.github.extensions.enrichment.settings.apiBaseUrl")
        .and_then(Value::as_str)
        .unwrap_or("https://api.github.com")
        .trim_end_matches('/');

    let raw_base_val = crate::config::get_dot_path(cfg, "plugins.github.extensions.enrichment.settings.rawBaseUrl")
        .and_then(Value::as_str)
        .unwrap_or("https://raw.githubusercontent.com")
        .trim_end_matches('/');

    let token_opt = crate::config::get_dot_path(cfg, "plugins.github.extensions.enrichment.settings.token")
        .and_then(Value::as_str);

    let started_at = crate::scan::now_ms();

    let (resolved_sha, method) = if version.len() == 40 && version.chars().all(|c| c.is_ascii_hexdigit()) {
        (version.clone(), "raw-sha")
    } else {
        let api_url = format!("{api_base_val}/repos/{owner}/{repo}/commits/{version}");
        let mut headers = vec![("Accept", "application/vnd.github+json")];
        let auth_hdr;
        if let Some(tok) = token_opt {
            auth_hdr = format!("Bearer {tok}");
            headers.push(("Authorization", &auth_hdr));
        }
        let (status, body) = http_get(&api_url, &headers).map_err(|e| CliError::Operational(e))?;
        if status != 200 {
            return Err(CliError::Operational(format!("GitHub API returned HTTP {status}")));
        }
        let body_json: Value = serde_json::from_str(&body).map_err(|e| CliError::Operational(e.to_string()))?;
        let sha = body_json.get("sha").and_then(Value::as_str).ok_or_else(|| {
            CliError::Operational("Commit JSON did not contain 'sha'".to_string())
        })?;
        (sha.to_string(), "api-ref")
    };

    let raw_url = format!("{raw_base_val}/{owner}/{repo}/{resolved_sha}/{remote_path}");
    let (status, remote_text) = http_get(&raw_url, &[]).map_err(|e| CliError::Operational(e))?;
    if status != 200 {
        return Err(CliError::Operational(format!("Raw fetch returned HTTP {status}")));
    }

    let local_file_path = scope.root.join(node_path);
    let local_text = std::fs::read_to_string(&local_file_path).map_err(|e| CliError::Operational(e.to_string()))?;

    let local_body = strip_frontmatter(&local_text);
    let remote_body = strip_frontmatter(&remote_text);

    let local_hash = hex::encode(Sha256::digest(local_body.as_bytes()));
    let remote_hash = hex::encode(Sha256::digest(remote_body.as_bytes()));
    let verified = local_hash == remote_hash;

    let finished_at = crate::scan::now_ms();
    let duration_ms = finished_at.saturating_sub(started_at);

    let report = json!({
        "verified": verified,
        "sourceUrl": source,
        "method": method,
        "resolvedSha": resolved_sha,
        "localBodyHash": local_hash,
        "remoteBodyHash": remote_hash,
    });

    let report_json = serde_json::to_string(&report).unwrap();
    let exec_id = crate::jobs::new_id("e");
    let node_ids_json = serde_json::to_string(&vec![node_path]).unwrap();

    conn.execute(
        "INSERT INTO state_executions (
            id, kind, extension_id, extension_version, node_ids_json, content_hash,
            status, failure_reason, exit_code, runner, started_at, finished_at,
            duration_ms, tokens_in, tokens_out, model, report_json, job_id
        ) VALUES (?1, 'action', 'github/enrichment', '0.1.0', ?2, NULL, 'completed', NULL, NULL, 'in-process', ?3, ?4, ?5, NULL, NULL, NULL, ?6, NULL)",
        params![exec_id, node_ids_json, started_at, finished_at, duration_ms, report_json],
    ).map_err(|e| CliError::Operational(e.to_string()))?;

    conn.execute(
        "INSERT INTO state_enrichments (node_id, provider_id, data_json, verified, fetched_at, stale_after)
         VALUES (?1, 'github/enrichment', ?2, ?3, ?4, NULL)
         ON CONFLICT(node_id, provider_id) DO UPDATE SET
            data_json = excluded.data_json,
            verified = excluded.verified,
            fetched_at = excluded.fetched_at",
        params![node_path, report_json, if verified { 1 } else { 0 }, finished_at],
    ).map_err(|e| CliError::Operational(e.to_string()))?;

    Ok(Some(1))
}

fn strip_frontmatter(text: &str) -> &str {
    if text.starts_with("---\n") || text.starts_with("---\r\n") {
        let rest = &text[3..];
        if let Some(pos) = rest.find("\n---\n") {
            return &rest[pos + 5..];
        }
        if let Some(pos) = rest.find("\r\n---\r\n") {
            return &rest[pos + 7..];
        }
    }
    text
}

fn parse_github_source(source: &str) -> Option<(String, String, String)> {
    let clean = source.split(['?', '#']).next().unwrap_or("");
    let without_scheme = clean.strip_prefix("https://").or_else(|| clean.strip_prefix("http://")).unwrap_or(clean);
    let parts: Vec<&str> = without_scheme.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() < 3 {
        return None;
    }
    let host = parts[0].trim_start_matches("www.");
    if host == "github.com" {
        let owner = parts[1].to_string();
        let repo = parts[2].to_string();
        let rest = &parts[3..];
        let path = if rest.len() >= 2 && rest[0] == "blob" {
            rest[2..].join("/")
        } else {
            rest.join("/")
        };
        Some((owner, repo, path))
    } else {
        None
    }
}

fn http_get(url_str: &str, headers: &[(&str, &str)]) -> Result<(u16, String), String> {
    let parsed = url::Url::parse(url_str).map_err(|e| e.to_string())?;
    let host = parsed.host_str().ok_or("no host")?;
    let port = parsed.port_or_known_default().unwrap_or(80);
    let path = if let Some(q) = parsed.query() {
        format!("{}?{}", parsed.path(), q)
    } else {
        parsed.path().to_string()
    };

    use std::io::{Read, Write};
    let addr = format!("{host}:{port}");
    let mut stream = std::net::TcpStream::connect(&addr).map_err(|e| format!("connect error: {e}"))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).ok();

    let mut req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: asm\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");

    stream.write_all(req.as_bytes()).map_err(|e| format!("write error: {e}"))?;

    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).map_err(|e| format!("read error: {e}"))?;

    let resp_str = String::from_utf8_lossy(&resp);
    let (head, raw_body) = resp_str.split_once("\r\n\r\n").unwrap_or(("", &resp_str));
    let status_line = head.lines().next().unwrap_or("");
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(500);

    let body = decode_chunked_if_needed(head, raw_body);
    Ok((status_code, body))
}

fn decode_chunked_if_needed(head: &str, body: &str) -> String {
    let is_chunked = head
        .lines()
        .any(|l| l.to_ascii_lowercase().starts_with("transfer-encoding:") && l.to_ascii_lowercase().contains("chunked"));

    if !is_chunked {
        return body.to_string();
    }

    let mut out = String::new();
    let mut rest = body;
    while !rest.is_empty() {
        let Some((line, next)) = rest.split_once("\r\n") else { break };
        let hex_str = line.trim();
        if hex_str.is_empty() {
            rest = next;
            continue;
        }
        let Ok(chunk_size) = usize::from_str_radix(hex_str, 16) else {
            return body.to_string();
        };
        if chunk_size == 0 {
            break;
        }
        if next.len() < chunk_size {
            out.push_str(next);
            break;
        }
        out.push_str(&next[..chunk_size]);
        let remaining = &next[chunk_size..];
        if let Some(after_crlf) = remaining.strip_prefix("\r\n") {
            rest = after_crlf;
        } else {
            rest = remaining;
        }
    }
    out
}
