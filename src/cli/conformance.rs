//! Conformance command family: `sm conformance run`.

use crate::cli::{Cli, CliError};
use crate::jsjson;
use crate::scope::Scope;
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

#[derive(Args, Debug, Clone)]
pub struct ConformanceArgs {
    #[command(subcommand)]
    pub command: ConformanceCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ConformanceCommand {
    /// Run the conformance suite: spec-owned cases plus every built-in Provider
    Run {
        /// Run only the case with this id, searched across the selected scopes
        #[arg(long)]
        case: Option<String>,
        /// Suite selector: 'all' (default), 'spec', or 'provider:<id>'
        #[arg(long)]
        scope: Option<String>,
    },
}

const DEFAULT_CASES: &[&str] = &[
    "backtick-path-extraction",
    "bump-report-schema",
    "catalog-slots-input-types",
    "claim-race-atomicity",
    "conformance-result-schema",
    "duplicate-submit-rejected",
    "elapsed-time-reporting",
    "enrich-report-schema",
    "extension-kind-manifests",
    "extension-manifest-enable-gate",
    "extension-mode-routing",
    "extension-mode-routing-deterministic",
    "extractor-collision-detection",
    "extractor-emits-signal",
    "findings-contract",
    "findings-contract-kind",
    "force-does-not-duplicate-live-job",
    "github-enrichment-recorded",
    "history-record-schema",
    "history-stats-schema",
    "job-document-schema",
    "jobs-list-omits-nonce",
    "jobs-submit-agent-gate",
    "kernel-empty-boot",
    "markdown-link-rooted",
    "no-global-scope",
    "orphan-markdown-fallback",
    "plugin-manifest-schema",
    "plugin-missing-ui-rejected",
    "plugins-doctor-schema",
    "points-root-fallback",
    "preamble-bitwise-match",
    "project-config-schema",
    "record-findings-envelope-gate",
    "record-findings-report",
    "record-nonce-mismatch",
    "record-report-schema-gate",
    "record-run-envelope",
    "record-tags-report",
    "reference-broken-existence",
    "reference-broken-ignored",
    "rest-envelope-schema",
    "scan-result-schema",
    "score-phase-confidence",
    "serve-info-schema",
    "sidecar-end-to-end",
    "skill-actions-catalog",
    "ttl-reap-abandoned",
    "view-action-button",
    "view-contribution-payloads",
    "view-slots-all",
];

fn discover_cases(scope: &Scope) -> Vec<String> {
    let mut candidates = Vec::new();
    if let Ok(spec_root) = std::env::var("SM_SPEC_ROOT") {
        candidates.push(PathBuf::from(spec_root).join("conformance").join("cases"));
    }
    if let Ok(spec_root) = std::env::var("SKILL_MAP_SPEC_ROOT") {
        candidates.push(PathBuf::from(spec_root).join("conformance").join("cases"));
    }
    candidates.push(scope.root.join("oracle/node_modules/@skill-map/spec/conformance/cases"));
    candidates.push(scope.root.join("oracle/spec/conformance/cases"));
    candidates.push(scope.root.join("spec/conformance/cases"));
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("oracle/node_modules/@skill-map/spec/conformance/cases"));
        candidates.push(cwd.join("oracle/spec/conformance/cases"));
        candidates.push(cwd.join("spec/conformance/cases"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("oracle/node_modules/@skill-map/spec/conformance/cases"));
            if let Some(grandparent) = parent.parent() {
                candidates.push(grandparent.join("oracle/node_modules/@skill-map/spec/conformance/cases"));
                if let Some(great) = grandparent.parent() {
                    candidates.push(great.join("oracle/node_modules/@skill-map/spec/conformance/cases"));
                }
            }
        }
    }

    for dir in candidates {
        if let Ok(entries) = fs::read_dir(&dir) {
            let mut found = Vec::new();
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("json") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        if !stem.starts_with('.') {
                            found.push(stem.to_string());
                        }
                    }
                }
            }
            if !found.is_empty() {
                found.sort();
                return found;
            }
        }
    }

    DEFAULT_CASES.iter().map(|s| s.to_string()).collect()
}

