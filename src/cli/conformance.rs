//! Conformance command family: `sm conformance run`.
//!
//! Cases are executed by the spec's own conformance runner (the reference
//! implementation, `@skill-map/cli/dist/conformance`), driving THIS binary
//! through a throwaway Node shim: the runner spawns `node <binary> …`, so
//! the shim forwards argv, stdio, signals and the exit status. Every case
//! really runs; when the runner or the spec cannot be found the command
//! fails instead of reporting anything.

use crate::cli::{Cli, CliError};
use crate::jsjson;
use crate::scope::Scope;
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
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

const ASSERTION_REASON_DISPLAY_CAP: usize = 1000;

/// Where `@skill-map/spec` and the reference runner live: the env override,
/// then `oracle/node_modules` next to the project, the cwd, or the binary
/// (its ancestors cover `target/release/agent-skill` inside the repo).
fn locate(scope: &Scope) -> (Option<PathBuf>, Option<PathBuf>) {
    let mut spec_roots: Vec<PathBuf> = Vec::new();
    for var in ["SM_SPEC_ROOT", "SKILL_MAP_SPEC_ROOT"] {
        if let Ok(v) = std::env::var(var) {
            spec_roots.push(PathBuf::from(v));
        }
    }
    let mut bases: Vec<PathBuf> = vec![scope.root.clone()];
    if let Ok(cwd) = std::env::current_dir() {
        bases.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        bases.extend(exe.ancestors().skip(1).take(4).map(Path::to_path_buf));
    }
    for b in &bases {
        spec_roots.push(b.join("oracle/node_modules/@skill-map/spec"));
        spec_roots.push(b.join("node_modules/@skill-map/spec"));
    }
    let spec = spec_roots.into_iter().find(|p| p.join("conformance/cases").is_dir());
    let mut runners: Vec<PathBuf> = Vec::new();
    if let Some(scoped) = spec.as_ref().and_then(|s| s.parent()) {
        runners.push(scoped.join("cli/dist/conformance/index.js"));
    }
    for b in &bases {
        runners.push(b.join("oracle/node_modules/@skill-map/cli/dist/conformance/index.js"));
        runners.push(b.join("node_modules/@skill-map/cli/dist/conformance/index.js"));
    }
    let runner = runners.into_iter().find(|p| p.is_file());
    (spec, runner)
}

fn list_cases(spec: &Path) -> Vec<String> {
    let mut ids: Vec<String> = std::fs::read_dir(spec.join("conformance/cases"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.strip_suffix(".json").filter(|s| !s.starts_with('.')).map(String::from)
        })
        .collect();
    ids.sort();
    ids
}

fn runtime_fail(id: &str, why: &str) -> Value {
    json!({ "id": id, "passed": false, "assertions": [], "stdout": "", "stderr": "", "error": why })
}

const DRIVER: &str = r#"
const path = require('path');
const fs = require('fs');
(async () => {
  const [runnerModule, specRoot, binary] = process.argv.slice(1);
  const { runConformanceCase } = require(runnerModule);
  const ids = JSON.parse(fs.readFileSync(0, 'utf8'));
  for (const id of ids) {
    const casePath = path.join(specRoot, 'conformance', 'cases', `${id}.json`);
    let row;
    try {
      const r = await runConformanceCase({ binary, specRoot, casePath, fixturesRoot: path.join(specRoot, 'conformance', 'fixtures') });
      row = { id: r.caseId || id, passed: r.passed, assertions: r.assertions || [], stdout: r.stdout || '', stderr: r.stderr || '' };
    } catch (e) {
      row = { id, passed: false, assertions: [], stdout: '', stderr: '', error: e instanceof Error ? e.message : String(e) };
    }
    process.stdout.write(JSON.stringify(row) + '\n');
  }
})();
"#;

/// Drive the reference runner over `cases`. One result per case, in order;
/// a case the runner could not execute becomes a `runtime-error` failure.
fn execute(spec: &Path, runner: &Path, cases: &[String]) -> Vec<Value> {
    let Ok(exe) = std::env::current_exe() else {
        return cases.iter().map(|c| runtime_fail(c, "cannot resolve the agent-skill binary path")).collect();
    };
    let shim_dir = std::env::temp_dir().join(format!("agent-skill-conformance-{}", std::process::id()));
    let shim = shim_dir.join("shim.cjs");
    let shim_src = format!(
        r#"const {{ spawn }} = require("node:child_process");
const child = spawn({bin}, process.argv.slice(2), {{ stdio: "inherit" }});
for (const sig of ["SIGTERM", "SIGINT", "SIGHUP"]) process.on(sig, () => child.kill(sig));
child.on("error", (e) => {{ process.stderr.write(String(e) + "\n"); process.exit(1); }});
child.on("exit", (code, signal) => {{
  if (signal) {{ process.removeAllListeners(signal); process.kill(process.pid, signal); }}
  else process.exit(code === null ? 1 : code);
}});
"#,
        bin = serde_json::to_string(&exe.to_string_lossy()).unwrap_or_default()
    );
    if std::fs::create_dir_all(&shim_dir).and_then(|_| std::fs::write(&shim, shim_src)).is_err() {
        return cases.iter().map(|c| runtime_fail(c, "cannot write the runner shim to the temp directory")).collect();
    }
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(shim_dir);

    let child = std::process::Command::new("node")
        .arg("-e")
        .arg(DRIVER)
        .arg(runner)
        .arg(spec)
        .arg(&shim)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return cases.iter().map(|c| runtime_fail(c, &format!("cannot start node: {e}"))).collect(),
    };
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(serde_json::to_string(cases).unwrap_or_default().as_bytes());
    }
    let rows: Vec<Value> = child
        .wait_with_output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
        .unwrap_or_default();
    // Every requested case gets a row; one the driver never reported failed.
    cases
        .iter()
        .enumerate()
        .map(|(i, c)| rows.get(i).cloned().unwrap_or_else(|| runtime_fail(c, "the conformance runner exited before reporting this case")))
        .collect()
}

