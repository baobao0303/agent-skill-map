//! The six built-in providers as static data (spec_doc §7). Registration
//! order is dispatch order; `markdown` is the non-gated universal fallback.

#[derive(Clone, Copy, PartialEq)]
pub enum Parser {
    FrontmatterYaml,
    Toml,
}

pub struct ReadRule {
    pub extension: &'static str,
    pub parser: Parser,
    pub body_field: Option<&'static str>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Identifier {
    FrontmatterName,
    FilenameBasename,
    Dirname,
}

pub struct Kind {
    pub name: &'static str,
    pub identifiers: &'static [Identifier],
    pub identifier_mismatch_warn: bool,
}

pub struct Provider {
    pub id: &'static str,
    pub gated: bool,
    pub markers: &'static [&'static str],
    pub detect_fallback: bool,
    pub subsumes: &'static [&'static str],
    pub read: &'static [ReadRule],
    pub kinds: &'static [Kind],
    /// link kind → target kinds, priority ordered.
    pub resolution: &'static [(&'static str, &'static [&'static str])],
    /// node kind → names the runtime reserves.
    pub reserved: &'static [(&'static str, &'static [&'static str])],
    pub mcp_sources: &'static [(&'static str, McpDialect)],
    pub classify: fn(&str) -> Option<&'static str>,
}

#[derive(Clone, Copy)]
pub enum McpDialect {
    Json,
    Toml,
}

use Identifier::{Dirname, FilenameBasename, FrontmatterName};

const MD: &[ReadRule] = &[ReadRule { extension: ".md", parser: Parser::FrontmatterYaml, body_field: None }];

const SKILL: Kind = Kind { name: "skill", identifiers: &[FrontmatterName, Dirname], identifier_mismatch_warn: true };

const CLAUDE_RESERVED_SLASH: &[&str] = &[
    "help", "clear", "compact", "cost", "init", "model", "agents", "login", "logout", "mcp", "memory", "config",
    "doctor", "permissions", "add-dir", "bug", "pr-comments", "release-notes", "review", "terminal-setup", "vim",
    "output-style", "hooks", "install-github-app", "migrate-installer", "upgrade", "resume", "exit", "quit",
    "security-review", "statusline", "usage", "feedback",
];

const ANTIGRAVITY_RESERVED: &[&str] = &[
    "add-dir", "agents", "clear", "config", "exit", "feedback", "help", "hooks", "logout", "mcp", "model",
    "permissions", "quit", "resume", "statusline", "usage", "artifact", "branch", "btw", "changelog", "context",
    "conversation", "copy", "credits", "diff", "fast", "fork", "goal", "grill-me", "keybindings", "new", "open",
    "planning", "quota", "rename", "rewind", "schedule", "settings", "skills", "switch", "tasks", "title", "undo",
];

const OPENCODE_RESERVED: &[&str] = &[
    "add-dir", "agents", "clear", "config", "exit", "feedback", "help", "hooks", "logout", "mcp", "model",
    "permissions", "quit", "resume", "statusline", "usage", "init", "redo", "share", "undo",
];

fn commons_skill(lower: &str) -> bool {
    let Some(rest) = lower.strip_prefix(".agents/skills/") else { return false };
    matches!(rest.split_once('/'), Some((dir, "skill.md")) if !dir.is_empty())
}

fn one_segment_md(lower: &str, prefix: &str) -> bool {
    lower.strip_prefix(prefix).is_some_and(|rest| !rest.contains('/') && rest.ends_with(".md") && rest.len() > 3)
}

fn skill_under(lower: &str, prefix: &str) -> bool {
    lower
        .strip_prefix(prefix)
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(dir, file)| !dir.is_empty() && file == "skill.md")
}

fn classify_claude(path: &str) -> Option<&'static str> {
    let lower = path.to_lowercase();
    if lower.starts_with(".claude/agents/") {
        Some("agent")
    } else if lower.starts_with(".claude/commands/") {
        Some("command")
    } else if skill_under(&lower, ".claude/skills/") {
        Some("skill")
    } else {
        None
    }
}

