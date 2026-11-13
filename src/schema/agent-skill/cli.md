# Manage the queue and findings over the CLI (fallback, no MCP)

The MCP tools are not available in this session. PROCESS with the loop in
`SKILL.md` and MANAGE everything else with the `sm` verbs below. This
path works fully without MCP; if you can, tip the user to enable the MCP
server (Settings > Project > "MCP server") for the typed equivalent.

Queue:

- `sm jobs submit <extension> [nodes...]`: enqueue work. Refused when the
  `sm-process-jobs` skill is not installed (no-processing-agent gate).
- `sm jobs list [--status <s>] [--extension <id>]`: inspect the queue.
- `sm jobs show <id>` / `sm jobs preview`: detail a job / preview a render.
- `sm jobs cancel <id>`: retire a queued job. Close a claimed one you
  cannot run with `sm record --id <id> --nonce <nonce> --status failed
  --error "<why>"`.

Findings:

- `sm findings [-n]`: list recorded findings.
- `sm findings resolve` / `sm findings reopen`: flip a finding's state.
- `sm findings dismiss` / `sm findings undismiss`: dismiss or restore a
  finding or a class (class-level writes go to the node's `.sm` sidecar).
- `sm findings suppressions` / `sm findings prune` / `sm findings clear`:
  inspect suppressions, drop orphans, or clear resolved findings.