fn truncate_head(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap { s.to_string() } else { format!("{}…", s.chars().take(cap).collect::<String>()) }
}

fn failures_of(row: &Value) -> Vec<Value> {
    if let Some(e) = row.get("error").and_then(Value::as_str) {
        return vec![json!({ "type": "runtime-error", "reason": truncate_head(e, ASSERTION_REASON_DISPLAY_CAP) })];
    }
    row.get("assertions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|a| a.get("ok") != Some(&Value::Bool(true)))
        .map(|a| json!({ "type": a.get("type").cloned().unwrap_or(json!("error")), "reason": a.get("reason").cloned().unwrap_or(json!("")) }))
        .collect()
}

fn json_error(code: &str, message: &str) -> Result<u8, CliError> {
    println!("{}", jsjson::to_string(&json!({ "ok": false, "error": { "code": code, "message": message } })));
    Err(CliError::Raw(String::new(), 2))
}

fn snippet(header: &str, text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    eprint!("{header}");
    for line in trimmed.lines() {
        eprint!("        {line}\n");
    }
}

pub fn run(cli: &Cli, scope: &Scope, args: &ConformanceArgs) -> Result<u8, CliError> {
    let started = Instant::now();
    let ConformanceCommand::Run { case, scope: scope_arg } = &args.command;
    if let Some(s) = scope_arg {
        if s != "all" && s != "spec" {
            let msg = format!("sm conformance: unknown --scope '{s}'. Available: spec.");
            if cli.global.json {
                return json_error("bad-query", &msg);
            }
            return Err(CliError::Raw(format!("✕  {msg}"), 2));
        }
    }

    let (spec, runner) = locate(scope);
    let (Some(spec), Some(runner)) = (spec, runner) else {
        let msg = "sm conformance: cannot find the spec conformance suite (@skill-map/spec) and its reference runner (@skill-map/cli/dist/conformance). Install @skill-map/cli under oracle/node_modules, or point SM_SPEC_ROOT at the spec package.";
        if cli.global.json {
            return json_error("runner-unavailable", msg);
        }
        return Err(CliError::Raw(format!("✕  {msg}"), 2));
    };

    let all_cases = list_cases(&spec);
    let active: Vec<String> = match case {
        Some(target) if !all_cases.iter().any(|c| c == target) => {
            if cli.global.json {
                return json_error("bad-query", &format!("no case with id \"{target}\" in the selected scope(s)"));
            }
            return Err(CliError::Raw(
                format!("✕  sm conformance: no case with id \"{target}\" in the selected scope(s).\n   Run without --case to see every case id, or widen --scope."),
                2,
            ));
        }
        Some(target) => vec![target.clone()],
        None => all_cases,
    };

    let human = !cli.global.json && !cli.global.quiet;
    if human {
        eprint!("Running conformance scope spec ({} case(s)) ...\n", active.len());
    }
    let rows = execute(&spec, &runner, &active);
    let mut pass = 0usize;
    let mut reports = Vec::new();
    for row in &rows {
        let id = row.get("id").and_then(Value::as_str).unwrap_or("");
        if row.get("passed") == Some(&Value::Bool(true)) {
            pass += 1;
            if human {
                eprint!("  ok    {id}\n");
            }
            reports.push(json!({ "id": id, "status": "pass", "failures": [] }));
            continue;
        }
        let failures = failures_of(row);
        if human {
            if let Some(e) = row.get("error").and_then(Value::as_str) {
                eprint!("✕  sm conformance: {e}\n");
                eprint!("  FAIL  {id}\n");
            } else {
                eprint!("  FAIL  {id}\n");
                for f in &failures {
                    let t = f.get("type").and_then(Value::as_str).unwrap_or("error");
                    let reason = f.get("reason").and_then(Value::as_str).unwrap_or("");
                    eprint!("        - [{t}] {}\n", truncate_head(reason, ASSERTION_REASON_DISPLAY_CAP));
                }
                snippet("        --- stdout ---\n", row.get("stdout").and_then(Value::as_str).unwrap_or(""));
                snippet("        --- stderr ---\n", row.get("stderr").and_then(Value::as_str).unwrap_or(""));
            }
        }
        reports.push(json!({ "id": id, "status": "fail", "failures": failures }));
    }
    let total = active.len();
    if human {
        eprint!("spec: {pass}/{total} passed.\n");
    }
    if cli.global.json {
        // Like the original: `ok` describes the run, failures are in the
        // totals and the exit code.
        let report = json!({
            "ok": true,
            "kind": "conformance.result",
            "totals": { "scopes": 1, "cases": total, "passCount": pass, "failCount": total - pass },
            "scopes": [{ "label": "spec", "passCount": pass, "caseCount": total, "cases": reports }],
            "elapsedMs": started.elapsed().as_millis() as u64
        });
        println!("{}", jsjson::to_string(&report));
    } else {
        print!("sm conformance: {pass}/{total} passed across 1 scope(s).\n");
    }
    Ok(if pass == total { 0 } else { 1 })
}
