#!/usr/bin/env bash
# REA differential check: capture sm (oracle) and asm on the same fixture,
# same working directory and environment, then compare the two captures.
# Usage: rea/compare.sh <fixture> <args...>   (outputs land in rea/compare/)
set -u
REPO=/home/baoltb/Desktop/agent-skill-map
FIX=$1; shift
TAG="${FIX}__$(echo "$*" | tr ' /-' '___')"
OUT=$REPO/rea/compare
WD=$REPO/oracle/runs/cmp
args_json=$(printf '%s\n' "$@" | jq -R . | jq -s -c .)
capture() { # <bin-name> <executable> <prescan: yes|no>
  "$REPO/oracle/reset.sh" "$FIX" "$1" $([ "$3" = no ] && echo no-scan) >/dev/null
  jq -n --arg exe "$2" --arg wd "$WD" --argjson args "$args_json" '{
    executable: $exe, arguments: $args, working_directory: $wd,
    environment: {SKILL_MAP_TELEMETRY: "0", SM_NO_UPDATE_CHECK: "1", NO_COLOR: "1"},
    normalization: {paths: true, pids: true, ports: true, time_bucket_ms: 1000},
    terminal: {columns: 200, rows: 60, scrollback: 2000}, timeout_ms: 60000}' > "$OUT/$TAG.$1.scenario.json"
  rea capture-process "$OUT/$TAG.$1.scenario.json" --format json > "$OUT/$TAG.$1.capture.json"
}
PRESCAN=${PRESCAN:-yes}
capture sm "$REPO/oracle/node_modules/.bin/sm" "$PRESCAN"
capture asm "$REPO/target/release/asm" "$PRESCAN"
rea compare-process-captures "$OUT/$TAG.sm.capture.json" "$OUT/$TAG.asm.capture.json" --format json > "$OUT/$TAG.compare.json"
echo "$OUT/$TAG.compare.json"
