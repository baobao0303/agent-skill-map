#!/usr/bin/env bash
# P3 orphans differential: list / reconcile / undo-rename after renames.
# Usage: orphans.sh <sm|agent-skill> <workdir>
REPO=$(cd "$(dirname "$0")/../.." && pwd)
BIN=$([ "$1" = sm ] && echo "$REPO/oracle/node_modules/.bin/sm" || echo "$REPO/target/release/agent-skill")
W=$2
rm -rf "$W" && cp -a "$REPO/conformance/fixtures/claude-coverage" "$W" && rm -rf "$W/.skill-map"
cd "$W" || exit 1
export NO_COLOR=1 SKILL_MAP_TELEMETRY=0
step() {
  echo "### $*"
  "$BIN" "$@" </dev/null >"$W.o" 2>"$W.e"; local code=$?
  sed -E "s#$W#<W>#g" "$W.o"
  grep -v '^done in\|^$' "$W.e" | sed -E "s#$W#<W>#g" | sed 's/^/E| /'
  echo "exit=$code"
}
db() { python3 -c "
import sqlite3,sys;c=sqlite3.connect('.skill-map/skill-map.db')
for q in sys.argv[1:]: print(q.split()[1], c.execute(q).fetchall())" "$@"; }
step orphans
"$BIN" scan >/dev/null 2>&1
step orphans
python3 -c "
import sqlite3;c=sqlite3.connect('.skill-map/skill-map.db')
for p in ['docs/runbook.md','.claude/commands/status.md','.claude/agents/dup-a.md']:
  c.execute('insert into state_node_favorites(node_path,favorited_at) values (?,1)',(p,))
c.execute(\"insert into state_node_favorites(node_path,favorited_at) values ('CLAUDE.md',1)\")
c.commit()"
mv docs/runbook.md docs/runbook2.md && echo "extra" >> docs/runbook2.md
mv .claude/agents/dup-a.md .claude/agents/dup-c.md && echo "x" >> .claude/agents/dup-c.md
rm .claude/commands/status.md
"$BIN" scan >/dev/null 2>&1
step orphans
step orphans --kind medium --json
step orphans --kind bogus
step orphans reconcile .claude/commands/status.md --to nope.md
step orphans reconcile nope.md --to CLAUDE.md
step orphans reconcile .claude/commands/status.md --to CLAUDE.md -n
step orphans reconcile .claude/commands/status.md --to CLAUDE.md
db "select node_path from state_node_favorites order by 1"
step orphans undo-rename nope.md
step orphans undo-rename docs/runbook2.md --from x.md
step orphans undo-rename docs/runbook2.md
step orphans undo-rename docs/runbook2.md -n
step orphans undo-rename docs/runbook2.md --force
db "select node_path from state_node_favorites order by 1" "select analyzer_id, node_ids_json, message, data_json from scan_issues where analyzer_id in ('orphan','auto-rename-medium') order by id"
step orphans
echo "--- log"; sed -E 's/"at":"[^"]*"/"at":"<TS>"/' .skill-map/operations.log | grep orphans