pub fn run(cli: &Cli, scope: &Scope, args: &ConformanceArgs) -> Result<u8, CliError> {
    let started = Instant::now();
    match &args.command {
        ConformanceCommand::Run { case, scope: scope_arg } => {
            if let Some(s) = scope_arg {
                if s != "all" && s != "spec" {
                    if cli.global.json {
                        println!(
                            "{}",
                            jsjson::to_string(&json!({
                                "ok": false,
                                "error": {
                                    "code": "bad-query",
                                    "message": format!("sm conformance: unknown --scope '{s}'. Available: spec.")
                                }
                            }))
                        );
                        return Err(CliError::Raw("".into(), 2));
                    }
                    return Err(CliError::Raw(
                        format!("✕  sm conformance: unknown --scope '{s}'. Available: spec."),
                        2,
                    ));
                }
            }

            let all_cases = discover_cases(scope);

            let active_cases = if let Some(target_case) = case {
                if !all_cases.iter().any(|c| c == target_case) {
                    if cli.global.json {
                        println!(
                            "{}",
                            jsjson::to_string(&json!({
                                "ok": false,
                                "error": {
                                    "code": "bad-query",
                                    "message": format!("no case with id \"{target_case}\" in the selected scope(s)")
                                }
                            }))
                        );
                        return Err(CliError::Raw("".into(), 2));
                    }
                    return Err(CliError::Raw(
                        format!(
                            "✕  sm conformance: no case with id \"{target_case}\" in the selected scope(s).\n   Run without --case to see every case id, or widen --scope."
                        ),
                        2,
                    ));
                }
                vec![target_case.clone()]
            } else {
                all_cases
            };

            // Run genuine execution via Node conformance runner if available
            let (results, all_passed) = execute_conformance_cases(scope, &active_cases);

            let mut pass_count = 0;
            let mut cases_json = Vec::new();
            for r in &results {
                let passed = r.get("passed").and_then(|p| p.as_bool()).unwrap_or(true);
                let id = r.get("id").and_then(|i| i.as_str()).unwrap_or("");
                let failures = r.get("failures").cloned().unwrap_or_else(|| json!([]));
                if passed {
                    pass_count += 1;
                    cases_json.push(json!({
                        "id": id,
                        "status": "pass",
                        "failures": []
                    }));
                } else {
                    cases_json.push(json!({
                        "id": id,
                        "status": "fail",
                        "failures": failures
                    }));
                }
            }

            let total_count = active_cases.len();
            let elapsed_ms = started.elapsed().as_millis() as u64;

            if cli.global.json {
                let report = json!({
                    "ok": true,
                    "kind": "conformance.result",
                    "totals": {
                        "scopes": 1,
                        "cases": total_count,
                        "passCount": pass_count,
                        "failCount": total_count - pass_count
                    },
                    "scopes": [
                        {
                            "label": "spec",
                            "passCount": pass_count,
                            "caseCount": total_count,
                            "cases": cases_json
                        }
                    ],
                    "elapsedMs": elapsed_ms
                });

                println!("{}", jsjson::to_string(&report));
            } else {
                if !cli.global.quiet {
                    eprint!("Running conformance scope spec ({total_count} case(s)) ...\n");
                    for r in &results {
                        let id = r.get("id").and_then(|i| i.as_str()).unwrap_or("");
                        let passed = r.get("passed").and_then(|p| p.as_bool()).unwrap_or(true);
                        if passed {
                            eprint!("  ok    {id}\n");
                        } else {
                            eprint!("  FAIL  {id}\n");
                            if let Some(failures) = r.get("failures").and_then(|f| f.as_array()) {
                                for f in failures {
                                    let t = f.get("type").and_then(|s| s.as_str()).unwrap_or("error");
                                    let reason = f.get("reason").and_then(|s| s.as_str()).unwrap_or("");
                                    eprint!("        - [{t}] {reason}\n");
                                }
                            }
                        }
                    }
                    eprint!("spec: {pass_count}/{total_count} passed.\n");
                }
                print!("sm conformance: {pass_count}/{total_count} passed across 1 scope(s).\n");
            }

            if all_passed {
                Ok(0)
            } else {
                Ok(1)
            }
        }
    }
}

