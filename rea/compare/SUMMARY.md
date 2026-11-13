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

# P2 differential verification (incremental scan, renames, watch)

Scenario scripts under `conformance/scenarios/` run the same edit sequence under both binaries in sibling work directories; `jsondiff.py` compares the ScanResult documents modulo `scannedAt`, `scannedBy`, `durationMs`, `modifiedAtMs` (file times differ between the two copies) and the known BOM token count. A raw byte comparison with the same fields masked was also run.

| Scenario | Result |
|---|---|
| `renames.sh`: scan → move / move+edit / delete / in-place edit → scan → scan → `scan --full` | all 4 documents byte-identical |
| `scan_extractor_runs` after the scenario | 219 rows, identical (ran_at excluded) |
| state migration: favorite on a renamed node + a stranded `ghost.md` row | same `state_node_favorites` rows, same stranded `orphan` issue |
| `--full --changed`, `--watch --full`, `--changed` without prior, zero-result wipe guard, `--allow-empty` | same stderr text and exit codes |
| `watch.sh`: `watch --json` while editing, renaming, editing | 4 ndjson batches byte-identical; same stderr |

REA (`rea/compare.sh`) on second scans (the cache-hit path): `check` terminal unchanged; `scan` / `scan --json` diverge only on the duration suffix, `scannedAt` and `scannedBy`.

Incremental semantics ported from the original, including its quirks: a cache-hit node reuses its prior links verbatim, so a link resolved against a since-deleted node keeps its stale `resolvedTarget` (lift never clears one); reused nodes and links keep the DB read-back key order; untouched files (same mtime) are claimed by their prior owner inside the current provider's walk; watcher batches walk only the changed paths and then re-inject the remaining prior nodes in prior order.

Not ported: the partial cache hit (some but not all of a node's extractors still valid) re-extracts the node fully; it arises only when one extractor's settings change.

# P3 differential verification (sidecar writes, issue suppressions)

| Scenario | Steps | Result |
|---|---|---|
| `bump.sh` (example): flag mutexes, disabled extension, unknown node, consent refusal, `--yes`, fresh refusal, `--force`, `--json`, `--pending`, `--pending --json`, `--staged` outside git | 18 | identical transcript incl. `.sm` bytes (timestamps masked) and 0600 modes |
| `sidecars.sh` (example): `refresh` / `annotate` / `prune` incl. consent, `--force`, `--json`, dry-run, non-TTY confirm | 16 | identical |
| `orphans.sh` (claude-coverage): list, `--kind`, `--json`, `reconcile` (incl. dry-run and a PK collision), `undo-rename` (mismatch, non-TTY prompt, dry-run, `--force`), DB state after each | 15 | identical |
| `issues.sh` (claude-coverage): `dismiss` / `undismiss` / `suppressions`, unknown analyzer, missing node, consent, notes, duplicates, `check` after dismissal, rescan, operations.log entries | 18 | identical |

The consent grant (`scope.lock.json`, sha256 over the `.skill-map/` inode and birth time) is interoperable: a grant minted by `sm` is accepted by `asm` in the same checkout.

# Spec conformance suite (original runner)

`node conformance/run-spec.mjs <sm|asm> [case...]` drives the 51 spec-owned cases of `@skill-map/spec` with the ORIGINAL runner (`@skill-map/cli/dist/conformance`); `conformance/asm-shim.cjs` lets it spawn the Rust binary. Baseline: `sm` 51/51, `asm` 24/51 (after agent + jobs). The remaining failures are verbs not yet ported (`agent`, `jobs`, `record`, `findings`, `history`, `plugins`, `config`, `serve`, `conformance`, `enrich`) and the drop-in JS plugin runtime.