fn classify_antigravity(path: &str) -> Option<&'static str> {
    let lower = path.to_lowercase();
    if one_segment_md(&lower, ".agent/workflows/") {
        Some("workflow")
    } else if commons_skill(&lower) {
        Some("skill")
    } else {
        None
    }
}

fn classify_codex(path: &str) -> Option<&'static str> {
    let lower = path.to_lowercase();
    if lower.starts_with(".codex/agents/") && lower.ends_with(".toml") {
        Some("agent")
    } else if commons_skill(&lower) {
        Some("skill")
    } else {
        None
    }
}

fn classify_opencode(path: &str) -> Option<&'static str> {
    let lower = path.to_lowercase();
    if one_segment_md(&lower, ".opencode/agent/") {
        Some("agent")
    } else if one_segment_md(&lower, ".opencode/commands/") {
        Some("command")
    } else if skill_under(&lower, ".opencode/skills/") || skill_under(&lower, ".claude/skills/") || commons_skill(&lower) {
        Some("skill")
    } else {
        None
    }
}

fn classify_agent_skills(path: &str) -> Option<&'static str> {
    commons_skill(&path.to_lowercase()).then_some("skill")
}

fn classify_markdown(_: &str) -> Option<&'static str> {
    Some("markdown")
}

pub static PROVIDERS: [Provider; 6] = [
    Provider {
        id: "claude",
        gated: true,
        markers: &[".claude"],
        detect_fallback: false,
        subsumes: &[],
        read: MD,
        kinds: &[
            Kind { name: "agent", identifiers: &[FrontmatterName, FilenameBasename], identifier_mismatch_warn: true },
            Kind { name: "command", identifiers: &[FrontmatterName, FilenameBasename], identifier_mismatch_warn: true },
            SKILL,
            Kind { name: "mcp", identifiers: &[FrontmatterName], identifier_mismatch_warn: false },
        ],
        resolution: &[("mentions", &["agent", "skill", "markdown"]), ("invokes", &["command", "skill"])],
        reserved: &[
            ("command", CLAUDE_RESERVED_SLASH),
            ("skill", CLAUDE_RESERVED_SLASH),
            ("agent", &["general-purpose", "output-style-setup", "statusline-setup"]),
        ],
        mcp_sources: &[(".mcp.json", McpDialect::Json)],
        classify: classify_claude,
    },
    Provider {
        id: "antigravity",
        gated: true,
        markers: &[".agent/workflows"],
        detect_fallback: false,
        subsumes: &[],
        read: MD,
        kinds: &[SKILL, Kind { name: "workflow", identifiers: &[FilenameBasename], identifier_mismatch_warn: false }],
        resolution: &[("invokes", &["skill", "workflow"])],
        reserved: &[("skill", ANTIGRAVITY_RESERVED), ("workflow", ANTIGRAVITY_RESERVED)],
        mcp_sources: &[],
        classify: classify_antigravity,
    },
    Provider {
        id: "codex",
        gated: true,
        markers: &[".codex"],
        detect_fallback: false,
        subsumes: &[],
        read: &[
            ReadRule { extension: ".toml", parser: Parser::Toml, body_field: Some("developer_instructions") },
            ReadRule { extension: ".md", parser: Parser::FrontmatterYaml, body_field: None },
        ],
        kinds: &[
            Kind { name: "agent", identifiers: &[FrontmatterName, FilenameBasename], identifier_mismatch_warn: true },
            SKILL,
        ],
        resolution: &[("invokes", &["skill"])],
        reserved: &[],
        mcp_sources: &[(".codex/config.toml", McpDialect::Toml)],
        classify: classify_codex,
    },
    Provider {
        id: "opencode",
        gated: true,
        markers: &[".opencode"],
        detect_fallback: false,
        subsumes: &["claude"],
        read: MD,
        kinds: &[
            Kind { name: "agent", identifiers: &[FilenameBasename], identifier_mismatch_warn: false },
            Kind { name: "command", identifiers: &[FilenameBasename], identifier_mismatch_warn: false },
            SKILL,
        ],
        resolution: &[("invokes", &["command"])],
        reserved: &[("command", OPENCODE_RESERVED)],
        mcp_sources: &[("opencode.json", McpDialect::Json)],
        classify: classify_opencode,
    },
    Provider {
        id: "agent-skills",
        gated: true,
        markers: &[".agents"],
        detect_fallback: true,
        subsumes: &[],
        read: MD,
        kinds: &[SKILL],
        resolution: &[("invokes", &["skill"])],
        reserved: &[],
        mcp_sources: &[],
        classify: classify_agent_skills,
    },
    Provider {
        id: "markdown",
        gated: false,
        markers: &[],
        detect_fallback: false,
        subsumes: &[],
        read: MD,
        kinds: &[Kind { name: "markdown", identifiers: &[FilenameBasename], identifier_mismatch_warn: false }],
        resolution: &[],
        reserved: &[],
        mcp_sources: &[],
        classify: classify_markdown,
    },
];