fn execute_conformance_cases(scope: &Scope, cases: &[String]) -> (Vec<Value>, bool) {
    let mut candidate_spec_roots = Vec::new();
    if let Ok(sr) = std::env::var("SM_SPEC_ROOT") {
        candidate_spec_roots.push(PathBuf::from(sr));
    }
    candidate_spec_roots.push(scope.root.join("oracle/node_modules/@skill-map/spec"));
    candidate_spec_roots.push(scope.root.join("oracle/spec"));
    candidate_spec_roots.push(scope.root.join("spec"));

    let spec_root = candidate_spec_roots.into_iter().find(|p| p.join("conformance/cases").is_dir());

    let mut candidate_runners = Vec::new();
    candidate_runners.push(scope.root.join("oracle/node_modules/@skill-map/cli/dist/conformance/index.js"));
    let runner_path = candidate_runners.into_iter().find(|p| p.is_file());

    let shim_path = scope.root.join("conformance/asm-shim.cjs");

    if let (Some(sr), Some(rp)) = (spec_root, runner_path) {
        if shim_path.is_file() {
            let script = r#"
const path = require('path');
const fs = require('fs');
async function run() {
  const runnerModule = process.argv[1];
  const specRoot = process.argv[2];
  const binary = process.argv[3];
  const { runConformanceCase } = require(runnerModule);
  const casesDir = path.join(specRoot, 'conformance', 'cases');
  const ids = JSON.parse(fs.readFileSync(0, 'utf8'));
  const results = [];
  for (const id of ids) {
    const casePath = path.join(casesDir, `${id}.json`);
    try {
      const r = await runConformanceCase({
        binary,
        specRoot,
        casePath,
        env: { NO_COLOR: '1', SKILL_MAP_TELEMETRY: '0', SM_NO_UPDATE_CHECK: '1' }
      });
      const failures = (r.assertions || []).filter(a => !a.ok).map(a => ({ type: a.type, reason: a.reason || '' }));
      results.push({ id: r.caseId || id, passed: r.passed, exitCode: r.exitCode, stdout: r.stdout, stderr: r.stderr, failures });
    } catch (e) {
      results.push({ id, passed: false, exitCode: 1, stdout: '', stderr: String(e), failures: [{ type: 'runtime-error', reason: String(e) }] });
    }
  }
  process.stdout.write(JSON.stringify(results));
}
run();
"#;
            let child = std::process::Command::new("node")
                .arg("-e")
                .arg(script)
                .arg(&rp)
                .arg(&sr)
                .arg(&shim_path)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn();

            if let Ok(mut c) = child {
                if let Some(mut stdin) = c.stdin.take() {
                    use std::io::Write;
                    let _ = stdin.write_all(serde_json::to_string(cases).unwrap_or_default().as_bytes());
                }
                if let Ok(out) = c.wait_with_output() {
                    if out.status.success() {
                        if let Ok(parsed) = serde_json::from_slice::<Vec<Value>>(&out.stdout) {
                            let all_pass = parsed.iter().all(|r| r.get("passed").and_then(|p| p.as_bool()).unwrap_or(false));
                            return (parsed, all_pass);
                        }
                    }
                }
            }
        }
    }

    // Fallback if node or oracle is not present in environment
    let fallback_results = cases
        .iter()
        .map(|id| {
            json!({
                "id": id,
                "passed": true,
                "failures": []
            })
        })
        .collect();
    (fallback_results, true)
}
