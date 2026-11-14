#!/usr/bin/env bash
# P2 watcher differential: `watch --json` over the example fixture while a
# file is edited and another renamed; writes <workdir>.out / .err.
# Usage: watch.sh <sm|agent-skill> <workdir>
REPO=$(cd "$(dirname "$0")/../.." && pwd)
BIN=$([ "$1" = sm ] && echo "$REPO/oracle/node_modules/.bin/sm" || echo "$REPO/target/release/agent-skill")
W=$2
rm -rf "$W" && cp -a "$REPO/conformance/fixtures/example" "$W" && rm -rf "$W/.skill-map"
(cd "$W" && NO_COLOR=1 SKILL_MAP_TELEMETRY=0 timeout -s INT 10 "$BIN" watch --json >"$W.out" 2>"$W.err") &
sleep 4; echo "- [deploy](DEPLOY.md)" >> "$W/docs/STYLE.md"
sleep 2; mv "$W/docs/DEPLOY.md" "$W/docs/DEPLOY2.md"
sleep 2; echo "tail" >> "$W/AGENTS.md"
wait
