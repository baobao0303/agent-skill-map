//! `sm agent install | uninstall | status`: materialise the `sm-process-jobs`
//! skill (the queue-processing protocol) into a Provider's skill directory.

use crate::cli::{Cli, CliError};
use crate::format;
use crate::scope::Scope;
use serde_json::json;
use std::path::{Path, PathBuf};

const SKILL_DIR: &str = "sm-process-jobs";
const FILES: [(&str, &str); 3] = [
    ("SKILL.md", include_str!("schema/agent-skill/SKILL.md")),
    ("mcp.md", include_str!("schema/agent-skill/mcp.md")),
    ("cli.md", include_str!("schema/agent-skill/cli.md")),
];
const GENERATED_GITIGNORE: &str = include_str!("schema/agent-skill/generated.gitignore");

/// Providers with a scaffold skill directory: (id, skillDir, marker).
pub const TARGETS: [(&str, &str, Option<&str>); 5] = [
    ("claude", ".claude/skills", None),
    ("antigravity", ".agents/skills", None),
    ("codex", ".agents/skills", Some(".codex")),
    ("opencode", ".agents/skills", None),
    ("agent-skills", ".agents/skills", None),
];

fn folder(root: &Path, dir: &str) -> PathBuf {
    root.join(dir).join(SKILL_DIR)
}

fn all_match(folder: &Path) -> bool {
    FILES.iter().all(|(f, c)| std::fs::read_to_string(folder.join(f)).is_ok_and(|t| t == *c))
}

/// (installed, stale) for one skill directory.
pub fn status(root: &Path, dir: &str) -> (bool, bool) {
    let installed = folder(root, dir).join("SKILL.md").exists();
    (installed, installed && !all_match(&folder(root, dir)))
}

/// `processingSkillPresence`: (installed anywhere, fresh anywhere).
pub fn presence(root: &Path) -> (bool, bool) {
    let mut dirs: Vec<&str> = TARGETS.iter().map(|t| t.1).collect();
    dirs.dedup();
    let s: Vec<(bool, bool)> = dirs.iter().map(|d| status(root, d)).collect();
    (s.iter().any(|x| x.0), s.iter().any(|x| x.0 && !x.1))
}

fn resolve_target(scope: &Scope, for_provider: Option<&str>) -> Result<(&'static str, &'static str, Option<&'static str>), CliError> {
    let ids: Vec<&str> = TARGETS.iter().map(|t| t.0).collect();
    let hint = format!("Providers with a skill directory: {}. Re-run with `--for <id>`.", ids.join(", "));
    if let Some(p) = for_provider {
        if let Some(t) = TARGETS.iter().find(|t| t.0 == p) {
            return Ok(*t);
        }
        let registered = crate::provider::get(p).is_some();
        let head = if registered {
            format!("sm agent: provider \"{}\" declares no skill directory", format::sanitize(p))
        } else {
            format!("sm agent: no registered provider \"{}\"", format::sanitize(p))
        };
        return Err(CliError::Usage(format!("{head}\n   {hint}")));
    }
    let lens = crate::cli::detect_lens(scope);
    TARGETS.iter().find(|t| t.0 == lens).copied().ok_or_else(|| {
        CliError::Usage(format!("sm agent: the active lens \"{}\" declares no skill directory\n   {hint}", format::sanitize(&lens)))
    })
}

fn envelope(kind: &str, id: &str, dir: &str, outcome: &str, started: std::time::Instant) -> String {
    crate::jsjson::to_string(&json!({
        "ok": true, "kind": kind, "provider": id, "skillDir": dir, "outcome": outcome,
        "elapsedMs": started.elapsed().as_millis() as u64,
    }))
}

pub fn install(cli: &Cli, scope: &Scope, for_provider: Option<&str>) -> Result<u8, CliError> {
    let started = std::time::Instant::now();
    let (id, dir, marker) = resolve_target(scope, for_provider)?;
    let f = folder(&scope.root, dir);
    let outcome = if !f.join("SKILL.md").exists() {
        "installed"
    } else if all_match(&f) {
        "up-to-date"
    } else {
        "updated"
    };
    let io = |e: std::io::Error| CliError::Operational(format!("sm agent: install failed: {}", format::sanitize(&e.to_string())));
    if outcome != "up-to-date" {
        std::fs::create_dir_all(&f).map_err(io)?;
        for (name, content) in FILES {
            std::fs::write(f.join(name), content).map_err(io)?;
        }
    }
    if outcome == "installed" && !f.join(".gitignore").exists() {
        std::fs::write(f.join(".gitignore"), GENERATED_GITIGNORE).map_err(io)?;
    }
    if let Some(m) = marker {
        std::fs::create_dir_all(scope.root.join(m)).map_err(io)?;
    }
    if cli.global.json {
        println!("{}", envelope("agent-install", id, dir, outcome, started));
        return Ok(0);
    }
    let path = format!("{dir}/{SKILL_DIR}/SKILL.md");
    let id_s = format::sanitize(id);
    match outcome {
        "installed" => println!("✓  sm agent: installed the sm-process-jobs skill at {path} ({id_s} lens)."),
        "updated" => println!("✓  sm agent: updated the sm-process-jobs skill at {path} to this CLI's version ({id_s} lens)."),
        _ => println!("✓  sm agent: the sm-process-jobs skill at {path} is already up to date ({id_s} lens)."),
    }
    if outcome != "up-to-date" && !cli.global.quiet {
        eprintln!("Any agent booted in this directory can now process the queue; check with `sm agent status`.");
    }
    Ok(0)
}

pub fn uninstall(cli: &Cli, scope: &Scope, for_provider: Option<&str>) -> Result<u8, CliError> {
    let started = std::time::Instant::now();
    let (id, dir, _) = resolve_target(scope, for_provider)?;
    let f = folder(&scope.root, dir);
    let removed = f.exists();
    if removed {
        std::fs::remove_dir_all(&f)
            .map_err(|e| CliError::Operational(format!("sm agent: uninstall failed: {}", format::sanitize(&e.to_string()))))?;
    }
    if cli.global.json {
        println!("{}", envelope("agent-uninstall", id, dir, if removed { "removed" } else { "not-installed" }, started));
    } else if removed {
        println!("✓  sm agent: removed the sm-process-jobs skill at {dir}/{SKILL_DIR}/.");
    } else if !cli.global.quiet {
        eprintln!("ℹ  sm agent: no sm-process-jobs skill at {dir}/{SKILL_DIR}/; nothing to do.");
    }
    Ok(0)
}

pub fn status_cmd(cli: &Cli, scope: &Scope, for_provider: Option<&str>) -> Result<u8, CliError> {
    let (id, dir, _) = resolve_target(scope, for_provider)?;
    let (installed, stale) = status(&scope.root, dir);
    if cli.global.json {
        println!(
            "{}",
            crate::jsjson::to_string(&json!({ "provider": id, "skillDir": dir, "installed": installed, "stale": stale }))
        );
    } else if !installed {
        println!("·  {id}: not installed ({dir}/{SKILL_DIR}/)");
    } else if stale {
        println!("⚠  {id}: installed (stale), {dir}/{SKILL_DIR}/SKILL.md differs from this CLI's copy\n   Re-run `sm agent install` to refresh it.");
    } else {
        println!("✓  {id}: installed ({dir}/{SKILL_DIR}/SKILL.md)");
    }
    Ok(0)
}
