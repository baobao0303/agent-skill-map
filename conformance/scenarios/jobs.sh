#!/usr/bin/env bash
# P4 job-flow differential: agent install, submit, claim, record, findings,
# history, plus config / plugins read verbs. Ids, nonces, hashes, times masked.
# Usage: jobs.sh <sm|agent-skill> <workdir>
REPO=$(cd "$(dirname "$0")/../.." && pwd)
BIN=$([ "$1" = sm ] && echo "$REPO/oracle/node_modules/.bin/sm" || echo "$REPO/target/release/agent-skill")
FIX="$REPO/oracle/node_modules/@skill-map/spec/conformance/fixtures"
W=$2
rm -rf "$W" && mkdir -p "$W" && cp -a "$FIX/job-record-chain/." "$W/" && cp -a "$FIX/findings-contract/." "$W/"
cd "$W" || exit 1
export NO_COLOR=1 SKILL_MAP_TELEMETRY=0
mask() { sed -E "s#$W#<W>#g; s/[de]-[0-9]{8}-[0-9]{6}-[0-9a-f]{4}/<ID>/g; s/[0-9a-f]{32}/<NONCE>/g; s/[0-9a-f]{64}/<HASH>/g; s/\"(createdAt|claimedAt|finishedAt|expiresAt|startedAt|elapsedMs|durationMs|generatedAt|recordedAt|timestamp|at)\":[0-9]+/\"\1\":0/g; s/[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z/<TS>/g; s/in [0-9]+ms/in Nms/g"; }
step() {
  echo "### $*"
  "$BIN" "$@" </dev/null >"$W.o" 2>"$W.e"; local code=$?
  mask < "$W.o"; grep -v '^done in\|^$' "$W.e" | mask | sed 's/^/E| /'; echo "exit=$code"
}
"$BIN" scan >/dev/null 2>&1
[ "${TRUST:-0}" = 1 ] && "$BIN" plugins trust finder-fixture >/dev/null 2>&1 && "$BIN" scan >/dev/null 2>&1
step jobs submit ai-summarizer-action -n notes.md
step agent install --for claude
step jobs submit ai-summarizer-action -n notes.md
step jobs submit ai-summarizer-action -n notes.md --json
step jobs submit finder-fixture/redundancy-probe -n notes.md
step jobs list
step jobs status
step jobs claim --json
J=$("$BIN" jobs list --json | python3 -c 'import json,sys;print([j["id"] for j in json.load(sys.stdin) if j["status"]=="running"][0])')
N=$(sqlite3 .skill-map/skill-map.db "select nonce from state_jobs where id='$J'" 2>/dev/null || python3 -c "import sqlite3;print(sqlite3.connect('.skill-map/skill-map.db').execute(\"select nonce from state_jobs where id='$J'\").fetchone()[0])")
step record --id "$J" --nonce 00000000000000000000000000000000 --status completed --report report.json
step record --id "$J" --nonce "$N" --status completed --report report-invalid.json
J2=$("$BIN" jobs claim --json | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d["id"],d["nonce"])')
set -- $J2
step record --id "$1" --nonce "$2" --status completed --report findings-report.json
step findings
step findings --json
step history
step history --json
step history stats
step jobs list --json
step config list
step config get scan.maxScan
step config set scan.maxScan 10
step config get scan.maxScan
step plugins list
