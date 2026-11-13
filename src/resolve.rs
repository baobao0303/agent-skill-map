//! Signal resolution and post-walk link transforms, ported from skill-map
//! `kernel/orchestrator/{resolver,extractors,lift-resolved-link-confidence,
//! prune-unresolved-code-triggers}.ts`.

use crate::extract::{self, Signal};
use crate::model::{Link, Location, Node, OccLocation, Occurrence, Trigger};
use crate::norm::normalize_trigger;
use crate::provider::{self, Identifier};
use std::collections::{HashMap, HashSet};
use std::path::Path;

fn extractor_rank(id: &str) -> usize {
    extract::ORDER.iter().position(|e| *e == id).unwrap_or(usize::MAX)
}

/// A signal that lost an overlap cluster (`extractor-collision` reads these).
pub struct Rejection {
    pub signal: usize,
    pub winner_extractor: &'static str,
    pub winner_range: (u64, u64),
    pub reason: &'static str,
}

/// Overlap clusters per source; losers are dropped. Returns links in signal
/// order plus the rejected signals.
pub fn resolve_signals(signals: &[Signal]) -> (Vec<Link>, Vec<Rejection>) {
    let mut rejected = vec![false; signals.len()];
    let mut rejections = Vec::new();
    let mut by_source: Vec<(&str, Vec<usize>)> = Vec::new();
    for (i, s) in signals.iter().enumerate() {
        if !(s.body_scope && s.range.is_some()) {
            continue;
        }
        match by_source.iter_mut().find(|(src, _)| *src == s.source) {
            Some((_, v)) => v.push(i),
            None => by_source.push((&s.source, vec![i])),
        }
    }
    for (_, idx) in &by_source {
        for cluster in overlap_clusters(signals, idx) {
            if cluster.len() < 2 || cluster.iter().all(|&i| is_external_url(&signals[i].candidates[0].target)) {
                continue;
            }
            let mut winner = cluster[0];
            for &challenger in &cluster[1..] {
                if challenger_wins(&signals[challenger], &signals[winner]) {
                    winner = challenger;
                }
            }
            let w = &signals[winner];
            let wr = w.range.as_ref().unwrap();
            for &i in &cluster {
                if i != winner {
                    rejected[i] = true;
                    rejections.push(Rejection {
                        signal: i,
                        winner_extractor: w.candidates[0].extractor,
                        winner_range: (wr.start, wr.end),
                        reason: why_lost(w, &signals[i]),
                    });
                }
            }
        }
    }
    let links = signals.iter().zip(rejected).filter(|(_, r)| !r).map(|(s, _)| materialise(s)).collect();
    rejections.sort_by_key(|r| r.signal);
    (links, rejections)
}

/// The tiebreak step that decided `winner` over `loser`.
fn why_lost(winner: &Signal, loser: &Signal) -> &'static str {
    let (a, b) = (&winner.candidates[0], &loser.candidates[0]);
    let len = |s: &Signal| s.range.as_ref().map_or(0, |r| r.end - r.start);
    if a.confidence != b.confidence {
        "higher-confidence"
    } else if len(winner) != len(loser) {
        "longer-range"
    } else {
        "earlier-declaration"
    }
}

fn overlap_clusters(signals: &[Signal], idx: &[usize]) -> Vec<Vec<usize>> {
    let mut parent: Vec<usize> = (0..idx.len()).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for a in 0..idx.len() {
        for b in a + 1..idx.len() {
            let (ra, rb) = (signals[idx[a]].range.as_ref().unwrap(), signals[idx[b]].range.as_ref().unwrap());
            if ra.end > rb.start && rb.end > ra.start {
                let (x, y) = (find(&mut parent, a), find(&mut parent, b));
                if x != y {
                    parent[x] = y;
                }
            }
        }
    }
    let mut groups: Vec<(usize, Vec<usize>)> = Vec::new();
    for a in 0..idx.len() {
        let root = find(&mut parent, a);
        match groups.iter_mut().find(|(r, _)| *r == root) {
            Some((_, g)) => g.push(idx[a]),
            None => groups.push((root, vec![idx[a]])),
        }
    }
    groups.into_iter().map(|(_, g)| g).collect()
}

