# Portfolio handbook

A small static portfolio site, served by Express (`server.js`). The
`.claude/` harness maintains it: an agent writes the pages, a skill
checks the links, a command publishes them, to the live site and,
mirrored, to Notion pages via the Notion MCP. The conventions live in the
style guide; the deploy steps in the deploy runbook. The pages still to
build are tracked in [the backlog](./docs/BACKLOG.md).

- When a page needs writing or fixing, brief @content-editor.
- When the site is ready to go out, run /publish (it also mirrors the pages to Notion).
