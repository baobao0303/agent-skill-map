#!/usr/bin/env bash
# Reset oracle/runs/cmp from a fixture and pre-scan it with the given binary.
# Usage: reset.sh <fixture-name> <sm|asm> [no-scan]
set -e
REPO=/home/baoltb/Desktop/agent-skill-map
rm -rf "$REPO/oracle/runs/cmp" && cp -a "$REPO/conformance/fixtures/$1" "$REPO/oracle/runs/cmp"
[ "${3:-}" = no-scan ] && exit 0
BIN=$([ "$2" = sm ] && echo "$REPO/oracle/node_modules/.bin/sm" || echo "$REPO/target/release/asm")
cd "$REPO/oracle/runs/cmp" && SKILL_MAP_TELEMETRY=0 SM_NO_UPDATE_CHECK=1 NO_COLOR=1 "$BIN" scan >/dev/null 2>&1 || true
