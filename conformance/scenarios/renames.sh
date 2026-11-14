#!/usr/bin/env bash
# P2 incremental/rename differential: run the same edit sequence under sm
# and agent-skill, writing each scan's JSON to <workdir>.N.json.
# Usage: renames.sh <sm|agent-skill> <workdir>
set -e
REPO=$(cd "$(dirname "$0")/../.." && pwd)
BIN=$([ "$1" = sm ] && echo "$REPO/oracle/node_modules/.bin/sm" || echo "$REPO/target/release/agent-skill")
W=$2
rm -rf "$W" && cp -a "$REPO/conformance/fixtures/claude-coverage" "$W" && rm -rf "$W/.skill-map"
cd "$W"
export SKILL_MAP_TELEMETRY=0 SM_NO_UPDATE_CHECK=1 NO_COLOR=1
"$BIN" scan --json > "$W.1.json" 2>/dev/null || true
# high: pure move
mv .claude/agents/planner.md .claude/agents/planner2.md
# medium: same frontmatter, new body
mv docs/runbook.md docs/runbook2.md && echo "extra line" >> docs/runbook2.md
# medium + orphan: one moved with edits, its sibling deleted
mv .claude/agents/dup-a.md .claude/agents/dup-c.md && echo "x" >> .claude/agents/dup-c.md && rm .claude/agents/dup-b.md
# orphan: plain delete
rm .claude/commands/status.md
# edit in place (cache miss) and sidecar-only edit
echo "See [style](style.md)." >> docs/style.md
"$BIN" scan --json > "$W.2.json" 2>/dev/null || true
"$BIN" scan --json > "$W.3.json" 2>/dev/null || true
"$BIN" scan --full --json > "$W.4.json" 2>/dev/null || true
