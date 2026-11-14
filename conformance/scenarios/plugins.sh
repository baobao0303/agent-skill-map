#!/usr/bin/env bash
# Project-local plugin runtime differential: trusted drop-in analyzers run
# for real (score-phase confidence ops, view contributions + payload schema
# rejections, manifest validation), on the spec fixtures plus variants that
# no fixture covers. Prints links/issues and stderr per case.
# Usage: plugins.sh <sm|agent-skill> <workdir>
REPO=$(cd "$(dirname "$0")/../.." && pwd)
BIN=$([ "$1" = sm ] && echo "$REPO/oracle/node_modules/.bin/sm" || echo "$REPO/target/release/agent-skill")
FIX="$REPO/oracle/node_modules/@skill-map/spec/conformance/fixtures"
W=$2
export NO_COLOR=1 SKILL_MAP_TELEMETRY=0
rm -rf "$W" && mkdir -p "$W"

variant() { # <name> <fixture> <plugin-relative index.js path>; body on stdin
  cp -a "$FIX/$2" "$W/$1" && cat > "$W/$1/.skill-map/plugins/$3"
}
for f in score-phase-confidence view-contribution-payloads plugin-missing-ui view-slots-all extension-enable-gate view-action-button; do
  cp -a "$FIX/$f" "$W/$f"
done
variant score-set-ceil score-phase-confidence link-scorer/analyzers/demote/index.js <<'JS'
export default {
  mode: 'deterministic',
  phase: 'score',
  evaluate(ctx) {
    for (const l of ctx.links) {
      ctx.adjustConfidence(l, { kind: 'set', value: 0.3 });
      ctx.adjustConfidence(l, { kind: 'delta', value: 0.25 });
      ctx.adjustConfidence(l, { kind: 'ceil', value: 0.5 });
    }
    return [
      { severity: 'warn', nodeIds: [ctx.nodes[0].path], message: 'from plugin', data: { target: 'x' } },
      { severity: 'loud', nodeIds: [], message: 'bad' },
    ];
  },
};
JS
variant bad-slot score-phase-confidence link-scorer/analyzers/demote/index.js <<'JS'
export default {
  mode: 'deterministic',
  phase: 'score',
  ui: { badge: { slot: 'inspector.header.badge.nope', label: 'x' } },
  evaluate() { return []; },
};
JS
variant bad-payloads view-contribution-payloads payloads-demo/analyzers/panels/index.js <<'JS'
const summary = { slot: 'inspector.body.panel.key-values', label: 'Summary' };
const dist = { slot: 'inspector.body.panel.breakdown', label: 'Distribution' };
export default {
  mode: 'deterministic',
  ui: { summary, dist },
  evaluate(ctx) {
    for (const node of ctx.nodes) {
      ctx.emitContribution(node.path, summary, { pairs: [{ key: '', value: [1], extra: true }], more: 1 });
      ctx.emitContribution(node.path, dist, { bars: 'nope' });
      ctx.emitContribution(node.path, { slot: 'x' }, {});
    }
    return [];
  },
};
JS

for d in $(ls "$W"); do
  echo "### $d"
  cd "$W/$d" || exit 1
  ids=$(ls .skill-map/plugins 2>/dev/null)
  [ -n "$ids" ] && "$BIN" plugins trust $ids >/dev/null 2>&1
  "$BIN" scan --json 2>"$W/.e" | python3 -c 'import json,sys
d=json.load(sys.stdin)
print(json.dumps({"links":[[l["source"],l["target"],l["confidence"]] for l in d["links"]],"issues":d["issues"],"nodes":len(d["nodes"])},sort_keys=True))'
  grep -v '^done in\|^$' "$W/.e" | sed 's/^/E| /'
done