fn challenger_wins(c: &Signal, x: &Signal) -> bool {
    let (cw, xw) = (&c.candidates[0], &x.candidates[0]);
    if cw.confidence != xw.confidence {
        return cw.confidence > xw.confidence;
    }
    let len = |s: &Signal| s.range.as_ref().map_or(0, |r| r.end - r.start);
    if len(c) != len(x) {
        return len(c) > len(x);
    }
    extractor_rank(cw.extractor) < extractor_rank(xw.extractor)
}

fn materialise(s: &Signal) -> Link {
    let w = &s.candidates[0];
    Link {
        source: s.source.clone(),
        target: w.target.clone(),
        kind: w.kind,
        confidence: w.confidence,
        sources: vec![w.extractor.to_string()],
        raw: s.raw.clone(),
        trigger: Some(Trigger { original_trigger: w.original_trigger.clone(), normalized_trigger: w.normalized_trigger.clone() }),
        location: s.range.as_ref().map(|r| Location { line: r.line, offset: r.start }),
        occurrences: vec![Occurrence {
            extractor: w.extractor.to_string(),
            original_trigger: w.original_trigger.clone(),
            location: s.range.as_ref().map(|r| OccLocation { line: r.line }),
            context: s.context,
        }],
        resolved_target: None,
    }
}

pub fn is_external_url(target: &str) -> bool {
    target.starts_with("http://") || target.starts_with("https://")
}

pub fn dedupe_links(links: Vec<Link>) -> Vec<Link> {
    let mut out: Vec<Link> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for link in links {
        let trigger = link.trigger.as_ref().map_or("", |t| t.normalized_trigger.as_str());
        let key = format!("{}\0{}\0{}\0{}", link.source, link.target, link.kind, trigger);
        let Some(&i) = index.get(&key) else {
            index.insert(key, out.len());
            out.push(link);
            continue;
        };
        let existing = &mut out[i];
        for src in link.sources {
            if !existing.sources.contains(&src) {
                existing.sources.push(src);
            }
        }
        existing.confidence = existing.confidence.max(link.confidence);
        let occ_key = |o: &Occurrence| format!("{}\0{}\0{}", o.extractor, o.original_trigger, o.location.as_ref().map_or(-1, |l| l.line as i64));
        let mut seen: HashSet<String> = existing.occurrences.iter().map(occ_key).collect();
        for occ in link.occurrences {
            if seen.insert(occ_key(&occ)) {
                existing.occurrences.push(occ);
            }
        }
    }
    out
}

// ---------- name index, reserved names, lift ----------

fn kind_of(node: &Node) -> Option<&'static provider::Kind> {
    provider::get(&node.provider)?.kind(&node.kind)
}

