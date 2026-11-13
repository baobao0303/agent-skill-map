#!/usr/bin/env bash
# P3 sidecars differential (refresh / annotate / prune) on claude-coverage.
# Usage: sidecars.sh <sm|asm> <workdir>
REPO=$(cd "$(dirname "$0")/../.." && pwd)
BIN=$([ "$1" = sm ] && echo "$REPO/oracle/node_modules/.bin/sm" || echo "$REPO/target/release/asm")
W=$2
rm -rf "$W" && cp -a "$REPO/conformance/fixtures/example" "$W" && rm -rf "$W/.skill-map"
cd "$W" || exit 1
export NO_COLOR=1 SKILL_MAP_TELEMETRY=0
step() {
  echo "### $*"
  "$BIN" "$@" </dev/null >"$W.o" 2>"$W.e"; local code=$?
  sed -E "s#$W#<W>#g; s/\"elapsedMs\":[0-9]+/\"elapsedMs\":0/" "$W.o"
  grep -v '^done in\|^$' "$W.e" | sed -E "s#$W#<W>#g" | sed 's/^/E| /'
  echo "exit=$code"
}
show() { echo "--- $1"; cat "$1" 2>&1; ls -l "$1" 2>/dev/null | cut -c1-10; }
step sidecars refresh AGENTS.md
"$BIN" scan >/dev/null 2>&1
step sidecars refresh nope.md
step sidecars refresh docs/STYLE.md
step sidecars refresh AGENTS.md
step sidecars refresh .claude/agents/content-editor.md
step sidecars refresh .claude/agents/content-editor.md --yes
show .claude/agents/content-editor.sm
"$BIN" scan >/dev/null 2>&1
step sidecars refresh .claude/agents/content-editor.md --json
step sidecars annotate docs/STYLE.md
show docs/STYLE.sm
step sidecars annotate .claude/agents/content-editor.md
step sidecars annotate .claude/agents/content-editor.md --force --json
show .claude/agents/content-editor.sm
step sidecars prune
mkdir -p ghost && printf 'identity: {}\n' > ghost/x.sm && printf 'a: 1\n' > lone.sm && touch keep && printf 'b: 1\n' > keep.sm
step sidecars prune
step sidecars prune -n
step sidecars prune -n --json
step sidecars prune --yes
step sidecars prune --json
