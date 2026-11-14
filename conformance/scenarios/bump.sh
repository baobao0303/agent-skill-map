#!/usr/bin/env bash
# P3 bump differential: the same bump sequence under sm and agent-skill; prints a
# normalized transcript (stdout, stderr, exit, .sm contents) to stdout.
# Usage: bump.sh <sm|agent-skill> <workdir>
REPO=$(cd "$(dirname "$0")/../.." && pwd)
BIN=$([ "$1" = sm ] && echo "$REPO/oracle/node_modules/.bin/sm" || echo "$REPO/target/release/agent-skill")
W=$2
rm -rf "$W" && cp -a "$REPO/conformance/fixtures/example" "$W" && rm -rf "$W/.skill-map"
cd "$W" || exit 1
export NO_COLOR=1 SKILL_MAP_TELEMETRY=0
step() {
  echo "### $*"
  "$BIN" "$@" </dev/null >"$W.o" 2>"$W.e"; local code=$?
  sed -E "s#$W#<W>#g; s/[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z/<TS>/g; s/\"elapsedMs\":[0-9]+/\"elapsedMs\":0/" "$W.o"
  grep -v '^done in\|^$' "$W.e" | sed -E "s#$W#<W>#g" | sed 's/^/E| /'
  echo "exit=$code"
}
show() { echo "--- $1"; sed -E 's/[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z/<TS>/g' "$1"; ls -l "$1" | cut -c1-10; }
"$BIN" scan >/dev/null 2>&1
step bump
step bump docs/STYLE.md --pending
step bump --staged docs/STYLE.md
step bump docs/STYLE.md
python3 -c "
import json;p='.skill-map/settings.json';d=json.load(open(p));d['plugins']={'core':{'extensions':{'node-bump':{'enabled':True}}}};json.dump(d,open(p,'w'),indent=2)"
step bump nope.md
step bump docs/STYLE.md
step bump docs/STYLE.md --yes
show docs/STYLE.sm
show .skill-map/settings.local.json
"$BIN" scan >/dev/null 2>&1
step bump docs/STYLE.md
step bump docs/STYLE.md --force
echo more >> docs/STYLE.md; "$BIN" scan >/dev/null 2>&1
step bump docs/STYLE.md --json
show docs/STYLE.sm
step bump --pending
step bump --pending --json
show .claude/agents/content-editor.sm
"$BIN" scan >/dev/null 2>&1
step bump --pending
step bump --pending --staged
