# P1 differential verification with REA

Oracle: `@skill-map/cli@1.12.7` (`oracle/node_modules/.bin/sm`). Candidate: `target/release/asm`. Each pair runs in the same working directory (`oracle/runs/cmp`, reset from the fixture and pre-scanned by the binary under test) with the same environment, captured by `rea capture-process` and compared by `rea compare-process-captures`. Verbs run with `-q` so the elapsed-time trailer (`done in Nms`) is not printed. Reproduce with `rea/compare.sh <fixture> <verb args>`.

| Fixture | check | list | graph (ascii) | graph mermaid | graph dot |
|---|---|---|---|---|---|
| example | same | same | same | same | same |
| claude-coverage | same | BOM token (known) | same content* | same content* | same content* |
| codex-lens | same | same | same | same | same |
| antigravity-lens | same | same | same | same | same |
| opencode-lens | same | same | same | same | same |
| no-marker | same | same | same | same | same |

`check -q` with no database (example): same terminal, exit 5 on both sides.

"same" = REA terminal dimension `unchanged` and identical exit code. REA marks the exit dimension `changed` for most pairs only because the recorded settlement time differs by a few milliseconds; the exit codes are equal in every pair.

\* REA reports the terminal dimension `changed` because the two processes flush the same bytes in a different number of PTY frames (sm 2 frames, asm 1 to 3). Joining the frames gives byte-identical output.

Known divergence: `.claude/skills/bom/SKILL.md` counts 17 body tokens in sm and 16 in asm. gpt-tokenizer's pre-tokenizer treats U+FEFF as whitespace (JavaScript `\s`), tiktoken-rs does not.
