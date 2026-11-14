#!/usr/bin/env bash
# P3 issues dismiss / undismiss / suppressions differential (claude-coverage).
# Usage: issues.sh <sm|agent-skill> <workdir>
REPO=$(cd "$(dirname "$0")/../.." && pwd)
BIN=$([ "$1" = sm ] && echo "$REPO/oracle/node_modules/.bin/sm" || echo "$REPO/target/release/agent-skill")
W=$2
rm -rf "$W" && cp -a "$REPO/conformance/fixtures/claude-coverage" "$W" && rm -rf "$W/.skill-map"
cd "$W" || exit 1
export NO_COLOR=1 SKILL_MAP_TELEMETRY=0
step() {
  echo "### $*"
  "$BIN" "$@" </dev/null >"$W.o" 2>"$W.e"; local code=$?
  sed -E "s#$W#<W>#g; s/\"elapsedMs\":[0-9]+/\"elapsedMs\":0/" "$W.o"
  grep -v '^done in\|^$' "$W.e" | sed -E "s#$W#<W>#g" | sed 's/^/E| /'
  echo "exit=$code"
}
R=.claude/agents/reviewer.md
step issues suppressions
"$BIN" scan >/dev/null 2>&1
step issues suppressions
step issues dismiss bogus x -n $R
step issues dismiss reference-broken @writer -n nope.md
step issues dismiss reference-broken @writer -n $R
step issues dismiss reference-broken @writer -n $R --yes --note "a teammate, not a node"
step issues dismiss core/reference-broken @writer -n $R --json
step issues dismiss core/reference-broken @Angular -n $R --json
step check -n $R
step issues suppressions
step issues suppressions --json
step issues suppressions -n CLAUDE.md
echo "--- sidecar"; cat .claude/agents/reviewer.sm
"$BIN" scan >/dev/null 2>&1
step check -n $R
step issues undismiss reference-broken @nobody -n $R
step issues undismiss core/reference-broken @writer -n $R
step issues undismiss reference-broken @Angular -n $R --json
echo "--- sidecar"; cat .claude/agents/reviewer.sm
echo "--- log"; sed -E 's/"at":"[^"]*"/"at":"<TS>"/' .skill-map/operations.log | grep issues
