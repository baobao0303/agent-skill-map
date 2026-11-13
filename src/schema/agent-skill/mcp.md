# Manage the queue and findings over MCP (hybrid mode)

You have skill-map's MCP tools in this session, so you are in HYBRID mode.
MANAGE the queue and findings over these typed tools (no stdout parsing).
CLAIMING is per-runtime: on Claude Code the backgrounded CLI
`sm jobs claim --wait` parks for free, so PROCESS with the CLI loop in
`SKILL.md`. On a runtime that caps shell time (Codex kills an exec at
~10s; OpenCode's bash tool tops out at 10 minutes), claim with
`claim_job` + `wait` below, a server-side blocking claim you park on
(its progress heartbeat keeps timeout-resetting clients like OpenCode
parked indefinitely).

Queue:

- `list_extensions`: discover the finders / fixers you can run (id, kind,
  role). Use it before `submit_job` so you enqueue a real extension.
- `claim_job`: claim the next job (returns its id + nonce + rendered
  prompt). Pass `wait` (seconds) for a server-side BLOCKING claim that
  parks until a job arrives, the token-cheap alternative to the CLI
  `--wait` on a runtime that caps shell time. While parked the server
  emits a ~15s progress heartbeat, so a client that resets its request
  timeout on progress (OpenCode) parks indefinitely; on a fixed-timeout
  client set the per-tool timeout >= `wait`.
- `submit_job`: enqueue an extension on a node. Refused with a clear
  error when the `sm-process-jobs` skill is not installed (same
  no-processing-agent gate as the CLI / UI).
- `list_jobs` / `get_job`: inspect the queue (nonce stripped).
- `cancel_job` / `fail_job`: retire a queued or stuck job.
- `record_job`: close a claimed job (same id + nonce the CLI claim gave
  you); you MAY close with either `sm record` or `record_job`.

Findings:

- `list_findings`: read what a finder recorded, node-scoped or
  whole-project (optional `node`, `extension`, `includeStale`).
- `resolve_finding` / `reopen_finding`: flip a single finding's state
  (pure DB writes).
- `dismiss_finding` / `undismiss_finding`: dismiss or restore a finding
  or a whole class. The class-level writes touch the node's `.sm`
  sidecar, so they take `confirm` / `always` consent params; they
  succeed under a standing `allowEditSmFiles` grant and refuse cleanly
  otherwise. `delete_finding` hard-deletes a row and lifts its
  orphan-suppression under the same consent.

Everything here writes through the SAME engines and consent gate as the
CLI and BFF, so an MCP-driven change is indistinguishable from one made
at the shell or in the UI.