pub fn get(id: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|p| p.id == id)
}

impl Provider {
    pub fn kind(&self, name: &str) -> Option<&Kind> {
        self.kinds.iter().find(|k| k.name == name)
    }
}

/// Lens detection from on-disk markers (spec_doc §7.1). Returns the
/// candidate provider ids after fallback and subsumption rules.
pub fn detect_candidates(root: &std::path::Path) -> Vec<&'static str> {
    let present: Vec<&Provider> = PROVIDERS
        .iter()
        .filter(|p| p.gated && p.markers.iter().any(|m| root.join(m).exists()))
        .collect();
    let vendors: Vec<&Provider> = present.iter().copied().filter(|p| !p.detect_fallback).collect();
    let mut candidates: Vec<&Provider> = if vendors.is_empty() { present } else { vendors };
    // One-way only: two providers subsuming each other keep the ambiguity.
    let subsumed: Vec<&str> = candidates
        .iter()
        .flat_map(|absorber| {
            absorber.subsumes.iter().copied().filter(|s| {
                candidates.iter().any(|c| c.id == *s && !c.subsumes.contains(&absorber.id))
            })
        })
        .collect();
    candidates.retain(|p| !subsumed.contains(&p.id));
    candidates.into_iter().map(|p| p.id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_table() {
        let claude = get("claude").unwrap();
        assert_eq!((claude.classify)(".claude/agents/foo.md"), Some("agent"));
        assert_eq!((claude.classify)(".claude/commands/sub/x.md"), Some("command"));
        assert_eq!((claude.classify)(".claude/skills/check-links/SKILL.md"), Some("skill"));
        assert_eq!((claude.classify)(".claude/skills/a/b/SKILL.md"), None);
        assert_eq!((claude.classify)("CLAUDE.md"), None);
        let codex = get("codex").unwrap();
        assert_eq!((codex.classify)(".codex/agents/x.toml"), Some("agent"));
        assert_eq!((codex.classify)(".agents/skills/x/SKILL.md"), Some("skill"));
        let ag = get("antigravity").unwrap();
        assert_eq!((ag.classify)(".agent/workflows/deploy.md"), Some("workflow"));
        assert_eq!((ag.classify)(".agent/workflows/a/b.md"), None);
        let oc = get("opencode").unwrap();
        assert_eq!((oc.classify)(".claude/skills/x/SKILL.md"), Some("skill"));
        assert_eq!((oc.classify)(".claude/agents/x.md"), None);
    }

    #[test]
    fn detection_rules() {
        let dir = tempfile::tempdir().unwrap();
        assert!(detect_candidates(dir.path()).is_empty());
        std::fs::create_dir(dir.path().join(".agents")).unwrap();
        assert_eq!(detect_candidates(dir.path()), ["agent-skills"]);
        std::fs::create_dir(dir.path().join(".claude")).unwrap();
        assert_eq!(detect_candidates(dir.path()), ["claude"]);
        std::fs::create_dir(dir.path().join(".opencode")).unwrap();
        assert_eq!(detect_candidates(dir.path()), ["opencode"]);
        std::fs::create_dir(dir.path().join(".codex")).unwrap();
        assert_eq!(detect_candidates(dir.path()), ["codex", "opencode"]);
    }
}
