//! Filesystem walk matching skill-map `kernel/scan/walk-content.ts`:
//! depth-first, entries of each directory in byte order (libuv scandir),
//! one gitignore-style matcher built from the layered ignore sources,
//! extension filter, size cap, and realpath containment for symlinks.
//! Reading/parsing the yielded files happens in parallel elsewhere (Rayon);
//! the walk itself stays sequential so its order is deterministic.

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const BUNDLED_IGNORE: &str = include_str!("../schema/skillmapignore");

pub struct IgnoreLayers<'a> {
    pub gitignore: Option<&'a str>,
    pub config: &'a [String],
    pub skillmapignore: Option<&'a str>,
}

pub fn build_ignore(root: &Path, layers: &IgnoreLayers) -> Gitignore {
    let mut b = GitignoreBuilder::new(root);
    let mut add = |text: &str| {
        for line in text.lines() {
            // Invalid patterns are dropped (the original logs a warning).
            let _ = b.add_line(None, line);
        }
    };
    add(BUNDLED_IGNORE);
    if let Some(t) = layers.gitignore {
        add(t);
    }
    for line in layers.config {
        add(line);
    }
    if let Some(t) = layers.skillmapignore {
        add(t);
    }
    b.build().unwrap_or_else(|_| Gitignore::empty())
}

pub struct Entry {
    pub rel: String,
    pub abs: PathBuf,
    pub mtime_ms: i64,
}

pub struct Oversized {
    pub path: String,
    pub bytes: u64,
}

pub struct WalkOpts<'a> {
    pub ignore: &'a Gitignore,
    pub extension: &'a str,
    pub max_file_size: u64,
    pub follow_external_symlinks: bool,
}

/// Walk every root for one read rule. Oversized files are reported once per
/// path across calls via `oversized`.
pub fn walk(roots: &[PathBuf], opts: &WalkOpts, oversized: &mut Vec<Oversized>) -> Vec<Entry> {
    let root_reals: Vec<PathBuf> = roots.iter().filter_map(|r| r.canonicalize().ok()).collect();
    let mut out = Vec::new();
    for root in roots {
        let Ok(real) = root.canonicalize() else { continue };
        let mut chain = HashSet::from([real.clone()]);
        walk_dir(root, root, &mut chain, &root_reals, opts, oversized, &mut out);
    }
    out
}

fn ignored(opts: &WalkOpts, rel: &str, is_dir: bool) -> bool {
    !rel.is_empty() && opts.ignore.matched_path_or_any_parents(rel, is_dir).is_ignore()
}

fn walk_dir(
    root: &Path,
    dir: &Path,
    chain: &mut HashSet<PathBuf>,
    root_reals: &[PathBuf],
    opts: &WalkOpts,
    oversized: &mut Vec<Oversized>,
    out: &mut Vec<Entry>,
) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = read.filter_map(Result::ok).collect();
    entries.sort_by(|a, b| a.file_name().as_encoded_bytes().cmp(b.file_name().as_encoded_bytes()));
    for entry in entries {
        let full = entry.path();
        let rel = rel_path(root, &full);
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            let Ok(target) = full.canonicalize() else { continue };
            if !opts.follow_external_symlinks && !root_reals.iter().any(|r| target.starts_with(r)) {
                continue;
            }
            if target.is_dir() {
                if ignored(opts, &rel, true) || !chain.insert(target.clone()) {
                    continue;
                }
                walk_dir(root, &full, chain, root_reals, opts, oversized, out);
                chain.remove(&target);
            } else if !ignored(opts, &rel, false) {
                accept_file(&full, rel, opts, oversized, out);
            }
            continue;
        }
        if ft.is_dir() {
            if ignored(opts, &rel, true) {
                continue;
            }
            let real = full.canonicalize().unwrap_or_else(|_| full.clone());
            if !chain.insert(real.clone()) {
                continue;
            }
            walk_dir(root, &full, chain, root_reals, opts, oversized, out);
            chain.remove(&real);
        } else if ft.is_file() && !ignored(opts, &rel, false) {
            accept_file(&full, rel, opts, oversized, out);
        }
    }
}

fn accept_file(full: &Path, rel: String, opts: &WalkOpts, oversized: &mut Vec<Oversized>, out: &mut Vec<Entry>) {
    if !rel.ends_with(opts.extension) {
        return;
    }
    let Ok(meta) = std::fs::metadata(full) else { return };
    if !meta.is_file() {
        return;
    }
    if meta.len() > opts.max_file_size {
        if !oversized.iter().any(|o| o.path == rel) {
            oversized.push(Oversized { path: rel, bytes: meta.len() });
        }
        return;
    }
    out.push(Entry { rel, abs: full.to_path_buf(), mtime_ms: mtime_ms(&meta) });
}

pub fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| (d.as_secs_f64() * 1000.0).round() as i64)
        .unwrap_or(0)
}

fn rel_path(root: &Path, full: &Path) -> String {
    full.strip_prefix(root)
        .unwrap_or(full)
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_fixture_walk_order() {
        let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/conformance/fixtures/example"));
        let skillmapignore = std::fs::read_to_string(root.join(".skillmapignore")).ok();
        let ig = build_ignore(&root, &IgnoreLayers { gitignore: None, config: &[], skillmapignore: skillmapignore.as_deref() });
        let opts = WalkOpts { ignore: &ig, extension: ".md", max_file_size: 1 << 20, follow_external_symlinks: false };
        let rels: Vec<String> = walk(&[root.clone()], &opts, &mut Vec::new()).into_iter().map(|e| e.rel).collect();
        assert_eq!(
            rels,
            [
                ".claude/agents/content-editor.md",
                ".claude/commands/init.md",
                ".claude/commands/publish.md",
                ".claude/skills/check-links/SKILL.md",
                ".claude/skills/notion-publish/SKILL.md",
                "AGENTS.md",
                "docs/DEPLOY.md",
                "docs/STYLE.md",
            ]
        );
    }

    #[test]
    fn dfs_not_full_path_sort() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("a")).unwrap();
        std::fs::write(dir.path().join("a/b.md"), "x").unwrap();
        std::fs::write(dir.path().join("a-c.md"), "x").unwrap();
        let ig = build_ignore(dir.path(), &IgnoreLayers { gitignore: None, config: &[], skillmapignore: None });
        let opts = WalkOpts { ignore: &ig, extension: ".md", max_file_size: 1 << 20, follow_external_symlinks: false };
        let rels: Vec<String> = walk(&[dir.path().to_path_buf()], &opts, &mut Vec::new()).into_iter().map(|e| e.rel).collect();
        assert_eq!(rels, ["a/b.md", "a-c.md"]);
    }
}