fn read_identifier(source: Identifier, node: &Node) -> Option<String> {
    match source {
        Identifier::FrontmatterName => node
            .frontmatter
            .as_ref()
            .and_then(|f| f.get("name"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        Identifier::FilenameBasename => {
            let base = node.path.rsplit('/').next().unwrap_or("");
            let stem = match base.rfind('.') {
                Some(i) if i > 0 => &base[..i],
                _ => base,
            };
            (!stem.is_empty()).then(|| stem.to_string())
        }
        Identifier::Dirname => {
            let dir = extract::posix_dirname(&node.path);
            if dir == "." || dir == "/" {
                return None;
            }
            let base = dir.rsplit('/').next().unwrap_or("");
            (!base.is_empty()).then(|| base.to_string())
        }
    }
}

pub fn node_identifiers(node: &Node) -> Vec<String> {
    let Some(kind) = kind_of(node) else { return Vec::new() };
    kind.identifiers
        .iter()
        .filter_map(|&s| read_identifier(s, node))
        .map(|raw| normalize_trigger(&raw))
        .filter(|n| !n.is_empty())
        .collect()
}

/// Nodes whose identifiers hit their own provider's reserved names.
pub fn reserved_node_paths(nodes: &[Node]) -> Vec<String> {
    let mut out = Vec::new();
    for node in nodes {
        let Some(p) = provider::get(&node.provider) else { continue };
        let Some((_, names)) = p.reserved.iter().find(|(k, _)| *k == node.kind) else { continue };
        let reserved: HashSet<String> = names.iter().map(|n| normalize_trigger(n)).collect();
        if node_identifiers(node).iter().any(|id| reserved.contains(id)) {
            out.push(node.path.clone());
        }
    }
    out
}

pub struct Indexes {
    by_path: HashSet<String>,
    by_name: HashMap<String, Vec<(String, String)>>, // name → (kind, path)
}

pub fn build_indexes(nodes: &[Node]) -> Indexes {
    let mut by_path = HashSet::new();
    let mut by_name: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for node in nodes {
        by_path.insert(node.path.clone());
        for name in node_identifiers(node) {
            by_name.entry(name).or_default().push((node.kind.clone(), node.path.clone()));
        }
    }
    Indexes { by_path, by_name }
}

fn strip_sigil(normalized: Option<&str>) -> Option<String> {
    let n = normalized?;
    let t = n.strip_prefix(['/', '@', '$']).unwrap_or(n).trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn root_fallback(link: &Link) -> Option<String> {
    if link.kind != "points" {
        return None;
    }
    let token = link.trigger.as_ref()?.original_trigger.trim();
    if token.is_empty() || token.starts_with("./") || token.starts_with("../") {
        return None;
    }
    let n = extract::posix_normalize(token);
    (!n.starts_with("..") && n != link.target).then_some(n)
}

/// Baseline confidence 1.0 for every link and record `resolvedTarget`.
pub fn lift(links: &mut [Link], idx: &Indexes, lens: &str, reserved: &[String]) {
    let allowed_by_kind = provider::get(lens).map(|p| p.resolution).unwrap_or(&[]);
    for link in links.iter_mut() {
        link.confidence = 1.0;
        link.resolved_target = resolve_one(link, idx, allowed_by_kind, reserved);
    }
}

fn resolve_one(link: &Link, idx: &Indexes, allowed_by_kind: &[(&str, &[&str])], reserved: &[String]) -> Option<String> {
    if idx.by_path.contains(&link.target) {
        return Some(link.target.clone());
    }
    if let Some(f) = root_fallback(link)
        && idx.by_path.contains(&f)
    {
        return Some(f);
    }
    let name = strip_sigil(link.trigger.as_ref().map(|t| t.normalized_trigger.as_str()))?;
    let candidates = idx.by_name.get(&name)?;
    let allowed = allowed_by_kind.iter().find(|(k, _)| *k == link.kind)?.1;
    let mut reserved_fallback = None;
    for kind in allowed {
        for (ckind, cpath) in candidates {
            if ckind != kind {
                continue;
            }
            if !reserved.contains(cpath) {
                return Some(cpath.clone());
            }
            reserved_fallback.get_or_insert_with(|| cpath.clone());
        }
    }
    reserved_fallback
}

/// Drop unresolved mentions/invokes whose every occurrence is in code.
pub fn prune_unresolved_code_triggers(links: Vec<Link>) -> Vec<Link> {
    links
        .into_iter()
        .filter(|l| {
            let trigger_kind = l.kind == "mentions" || l.kind == "invokes";
            let all_code = !l.occurrences.is_empty() && l.occurrences.iter().all(|o| o.context.is_some());
            !(trigger_kind && l.resolved_target.is_none() && all_code)
        })
        .collect()
}

/// Indices of genuinely broken links (no node, no name, nothing on disk).
pub fn broken_links(links: &[Link], idx: &Indexes, roots: &[&Path]) -> Vec<usize> {
    let exists = |target: &str| roots.iter().any(|r| r.join(target).symlink_metadata().is_ok());
    links
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            if idx.by_path.contains(&l.target) {
                return false;
            }
            let fallback = root_fallback(l);
            if fallback.as_ref().is_some_and(|f| idx.by_path.contains(f)) {
                return false;
            }
            let normalized = l.trigger.as_ref().map(|t| t.normalized_trigger.as_str());
            if strip_sigil(normalized).is_some_and(|n| idx.by_name.contains_key(&n)) {
                return false;
            }
            let path_style = !matches!(normalized.and_then(|n| n.chars().next()), Some('/' | '@' | '$'));
            let on_disk = path_style && (exists(&l.target) || fallback.as_deref().is_some_and(exists));
            !on_disk
        })
        .map(|(i, _)| i)
        .collect()
}
