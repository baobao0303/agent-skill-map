# skflow — Spec dịch ngược từ `skill-map` (để port sang Rust)

> **Mục đích:** tài liệu đặc tả đầy đủ để viết lại `skill-map` bằng Rust (dự án `skflow`).
> **Nguồn:** `crystian/skill-map` (MIT) — repo GitHub + gói npm đã ship.
> **Ngày lập:** 2026-11-13.

---

## 0. Nguồn gốc & độ tin cậy (provenance)

| Nguồn | Phiên bản | Cách dùng | Độ tin cậy |
|---|---|---|---|
| `@skill-map/cli` (npm tarball, đã ship) | **1.12.7** | REA `analyze_javascript_application` (static, không chạy code) trên `bin/sm.js`, `dist/cli.js`, `dist/index.js`, `dist/kernel/index.js`; grep tĩnh danh sách command path trong bundle | `shipped-artifact` — thực tế đang chạy |
| `@skill-map/spec` (npm) | 1.12.7 | Gói chứa JSON Schemas + prose contracts | normative |
| Repo GitHub `crystian/skill-map` (shallow clone, HEAD `0cd3a5b`) | — | Đọc `spec/*.md`, `spec/schemas/**`, `src/migrations/001_initial.sql`, `src/config/defaults.json`, `src/plugins/**` | `historical-reference` |

**REA evidence:** `ev_56894f0bf6177dc89f7e4cd038a100da13e30aa736935043d3a071950a6dff73` (summary view) → parent `ev_f038dfe8…aaa2`.
- Artifact root sha256: `cd3bf2f18664fbfa26abe1b4532bef8ac51a4e4c7b52a3312c1fc1f5870a1476`
- 101 file liên quan, 4 file JS được parse (954 KB text, 104 993 AST node), 0 lỗi parse, không có Electron/IPC/native addon.
- Coverage của REA là **partial** (semantic graph bị cắt ở 20 000 node/file): REA chỉ dùng để xác nhận *hình dạng* artifact (entrypoints, chunk, dependency, verb set). Mọi hợp đồng hành vi chi tiết dưới đây lấy từ `spec/` — chính tác giả tuyên bố `spec/` là *source of truth*, đủ để viết implementation thay thế.

**Verb set xác nhận từ bundle 1.12.7** (khớp 100% với `spec/cli-contract.md`, thêm 1 verb ẩn `intentional-fail` dùng để test):
```
actions {list,show} · activity {install,status,uninstall} · agent {install,status,uninstall}
bump · check · config {get,list,reset,set,show} · conformance run
db {backup,browser,dump,migrate,reset,restore,shell} · doctor · enrich · example · export
findings [clear,dismiss,prune,reopen,resolve,suppressions,undismiss] · graph · help
history [stats] · hooks install · init · issues {dismiss,suppressions,undismiss}
jobs {cancel,claim,fail,list,preview,prune,show,status,submit} · list
orphans [reconcile,undo-rename] · plugins {config,create,disable,doctor,enable,list,show,slots list,trust,untrust,upgrade}
record · scan [compare-with] · serve · show · sidecars {annotate,prune,refresh}
tutorial · version · watch
```

**Stack gốc (để biết cần thay gì):** Node ≥ 24, TypeScript 6, `node:sqlite` + Kysely, Hono (HTTP), `ws`, `@modelcontextprotocol/sdk`, `ajv` (JSON Schema 2020-12), `gpt-tokenizer` (`cl100k_base`), `chokidar` / `@parcel/watcher`, `js-yaml` 5, `smol-toml`, `zod`, Sentry + PostHog (telemetry opt-in). UI: Angular + PrimeNG + Foblex Flow.

---

## 1. Sản phẩm là gì

`skill-map` quét một project chứa hệ sinh thái Markdown của AI agent (skills, agents, commands, hooks, notes — cho Claude Code, Codex, Antigravity, OpenCode, Agent Skills open standard) và dựng **một đồ thị**:

- **Node** = một file (hoặc node ảo như `mcp://<server>`), có kind, provider, frontmatter, byte/token weight.
- **Link** = quan hệ có hướng giữa node (`invokes`, `references`, `mentions`, `points`) với `confidence ∈ [0,1]`.
- **Issue** = phát hiện tất định (deterministic) từ analyzer: broken ref, trùng tên, tên bị runtime chiếm, frontmatter sai…
- **Finding** = phát hiện xác suất (LLM) do *agent bên ngoài* xử lý qua hàng đợi job.

Hai chế độ thực thi, tách biệt tuyệt đối:
1. **Deterministic** — chạy trong `sm scan` / `sm check`: nhanh, offline, tái lập được, an toàn cho CI.
2. **Probabilistic** — không bao giờ chạy trong scan. Kernel render prompt thành job trong DB; agent của người dùng `claim` → thực thi bằng model của nó → `record` kết quả. **skill-map không bao giờ gọi LLM.**

---

## 2. Phạm vi port & lộ trình đề xuất cho Rust

Bản gốc rất lớn (~222k dòng code, 79 verb, 68 extension). Đề xuất port theo pha; mỗi pha tự đứng được và có test conformance.

| Pha | Nội dung | Mục tiêu "xong" |
|---|---|---|
| **P1 — Scan core** | FS walker + ignore, frontmatter parser (YAML/TOML), 6 provider built-in, 9 extractor, normalization, resolver + confidence, ~12 analyzer tất định, formatter ascii/json/mermaid/dot, `scan`/`check`/`list`/`show`/`graph`, DB `scan_*` | `sm scan --json` cho ra `ScanResult` hợp lệ theo `scan-result.schema.json` và khớp bản Node trên cùng fixture |
| **P2 — Incremental & watch** | `scan_extractor_runs` cache, rename heuristic, `watch` (notify crate), `scan_meta` drift | `sm scan` lần 2 không đọc lại file không đổi |
| **P3 — Sidecar `.sm`** | Đọc/ghi YAML sidecar, identity hash, bump, consent gate, issue suppressions | `.sm` do bản Rust ghi được bản Node đọc và ngược lại |
| **P4 — Job queue** | `state_jobs`, prompt preamble, submit/claim/record, findings, summaries | Một agent Claude Code xử lý được job qua `sm jobs claim --json` + `sm record` |
| **P5 — Server** | HTTP BFF (axum) + WebSocket + MCP Streamable HTTP; phục vụ UI Angular gốc nguyên vẹn | UI gốc chạy trên backend Rust |
| **P6 — Plugin & activity** | Plugin bên thứ ba, live activity hooks, session journal | xem §22 về quyết định plugin runtime |

**Ngoài phạm vi khuyến nghị:** telemetry (Sentry/PostHog), update-check npm, `sm tutorial`/`sm example` nội dung (có thể nhúng sau), `db browser`.

---

## 3. Kiến trúc (hexagonal)

```
Driving adapters:   CLI (sm)   ·   Server (HTTP BFF + WS + MCP)   ·   Skill (agent)
                         │                │
                         ▼                ▼
Kernel (domain-pure):  Registry · Orchestrator · Use cases (scan/refresh/action/watch) · Config layering
                         │
Driven adapters:     StoragePort (SQLite) · FilesystemPort · PluginLoaderPort · ProgressEmitterPort · LoggerPort
Plugins (6 kind):    providers · extractors · analyzers · actions · formatters · hooks
```

**Bất biến bắt buộc:**
- Kernel **không** import trực tiếp FS / DB / subprocess; mọi IO đi qua port.
- Kernel **không** chứa nhánh theo platform (`if provider == "claude"`). Mọi routing là cấu trúc (kind, `$id` prefix schema, field manifest).
- **Boot invariant:** gỡ hết extension → kernel vẫn boot và trả graph rỗng (conformance case `kernel-empty-boot`).
- Kernel không ghi stdout/stderr; tất cả qua `LoggerPort` (levels `trace < debug < info < warn < error < silent`).
- **Extension không bao giờ ghi filesystem hay gọi network.** Ngoại lệ duy nhất: Action khai `io: ['network']` nhận `ctx.fetch` inject, chỉ chạy qua `sm enrich`, và bị chặn khi `allowNetworkActions=false`.

### 3.1 Ports

| Port | Operations |
|---|---|
| `StoragePort` | repository có kiểu cho 3 zone `scan_*`, `state_*`, `config_*`; transaction cho atomic claim; migration với `PRAGMA user_version` |
| `FilesystemPort` | `walk(roots, ignore)`, `readNode(path)`, `stat(path)`, `writeJobFile`, `ensureDir` |
| `PluginLoaderPort` | `discover(scopes)`, `load(path)`, `validateManifest(json)` |
| `ProgressEmitterPort` | `emit(event)`, `subscribe(listener)` — event định nghĩa ở `job-events.md` |
| `LoggerPort` | `trace/debug/info/warn/error(message, context?)`; record có `level`, `timestamp` ISO 8601, `message`, `context` |

**Không có RunnerPort.** (Đã bị xoá 2026-07-13 — skill-map là bản đồ + hàng đợi, agent là executor.)

---

## 4. Layout trên đĩa

```
<project>/
├── .skillmapignore                 # gitignore syntax, layer cao nhất của ignore
├── **/*.md, *.toml                 # nội dung được quét
├── **/<name>.sm                    # sidecar YAML cạnh file .md (human curation, COMMIT)
└── .skill-map/
    ├── .gitignore                  # do `sm init` ghi; KHÔNG đụng .gitignore gốc
    ├── settings.json               # layer `project` (commit)
    ├── settings.local.json         # layer `project-local` (gitignore)
    ├── skill-map.db                # SQLite (machine output, tái tạo được)
    ├── scope.lock.json             # trust plugin + grant khoá local (gitignore, KHÔNG portable)
    ├── views/<slug>.json           # named map views (commit)
    ├── sessions/<id>.json          # session journal của activity (gitignore)
    └── plugins/<id>/plugin.json    # drop-in plugins
~/.skill-map/settings.json          # ngoại lệ duy nhất đọc $HOME: update-check toggle, KHÔNG merge vào config
```

**Storage rule** (quyết định "lưu ở đâu" cho mọi bề mặt persisted):

| Loại | Nơi lưu | Ví dụ |
|---|---|---|
| Machine output (tái tạo được) | DB | findings, summaries, executions, jobs, enrichments |
| Human curation (portable) | `.sm` sidecar | tags, stability, version, audit, suppressions |
| Machine-local authorization | `scope.lock.json` | plugin trust, grant cho key nhạy cảm |
| Project-level shared curation | `.skill-map/views/` | map views |
| Per-session journal | `.skill-map/sessions/` | activity đã record |

**Scope luôn project-local**: không có `-g/--global`, không đọc `$HOME` mặc định.

---

## 5. Config

### 5.1 Layering (thấp → cao, deep-merge theo dot-path; object đệ quy, array thay thế)

| # | Layer | Nguồn |
|---|---|---|
| 1 | `defaults` | `defaults.json` nhúng trong binary |
| 2 | `project` | `.skill-map/settings.json` |
| 3 | `project-local` | `.skill-map/settings.local.json` |
| 4 | `override` | env var, CLI flag |

Loader ghi lại `sources` map (key → layer cuối cùng ghi) cho `sm config show --source`.

### 5.2 `defaults.json` (nguyên văn từ bản gốc)

```json
{
  "schemaVersion": 1,
  "allowEditSmFiles": false,
  "allowNetworkActions": false,
  "allowSidecarWriters": true,
  "tutorialReminderStep": 0,
  "tokenizer": "cl100k_base",
  "server": { "port": 4242, "host": "127.0.0.1" },
  "roots": [],
  "ignore": [],
  "scan": {
    "tokenize": true, "strict": false,
    "maxFileSizeBytes": 1048576, "maxScan": 5000, "maxNodes": 256,
    "watch": { "debounceMs": 300, "backend": "chokidar" },
    "referencePaths": [], "followExternalSymlinks": false, "respectGitignore": false
  },
  "plugins": {},
  "skillActions": { "enabled": false },
  "jobs": {
    "claimWaitSeconds": 2, "perExtensionTtl": {}, "perExtensionPriority": {},
    "retention": { "completed": 2592000, "failed": null, "cancelled": 2592000 }
  }
}
```

### 5.3 `PROJECT_LOCAL_ONLY_KEYS` (bảo mật — bắt buộc port đúng)

Các key này **bị strip (kèm warning) nếu xuất hiện trong `settings.json` commit**, và cũng bị strip khỏi `settings.local.json` nếu không có grant trong scope lock (vì `git add -f` có thể ship file local):

`allowEditSmFiles`, `allowNetworkActions`, `logLevel`, `scan.referencePaths`, `scan.followExternalSymlinks`, `activity.captureConversations`, `tutorialReminderStep`, `ui.liveUpdates`, `ui.realtimeActivity`, `ui.showRuntimeAgents`, `mcp.server.enabled`, `plugins.github.extensions.enrichment.settings.apiBaseUrl`, `…rawBaseUrl`.

Lý do: repo clone về độc hại không được phép tự mở rộng quyền truy cập đĩa/network/token của nạn nhân.

`sm config set` với key mở rộng bề mặt ra ngoài project root (`scan.referencePaths` ngoài root, `scan.followExternalSymlinks=true`) **bắt buộc `--yes`**, thiếu thì exit 2.

### 5.4 Extension settings
Giá trị ở `plugins.<pluginId>.extensions.<extId>.settings.<settingId>`. Resolve: manifest `default` → giá trị config merged → (với `secret` có `envVar`) env var không rỗng thắng → validate theo input-type; sai thì fallback default + warning.

---

## 6. Domain model (JSON shape — wire format phải giữ nguyên để UI gốc chạy được)

Tất cả là JSON Schema draft 2020-12, `$id` prefix `https://skill-map.ai/spec/v1`. Khuyến nghị Rust: `serde` struct + `#[serde(rename_all = "camelCase")]`, validate bằng `jsonschema` crate với schema gốc nhúng qua `include_str!`.

### 6.1 `Node`
Required: `path, kind, provider, bodyHash, frontmatterHash, bytes, linksOutCount, linksInCount, externalRefsCount`.

| Field | Kiểu | Ghi chú |
|---|---|---|
| `path` | string | scope-root-relative, forward slash; định danh canonical |
| `kind` | string | vd `agent`, `command`, `skill`, `workflow`, `markdown`, `mcp` |
| `provider` | string | id provider đã classify |
| `frontmatter` | object | **bất biến**, đúng như tác giả viết |
| `bodyHash`, `frontmatterHash` | string | sha256 hex (xem §12.1) |
| `bytes` | `{frontmatter, body, total}` | |
| `tokens` | `{frontmatter, body, total}` | optional; `cl100k_base` |
| `modifiedAtMs` | int\|null | |
| `linksOutCount`, `linksInCount`, `externalRefsCount` | int | |
| `externalRefs` | array | URL http(s) được tách khỏi links |
| `sidecar` | object | overlay `{present, status, annotations, …}` |
| `isFavorite` | bool | |
| `contributions` | array | view contributions |
| `tags` | string[] | chỉ wire shape (từ `sidecar.annotations.tags`) |
| `body` | string\|null | chỉ khi request `include=body` |
| `virtual` | bool | node ảo (vd `mcp://server`) |
| `derivedFrom` | string[] | file nguồn của node ảo |

### 6.2 `Link`
Required: `source, target, kind, confidence, sources`.

| Field | Ghi chú |
|---|---|
| `source`, `target` | path; `target` đã normalize POSIX tương đối với `dirname(source)` khi là path |
| `kind` | enum đóng: `invokes` \| `references` \| `mentions` \| `points` |
| `confidence` | `[0,1]` — sau post-walk luôn bắt đầu từ baseline 1.0 (§11) |
| `sources` | string[] — id extractor đã phát ra (union khi dedup) |
| `trigger` | `{originalTrigger, normalizedTrigger}` \| null |
| `location` | `{line, column, offset}` \| null — `line` 1-indexed, tính cả frontmatter |
| `raw` | string\|null |
| `occurrences` | array; mỗi entry có thể có `context: 'inline-code' \| 'code-block'` |
| `resolvedTarget` | string\|null — path node mà link resolve tới |

Link kind ý nghĩa:
- `invokes` — gọi chạy (`/command`, `$skill`)
- `references` — markdown link `[x](y.md)`, `@file`
- `mentions` — nhắc tên `@handle`
- `points` — path `.md` viết trong backtick/code block

### 6.3 `Issue`
Required: `analyzerId, severity, nodeIds, message`. Fields: `severity ∈ {error, warn, info}`, `nodeIds[]`, `linkIndices[]`, `detail`, `fix`, `data` (object; `data.target` dùng cho suppression).

### 6.4 `ScanResult`
Required: `schemaVersion, scannedAt, roots, nodes, links, issues, stats`.
Thêm: `scannedBy {name, version, specVersion}`, `providers[]`, `tokenizer`, `activeProvider`, `scanCeiling`, `scanTruncated`, `maxRenderNodes`, `oversizedFiles[{path, bytes}]`.
`stats`: `filesWalked, filesSkipped, filesOversized, nodesCount, linksCount, issuesCount, durationMs`.

`sm scan --json`, `sm graph --format json`, `GET /api/scan` phải **byte-equivalent modulo whitespace**.

### 6.5 Frontmatter base
`spec/schemas/frontmatter/base.schema.json`: properties `name`, `description` (không required ở base; mỗi kind schema `allOf` + `$ref` base và tự đặt required).

---

## 7. Providers (6 built-in)

Provider = extension nhận diện platform: classify `path → kind`, khai kind catalog, resolution matrix, reserved names.

### 7.1 Active Provider Lens
- Mỗi project có **đúng một lens** active, lưu ở `settings.json` key `activeProvider` (+ snapshot `activeProviderMarkers`).
- Chưa set → auto-detect theo marker do provider khai (`detect.markers`):

| Provider | Marker | Ghi chú |
|---|---|---|
| `claude` | `.claude/` | stable |
| `codex` | `.codex/` | beta (`AGENTS.md` **không** phải marker) |
| `antigravity` | `.agent/workflows/` | beta |
| `opencode` | `.opencode/` | beta, `subsumes: ['claude']` |
| `agent-skills` | `.agents/` | stable, `detect.fallback: true` — chỉ thắng khi không có vendor marker; là lens mặc định khi không có marker nào (không persist) |
| `core/markdown` | — | không phải lens; fallback phổ quát |

- Nhiều vendor marker → hỏi user (CLI prompt / UI), trừ khi một provider `subsumes` cái kia.
- Đổi lens → **drop toàn bộ zone `scan_*` atomically** rồi scan lại. `state_*`, `config_*`, `.sm` giữ nguyên.
- Drift: mỗi scan so marker hiện tại với snapshot, phát **một** warning "New: …"/"Removed: …"; không tự đổi lens.
- `gatedByActiveLens: true` (vendor + agent-skills): classify chỉ chạy khi `provider.id == activeProvider`. `core/markdown` không gated.

### 7.2 Dispatch order & first-claim-wins
Thứ tự: `claude → antigravity → codex → opencode → agent-skills → (plugin người dùng) → core/markdown` (cuối cùng).
- Mỗi path chỉ được offer cho tối đa 1 provider (per-scan `Set<path>`); provider đầu tiên trả non-null thắng.
- `core/markdown` claim mọi `.md` chưa ai claim, trả `markdown` vô điều kiện.
- File không đổi (mtime khớp) giữ cặp `(provider, kind)` cũ; lens đổi (`scan_meta.active_provider` khác) → rebuild toàn bộ.

### 7.3 Bảng classify / identifiers / resolution

| Provider | read | classify (path lowercase) | Kind → identifiers | resolution | reservedNames |
|---|---|---|---|---|---|
| `claude` | `.md`, `frontmatter-yaml` | `.claude/agents/**` → `agent`; `.claude/commands/**` → `command`; `^\.claude/skills/[^/]+/skill\.md$` → `skill` | agent, command: `[frontmatter.name, filename-basename]`; skill: `[frontmatter.name, dirname]`; mcp: `[frontmatter.name]` | `mentions: [agent, skill, markdown]`, `invokes: [command, skill]` | command, skill: `help clear compact cost init model agents login logout mcp memory config doctor permissions add-dir bug pr-comments release-notes review terminal-setup vim output-style hooks install-github-app migrate-installer upgrade resume exit quit security-review statusline usage feedback`; agent: `general-purpose, output-style-setup, statusline-setup` |
| `codex` | `.toml` (`toml`, `bodyField: developer_instructions`) rồi `.md` | `.codex/agents/*.toml` → `agent`; `.agents/skills/*/SKILL.md` → `skill` | `[frontmatter.name, filename-basename]` | `invokes: [skill]` | không có |
| `agent-skills` | `.md` | `^\.agents/skills/[^/]+/skill\.md$` → `skill` | `[frontmatter.name, dirname]` | `invokes: [skill]` | không có |
| `antigravity` | `.md` | `^\.agent/workflows/[^/]+\.md$` → `workflow` + kind `skill` của agent-skills | workflow: `[filename-basename]` | `invokes: [skill, workflow]` | `COMMONS_RESERVED_NAMES` + `goal`… dưới `skill` và `workflow` |
| `opencode` | `.md` | `.opencode/agent/*.md` → agent; `.opencode/commands/*.md` → command; `.opencode/skills/*/SKILL.md`, `.claude/skills/*/SKILL.md` → skill | `[filename-basename]` | `invokes: [command]` | — |
| `core/markdown` | `.md` | mọi `.md` còn lại → `markdown` | `[filename-basename]` | — | — |

`invocationSigil`: `/` cho claude/antigravity/opencode, `$` cho codex.

Mỗi kind có `ui: {label, color, colorDark?, emoji?, icon?}` (vd claude agent `#3b82f6`/`#60a5fa` `pi-user`; command `#f59e0b`; skill `#10b981` `pi-bolt`; mcp `#8b5cf6` `pi-server`). UI gốc đọc qua `kindRegistry` / `providerRegistry` trong REST envelope — **bắt buộc giữ** nếu muốn tái sử dụng UI.

### 7.4 Identifier sources (đóng)
- `frontmatter.name` → `node.frontmatter.name`
- `filename-basename` → `basename(path)` bỏ extension
- `dirname` → `basename(dirname(path))`

Absent ≡ `[]` → node chỉ resolve được qua path-match.

### 7.5 MCP config discovery (`mcpConfig`)
Provider khai `{ sources: [{ path, dialect: 'json-mcp-servers' | 'toml-mcp-servers' }] }` (vd `.claude/settings.json`, `.codex/config.toml`, `opencode.json`). Kernel parse, chấp nhận top-level key `mcpServers` / `mcp_servers` / `mcp`; với mỗi server phát node ảo `mcp://<server>` (`virtual: true`, `derivedFrom: [config path]`, metadata vào `frontmatter`). `enabled: false` (OpenCode) → không phát.

---

## 8. Scan pipeline (thứ tự bắt buộc)

```
1. Load config (4 layer) → resolve activeProvider (+ drift warn)
2. Build ignore filter: bundled defaults → .gitignore (nếu scan.respectGitignore) → config.ignore → .skillmapignore  (layer sau có thể `!` re-include)
3. Walk roots theo read.extensions của từng provider được phép
     - bỏ file > scan.maxFileSizeBytes (ghi vào oversizedFiles, không đọc)
     - dừng ở scan.maxScan file (scanTruncated = true)
     - symlink: theo nếu realpath nằm trong một scan root; ngoài root → từ chối (trừ scan.followExternalSymlinks)
4. Với mỗi file: provider.classify → (provider, kind) (first-claim-wins)
5. node-build: parse frontmatter (KHÔNG throw), tính hash, bytes, tokens,
     kernel-stamp: frontmatter-parse-error / frontmatter-malformed / frontmatter-invalid, backtick-unbalanced
6. Load sidecar .sm cạnh file → overlay annotations, drift status
7. MCP config discovery → node ảo
8. Extract: với mỗi node × extractor thoả precondition (kind, provider = active lens) → emitLink / emitSignal / enrichNode / emitContribution
     (incremental cache: bỏ qua nếu body hash + sidecar hash + settings hash khớp)
9. Signal resolver: rank candidate, overlap cluster, materialise Link
10. dedupeLinks (key gồm kind) → partition URL ra externalRefs
11. liftResolvedLinkConfidence: baseline 1.0, set resolvedTarget, tính reservedNodePaths / brokenLinks
12. Code-region resolution gate: xoá link mentions/invokes không resolve mà mọi occurrence đều trong code
13. Analyzers theo phase: score → detect → aggregate (fold confidence ops sau phase score)
14. Drop issue bị dismiss (annotations.issueSuppressions khớp (analyzer, data.target))
15. Validate output với scan-result.schema.json
16. Persist replace-all vào scan_* (một transaction) + scan_meta
```

---

## 9. Extractors

Contract: `extract(ctx) → void`, chỉ deterministic. Output qua callback:
- `ctx.emitLink(link)` — kind phải thuộc enum đóng, sai → drop + `extension.error` event.
- `ctx.emitSignal(signal)` — candidate có thể mơ hồ, để resolver quyết.
- `ctx.enrichNode(partial)` — ghi lớp enrichment (`node_enrichments`), **không bao giờ** sửa frontmatter tác giả.
- `ctx.emitContribution(id, payload)` — view slot cho UI.
- `ctx.store` — KV riêng plugin (nếu manifest khai `storage`).

`precondition: { kind?: string[], provider?: string[] }` → lọc trước khi tạo ctx. Gate là **active lens**, không phải provider của node.

### 9.1 Strip policy
Mọi extractor *prose* chạy trên body đã xoá: fenced code block, inline code span, raw HTML (comment + tag token; **không** xoá nội dung giữa tag). Các extractor *code-region* chạy trên **inverse mask**: cùng độ dài, ký tự ngoài code bị blank → giữ nguyên offset/line.

### 9.2 Bảng extractor built-in

| Id | Gate (lens) | Miền | Phát | Confidence emit |
|---|---|---|---|---|
| `core/markdown-link` | mọi lens | prose | `references` (bỏ image `![]`, scheme `http:`/`mailto:`/`tel:`/`data:`, `#anchor`; strip `#frag`/`?query`) | 0.95 |
| `core/backtick-path` | mọi lens | code | `points` (chỉ `.md`), Signal context `inline-code`/`code-block` | 0.85 |
| `core/external-url-counter` | mọi lens | prose | đếm URL http(s) khác nhau → `externalRefsCount` | — |
| `core/mcp-tools` | mọi lens | frontmatter `tools: [mcp__<server>__<tool>]` | node ảo `mcp://<server>` + link | — |
| `core/slash-command` | claude, antigravity, opencode | prose | `invokes` | 0.8 |
| `core/backtick-slash` | claude, antigravity, opencode | code | `invokes` (qua resolution gate) | 0.8 |
| `core/at-file` | codex, antigravity, claude | prose | `references` cho `@file` dạng path/extension | — |
| `claude/at-directive` | claude | prose | `mentions` cho `@handle`; `@foo.md`/`@./x` → reference path | bare handle 0.5; path-style 0.85 |
| `claude/backtick-mention` | claude | code | `mentions` (qua resolution gate) | 0.5 |
| `claude/tools-counter` | claude | frontmatter | contribution đếm tools của agent | — |
| `codex/dollar-skill` | codex | prose | `invokes` cho `$skill` | 0.8 |
| `codex/backtick-dollar` | codex | code | `invokes` (qua resolution gate) | 0.8 |

> Lưu ý: confidence lúc emit **bị bỏ** ở post-walk; kernel đặt baseline 1.0 cho mọi link rồi analyzer score-phase trừ penalty (§11).

### 9.3 Token grammars (pin — phải khớp chính xác)

```text
# slash  (kernel/util/slash-token.ts)
/(?<![A-Za-z0-9_/.:?#=&])(\/(?=[0-9_-]*[a-z])[a-z0-9][a-z0-9_-]*(?::(?=[0-9_-]*[a-z])[a-z0-9][a-z0-9_-]*)?)/gi
  + post-match path-suffix guard: loại `/api/v1/items`

# at  (kernel/util/at-token.ts)
/(?:^|[^A-Za-z0-9_@])(@(?=[0-9_\-./:]*[a-z])(?:(?:\.{1,2}\/)+|\/)?\.?[a-z0-9](?:[a-z0-9_\-./]*[a-z0-9_])?(?::[a-z0-9][a-z0-9_-]*)?)/gi
  file-shaped (@foo.md, @./x, @../x) → không phải mention; @/abs/x → bỏ qua

# dollar  (kernel/util/dollar-token.ts)  — không flag i: chữ đầu phải thường ($PATH, $5 không khớp)
/(?<![A-Za-z0-9_$])(\$[a-z][a-z0-9_-]*)/g

# backtick-path  (spec/architecture.md §Extractor · code-region file references)
/(?<![\w/:.-])(?:\.{1,2}\/)*\.?[\w][\w.-]*(?:\/[\w.-]+)*\.md\b(?![\w/])/g
```

Quy tắc chung: token phải có ít nhất một chữ cái (`@10`, `/10` không khớp).

**⚠ Rust:** crate `regex` **không hỗ trợ lookbehind/lookahead**. Hai lựa chọn: (a) `fancy-regex` (dễ port, chậm hơn); (b) viết scanner tay cho 4 grammar (nhanh, nên làm, nhưng phải có test bảng đối chiếu từ test suite gốc `src/kernel/util/__tests__`). Khuyến nghị: bắt đầu bằng `fancy-regex`, giữ test golden, rồi tối ưu.

### 9.4 Markdown link resolution
- Bare relative (`docs/x.md`, `./x.md`, `../x.md`) → POSIX-normalize theo `dirname(node.path)`; **không** root fallback.
- Leading `/` (`/docs/x.md`) → relative scan root; `/` hoặc `/..` thoát root → không phát link.
- Dedup per-node theo target đã resolve.

### 9.5 Backtick-path resolution
Target = normalize theo `dirname(node.path)`; `trigger.originalTrigger` = token nguyên văn. Post-walk: nếu file-relative không khớp node nào **và** token không có prefix `./`/`../` → thử token normalize theo scan root.

### 9.6 Code-region resolution gate (normative)
Sau dedup + resolve: xoá mọi link `mentions`/`invokes` (a) không resolve và (b) **mọi** occurrence có `context` code. Link có ít nhất một occurrence prose → giữ (và sẽ bị flag broken nếu không resolve). `references`/`points` nằm ngoài gate.

### 9.7 Signal IR
`signal.schema.json`. Resolver: rank candidate theo `resolverRules.kindPriority` → confidence DESC → độ dài range DESC → thứ tự khai extractor. Overlap cluster theo range: loser → `outcome: rejected` + `rejectedBy {source, range, extractorId, reason ∈ kind-priority|higher-confidence|longer-range|earlier-declaration}`. Cluster toàn URL http(s) không xếp hạng chéo. Signal materialised → Link (sources = extractorId thắng). `core/extractor-collision` phát `warn` cho mỗi Signal bị reject.

---

## 10. Trigger normalization (normative, byte-identical mọi platform)

1. Unicode **NFD**
2. Bỏ mọi code point category **Mn**
3. Lowercase (locale-independent)
4. Thay `-`, `_`, và mọi run whitespace (space, tab, newline, NBSP…) bằng một space ASCII
5. Gộp nhiều space liên tiếp
6. Trim

Ký tự khác (`/ @ : .`) **giữ nguyên**.

| original | normalized |
|---|---|
| `Hacer Review` | `hacer review` |
| `hacer-review` / `hacer_review` | `hacer review` |
| `  hacer   review  ` | `hacer review` |
| `Clúster` | `cluster` |
| `/MyCommand` | `/mycommand` |
| `@FooExtractor` | `@fooextractor` |
| `skill-map:explore` | `skill map:explore` |

Rust: `unicode-normalization` (`.nfd()`), `unicode-general-category` hoặc `unicode_categories` để lọc Mn, `str::to_lowercase()` (locale-independent sẵn).

---

## 11. Resolution & confidence

### 11.1 Hai bước
1. **Post-walk lift** (`liftResolvedLinkConfidence`, chạy sau `dedupeLinks`, trước analyzer): `confidence = 1.0` cho **mọi** link, set `resolvedTarget`, tính `reservedNodePaths`, `brokenLinks`.
2. **Score-phase analyzer** áp penalty qua `ctx.adjustConfidence(link, op)`.

### 11.2 Quy tắc resolve
1. **Path match (mọi kind):** `link.target == node.path` → resolved. Root fallback chỉ cho `points` (xem §9.5).
2. **Name match (link có `trigger.normalizedTrigger`):** bỏ sigil `@`/`/`/`$` → tra **name index** (xây từ identifiers mọi node, đã normalize). Dùng `resolution[link.kind]` của **active lens**; duyệt mảng theo thứ tự ưu tiên; ưu tiên candidate **không reserved** (non-reserved kind thấp hơn vẫn thắng reserved kind cao hơn).
3. **Virtual target** → vẫn resolved, giữ 1.0.
4. **Broken** = không khớp path, không khớp tên nào trong index, **và** (với link dạng path, không sigil) file không tồn tại trên đĩa dưới bất kỳ scan root nào (probe tồn tại, không đọc; symlink entry tính là tồn tại). Bỏ qua nếu nằm trong `scan.referencePaths` hoặc khớp setting `ignored-references` (`literal` | `regex` | `glob`) của `core/reference-broken`.

Trường hợp `not-broken + not-resolved` (tên tồn tại nhưng sai kind cho link.kind) → giữ 1.0, không detector built-in nào chạm.

### 11.3 Confidence ops (fold tất định)
`op ∈ {set, delta, ceil, floor}`. Sort ops theo `(pluginId, extensionId)`. Từ baseline 1.0: `set` (cái cuối thắng) → cộng mọi `delta` → `floor` (nâng) → `ceil` (hạ) → **clamp [0,1] một lần ở cuối**. Ghi mỗi op vào `scan_link_scores`.

| Detector | Op | Kết quả |
|---|---|---|
| `core/reference-broken` | `delta -0.75` | 0.25 |
| `core/name-reserved` | `delta -0.9` | 0.1 |

Tắt detector → link về baseline 1.0.

### 11.4 Name collisions (`core/name-collision`)
- `error`: ≥2 node khác nhau khai cùng tên normalize qua `frontmatter.name`.
- `warn`: bucket trộn — một node qua `frontmatter.name`, node khác qua path-derived.
- Chỉ kind có `frontmatter.name` trong identifiers mới tham gia.

### 11.5 Reserved names (self scope)
Node có identifier normalize ∈ `reservedNames[node.kind]` của **chính provider của node** → `warn` + mọi link resolve tới nó bị `delta -0.9`.

---

## 12. Analyzers

Dual-mode: **deterministic** (`evaluate(ctx) → Issue[]`, chạy trong scan/check) hoặc **probabilistic finder** (không có `evaluate`, có `prompt.md` + `report.schema.json` extend `findings/report.schema.json`, chỉ chạy dưới dạng job, kết quả vào `state_findings`).

Phase: `score` (duy nhất được ghi — confidence) → `detect` (mặc định) → `aggregate` (đọc `ctx.accumulatedIssues`).

`IAnalyzerContext`: `nodes`, `links`, `reservedNodePaths`, `brokenLinks`, `referenceablePaths`, `signals`, `observedRelations`, `accumulatedIssues` (aggregate), `adjustConfidence` (score).

### 12.1 Kernel-stamped (trong node-build, không phải analyzer)

| Id | Khi nào | Severity |
|---|---|---|
| `frontmatter-parse-error` | block khai báo nhưng parse lỗi (message không chứa raw input; thêm hint cho lỗi `description: use when: x`) | warn (error với `--strict`) |
| `frontmatter-malformed` | `data.hint ∈ {early-close, paste-with-indent, byte-order-mark, leading-blank-line, missing-close}` | warn/strict→error |
| `frontmatter-invalid` | parse OK nhưng sai schema kind (kể cả block vắng mặt mà kind có required) | warn/strict→error |
| `backtick-unbalanced` | fence không đóng (`data.kind: 'fence'`) hoặc inline backtick lẻ (`'inline'`) | warn/strict→error |

Mỗi node đi đúng **một** lane: parse-error → declared block (early-close trước, rồi schema) → heuristic fence → absent block (vẫn validate `{}`).

Hash (phải tái tạo **byte-chính xác** để tương thích sidecar cũ):
- `bodyHash` = sha256 của body bytes sau frontmatter.
- `frontmatterHash` = sha256 của YAML re-emit bằng `js-yaml dump` với `sortKeys: true, lineWidth: -1, noRefs: true, schema: CORE_SCHEMA`. **Rủi ro cao nhất khi port**: serializer YAML Rust phải cho output y hệt js-yaml 5. Cần golden test từ bản gốc.

### 12.2 Deterministic analyzers built-in

| Id | Phase | Làm gì |
|---|---|---|
| `reference-broken` | score | link trỏ tới thứ không tồn tại → error + delta -0.75 |
| `name-reserved` | score | file che lệnh built-in runtime → warn + delta -0.9 cho link tới nó |
| `name-collision` | detect | trùng tên (xem §11.4) |
| `name-mismatch` | detect | `frontmatter.name` ≠ handle từ path (theo `identifierMismatch`) |
| `link-kind-conflict` | detect | extractor bất đồng nghĩa (references vs invokes); loại trừ `points` |
| `link-self-loop` | detect | link tự trỏ về chính nó (bỏ qua nếu mọi occurrence ở code) |
| `reference-redundant` | detect | một node tham chiếu cùng target qua ≥2 link khác nhau |
| `extractor-collision` | detect | Signal bị reject |
| `schema-violation` | detect | node/link vi phạm schema (locked, không tắt được) |
| `annotation-field-unknown` | detect | key lạ trong `.sm` (typo guard) |
| `annotation-orphan` | detect | `.sm` mà `.md` không còn → warn |
| `annotation-stale` | detect | drift hash → **chỉ contribution UI, không phát issue** |
| `node-stability` | detect | `deprecated` → chip + issue; `experimental` → chip |
| `observed-link-dead` / `observed-link-missing` / `observed-node-dead` | detect | so link khai báo với session journal đã record |
| `link-counter` | detect | đếm in/out |
| `issue-counter` | aggregate | chip error/warn per node |

### 12.3 Probabilistic finders built-in (job, không chạy trong scan)
`ai-contradiction`, `ai-incoherence`, `ai-prose-to-rules`, `ai-redundancy`, `ai-scope`, `ai-security`, `ai-structure`, `ai-suspicion` (prompt injection), `ai-trigger` (description sai), `ai-vagueness`, `ai-verbosity`.

**Dismissal:** sau khi analyzer trả về, kernel drop issue có `data.target` khớp `annotations.issueSuppressions` `(analyzer, value)` của bất kỳ `nodeIds` — so khớp chính xác, phân biệt hoa thường.

---

## 13. Actions, Formatters, Hooks

### 13.1 Action
- `invoke(input, ctx)`: deterministic (code in-process) hoặc probabilistic (render prompt → job).
- `project(ctx)`: optional, deterministic, scan-time, chỉ phát view contribution (vd nút inspector).
- File theo convention: `<action-dir>/report.schema.json` (bắt buộc), `prompt.md` (probabilistic).
- Ghi state chỉ qua union `TActionWrite` (vd `{ kind: 'sidecar' }`, khai `writes: ['sidecar']`).
- **Modelo B:** fixer action khai `precondition.analyzerIds` = finder/analyzer mà nó sửa.

Built-in deterministic: `node-bump`, `node-set-stability`, `node-set-tags` (đều ghi sidecar, `defaultEnabled: false` với bump/stability), `github/enrichment` (network, `sm enrich`).
Built-in probabilistic: fixer cho mỗi finder (`ai-*-action`), `ai-frontmatter-action`, `ai-name-action`, `ai-reference-action` (fix broken ref — fixer của analyzer tất định), `ai-summarizer-action` (→ `state_summaries`), `ai-tagger-action` (→ chỉ *đề xuất* tags, không ghi), `ai-ping-action` (probe liveness, `probNodeless`).

Routing report: Analyzer → `state_findings`; Action có schema dưới `summaries/` → `state_summaries`; dưới `tags/` → proposal trên event; dưới `enrichments/` → `state_enrichments`; còn lại → `state_executions.report_json`.

### 13.2 Formatter
`sm graph --format <folder-name>`. Built-in: `ascii` (mặc định, locked), `json` (= ScanResult), `mermaid` (`flowchart`, mỗi file một node, edge có label), `dot` (Graphviz `digraph`). Output phải diffable.

### 13.3 Hook
9 event cố định: `scan.started`, `scan.completed`, `extractor.completed`, `analyzer.completed`, `action.completed`, `job.completed`, `job.failed`, `boot`, `shutdown`. Chỉ phản ứng, không chặn/sửa pipeline; muốn LLM thì `ctx.queue('<plugin>/<action>', payload)`. Built-in: `core/update-check`.

---

## 14. Incremental scan cache

Cache key cho mỗi `(node, extractor)`: **body hash + sidecar-annotations hash (canonical) + resolved-settings hash**.
- Node không đổi chỉ được skip hoàn toàn khi **mọi** extractor áp dụng cho kind đó đều có run cũ khớp.
- Extractor mới cài → chạy trên node cache.
- Extractor bị gỡ → xoá cache row, xoá link chỉ do nó phát; link có nhiều source → bỏ id đó khỏi `sources`.
- Sửa `.sm` → invalidate mọi extractor của node đó.
- Token: node cache giữ `tokens` cũ kể cả khi scan này tắt tokenize; node cũ không có tokens không được cache khi tokenize bật.
- Analyzer **luôn** chạy lại trên toàn graph merged (không có single-node scan).
- `--full` bỏ cache; `--changed` = alias mặc định; `--full --changed` → exit 2.

**Schema drift (pre-1.0):** so `scan_meta.scanned_by_version` (khác minor/major = drift) và `scan_meta.schema_fingerprint` (hash DDL migration). Verb read-only: cảnh báo, lỗi do drift → advisory sạch exit 2 (không lộ SQL error). Verb ghi (jobs, record, findings…): từ chối exit 2 trước khi đụng bảng.

---

## 15. Sidecar `.sm` (annotation system)

YAML, đặt cạnh file: `foo.md` → `foo.sm`. Root blocks reserved: `identity`, `annotations`, `settings`, `audit`, + namespace plugin `<plugin-id>:`.

```yaml
identity:
  path: .claude/agents/foo.md
  bodyHash: <sha256>
  frontmatterHash: <sha256>
annotations:
  version: 3                 # int đơn điệu
  stability: stable          # experimental | stable | deprecated
  authors: [...]
  license: MIT
  source: https://github.com/...
  sourceVersion: v1.2.0
  tags: [review, ci]
  docsUrl: https://...
  suppressions: [{ extension, type?, note? }]          # finder classes
  issueSuppressions: [{ analyzer, value, note? }]       # deterministic issues
audit:
  createdAt: 2026-...; createdBy: <git user.name | cli | ui | plugin:<id>>
  lastBumpedAt: ...;   lastBumpedBy: ...
```

- **Drift** = hash live ≠ hash lưu → derived, không lưu cờ.
- **Orphan** = `identity.path` không còn tồn tại.
- **Bump** (`core/node-bump`): `version += 1` (hoặc 1), refresh 2 hash, stamp audit.
- **Ghi** chỉ qua `SidecarStore.applyPatch`: lock theo path, đọc-sửa-ghi, deep-merge (array thay, object đệ quy, `null` = xoá), ghi atomic `<path>.tmp` + rename.
- **Consent gate:**
  1. `allowSidecarWriters === false` (policy commit) → từ chối cứng, `--yes` không vượt qua.
  2. `allowEditSmFiles === true` → ghi.
  3. `always: true` → persist `allowEditSmFiles: true` vào `settings.local.json`, rồi ghi.
  4. `confirm: true` → ghi một lần, không persist.
  5. Không có gì → `EConsentRequiredError`: CLI TTY hỏi; CLI non-TTY exit 2 gợi ý `--yes`; BFF trả **412** `{ ok:false, error:{ code:'confirm-required', details:{ key:'allowEditSmFiles' } } }`.
- Denormalize vào DB: `scan_nodes.stability`, `version`, `annotations_json`, `sidecar_present`, `sidecar_status`; tags → `scan_node_tags`.
- Pre-commit hook: `sm hooks install pre-commit-bump` → `sm bump --pending --staged --force`.

---

## 16. Job queue (probabilistic execution)

### 16.1 State machine
```
(none) --submit--> queued --atomic claim--> running --record--> completed | failed
queued  --cancel--> cancelled        running --cancel--> cancelled
queued  --fail----> failed(user-failed)   running --fail/TTL/report-invalid/...--> failed
```
Terminal: `completed`, `failed`, `cancelled` — không chuyển tiếp nữa. `failure_reason ∈ {runner-error, report-invalid, timeout, abandoned, job-file-missing, user-failed}`.

### 16.2 Submit (`sm jobs submit <ext> -n <path>`)
1. Resolve extension (id, version, kind, `promptTemplateHash`); không có → exit 5; chỉ deterministic → exit 2.
2. Resolve node (exit 5 nếu không có).
3. `contentHash = sha256(join("\0", extensionId, extensionVersion, node.path, bodyHash, frontmatterHash, promptTemplateHash))`.
4. Dedup: tồn tại job `queued|running` cùng `(ext, version, node, contentHash)` → exit 3 (trừ `--force`, và unique partial index vẫn là bất biến cứng).
5. TTL (chỉ khi operator set: `--ttl` → `jobs.perExtensionTtl` → `jobs.ttlSeconds`; không có = không bao giờ hết hạn).
6. Priority: manifest `defaultPriority` → `jobs.perExtensionPriority.<id>` → `--priority`.
7. Nonce ngẫu nhiên mật mã ≥ 128 bit.
8. **Drift check:** đọc lại file, hash body phải khớp `bodyHash` đã scan.
9. Render content = preamble chuẩn + `prompt.md` với các section kernel chèn ở `{{userContent}}` (findings/issues cần fix, report contract inline) + khối `<user-content id="<node.path>">…</user-content>`. Lưu vào `state_job_contents`.
10. Insert `state_jobs` (bước 9–10 cùng transaction).

**Processing-agent gate:** nếu project chưa cài skill `sm-process-jobs` (`sm agent install`) → từ chối exit 2 kèm giải thích.
Nodeless (`probNodeless`): node id `sm://<qualified-id>`, hash dùng body/frontmatter hằng.
Fixer: inject `## Findings to resolve` (JSON array open findings của finder trên node; có thể giới hạn `--finding <id>`) hoặc `## Issues to resolve` (fixer của analyzer tất định). Selection rỗng → exit 2. Supersede job queued cũ chồng chéo.

### 16.3 Atomic claim (bắt buộc single-statement)
```sql
UPDATE state_jobs
   SET status='running', claimed_at=:now, runner=:runner,
       expires_at = CASE WHEN ttl_seconds IS NULL THEN NULL ELSE :now + ttl_seconds*1000 END
 WHERE id = (SELECT id FROM state_jobs WHERE status='queued' AND (:filter)
             ORDER BY priority DESC, created_at ASC LIMIT 1)
   AND status='queued'
RETURNING id;
```
`sm jobs claim --json` → `{ id, nonce, content }`; hàng đợi rỗng → exit 1. `--wait [--interval s] [--timeout s]`: poll (reap trước mỗi lần), stdout chỉ chứa payload. Content row thiếu → mark `failed/job-file-missing`, exit 2.
**Nonce chỉ lộ ở** `jobs submit --json` và `jobs claim --json`; mọi read khác dùng `PublicJob` (không có nonce).

### 16.4 Record (`sm record --id --nonce --status completed|failed --report <file|-> [--model …]`)
1. Không có job → exit 5. 2. Nonce sai → exit 4, không mutate. 3. Không ở `running` → exit 2.
4. `completed`: validate report với `report.schema.json` của extension; sai → `failed/report-invalid`.
5. Ghi `state_executions` (metrics, model, report_json). 6. Chuyển terminal. 7. Emit `job.callback.received` + `job.completed|failed`.
Write-through cùng transaction: summarizer → `state_summaries`; finder → xoá findings cũ `(node, ext)` rồi insert mới (`origin='extension'`); fixer → stamp `resolution` theo `resolved[]`. `auto_fix` → chain mọi fixer của finder sau commit.

### 16.5 Prompt preamble
Có delimiter contract `<user-content>` và text preamble chuẩn có version (`spec/prompt-preamble.md`). Nội dung giữa delimiter là **data, không phải instruction**. Port: nhúng nguyên văn file preamble gốc (MIT) để `contentHash` khớp.

### 16.6 Findings lifecycle
`resolution ∈ {NULL(open), fixed, human-decision, dismissed}`, `resolution_actor ∈ {human, fixer}`. Stale = body hash đổi từ lúc sinh. Kernel cũng ghi safety rows `origin='kernel'` (`injection-detected`, `content-suspicious`, `content-malformed`). Dismiss mặc định là row-grain (DB); `--class` ghi suppression vĩnh viễn vào `.sm`.

---

## 17. Database (SQLite)

File: `.skill-map/skill-map.db`. 3 zone: `scan_*` (replace-all mỗi scan, drop khi đổi lens), `state_*` (giữ qua scan), `config_*`. Một migration `001_initial.sql`; version theo `PRAGMA user_version` + `config_schema_versions`. Cột `*_json` là TEXT JSON; thời gian là epoch ms INTEGER.

DDL đầy đủ ở **Phụ lục A** (đã bỏ comment, nguyên văn cấu trúc). Tóm tắt:

| Bảng | PK | Vai trò |
|---|---|---|
| `scan_nodes` | `path` | node + hash, bytes, tokens, sidecar overlay |
| `scan_links` | `id` autoinc | link + trigger, location, occurrences, resolved_target |
| `scan_issues` | `id` autoinc | issue (analyzer_id lưu dạng **short**) |
| `scan_meta` | `id=1` singleton | roots, providers, stats, ceiling, tokenizer, active_provider, fingerprints |
| `scan_extractor_runs` | `(node_path, extractor_id)` | incremental cache 3 chân |
| `scan_contributions` / `scan_contribution_errors` | … | view slots |
| `scan_link_scores` | — | audit trail confidence ops |
| `scan_node_tags` | `(node_path, tag)` | index tag |
| `node_enrichments` | `(node_path, extractor_id)` | lớp enrichment của extractor |
| `state_jobs` | `id` | queue; unique partial index `(extension_id, node_id, content_hash) WHERE status IN ('queued','running')` |
| `state_job_contents` | `content_hash` | prompt đã render |
| `state_executions` | `id` | lịch sử chạy |
| `state_summaries` | `(node_id, summarizer_action_id)` | |
| `state_findings` | `id` | |
| `state_enrichments` | `(node_id, provider_id)` | enrichment action (network) |
| `state_plugin_kvs` | `(plugin_id, node_id, key)` | KV plugin |
| `state_node_favorites`, `state_activity_stats`, `state_activity_pairs` | | UI / activity |
| `config_preferences`, `config_schema_versions` | | |

`sm db reset` → drop `scan_*`; `--state` → + `state_*`; `--hard` → xoá file.

---

## 18. CLI contract

Binary `sm`, alias `skill-map`. Bare `sm`:
- có `.skill-map/` → như `sm serve`;
- cwd rỗng + TTY → menu tutorial / example;
- cwd không rỗng + TTY → hỏi `sm init` rồi serve;
- còn lại → hint stderr, exit 2.

### 18.1 Global flags
| Flag | Env | Ghi chú |
|---|---|---|
| `--json` | `SKILL_MAP_JSON=1` | stdout máy đọc; tiến trình ra stderr |
| `--log/--log-level <lvl>` | `SKILL_MAP_LOG_LEVEL` | trace…silent; sai → warn, không fail; standing pref `logLevel` trong settings.local |
| `-q/--quiet` | | |
| `--no-color` | `NO_COLOR` | tự tắt khi stdout không phải TTY |
| `-h/--help` | | |
| `-v/--version` | | chỉ bare invocation |
| `--db <path>` | `SKILL_MAP_DB` | |

Thứ tự ưu tiên: flag > env > config. Global flag đặt ở đâu cũng được. `--all` chỉ hợp lệ trên verb liệt kê (jobs submit/cancel/fail, findings clear, plugins enable/disable/trust/untrust).

### 18.2 Exit codes
| Code | Nghĩa |
|---|---|
| 0 | OK |
| 1 | Có issue error / item lỗi trong batch / queue rỗng (claim) / delta khác rỗng (compare-with) / conformance fail |
| 2 | Lỗi vận hành (flag sai, DB hỏng, config hỏng, exception) |
| 3 | Duplicate job |
| 4 | Nonce mismatch |
| 5 | Not found (node/job/plugin/key, hoặc **DB chưa tồn tại** với verb đọc) |
| 6–15 | reserved |

### 18.3 Quy ước chung
- **Dry-run** (`-n/--dry-run`, per-verb): không side effect, không tạo thư mục, output giống live + `dryRun: true`, exit code giống live.
- **Destructive confirm** từ chối → exit 0 + dòng `ℹ` (không phải lỗi), không nhớ.
- **`sm help --format json`**: `{ cliVersion, specVersion, globalFlags[], verbs[{name, category, description, details, positionals, examples, flags[{name, aliases, type: boolean|string, description, required}], exitCodes}] }` — `verbs` phẳng (`"jobs submit"`), flags đầy đủ. Rust: sinh tự động từ định nghĩa `clap`.

### 18.4 Verb catalog (rút gọn)

| Nhóm | Verb |
|---|---|
| Setup | `init [--no-scan] [--force] [-n]`, `tutorial [--for] [--force] [--completed]`, `example [--force]`, `version` (`{sm, spec, dbSchema}`), `doctor` (checks: db-integrity, migrations, orphan-history, job-contents, job-gc, jobs-overdue, plugins, trust-scope, providers), `help` |
| Config | `config list/get/set [--yes]/reset/show --source` (key lạ → exit 5) |
| Scan | `scan [roots…] [--full\|--changed] [--watch] [--strict] [--no-tokens] [--max-scan N] [--max-nodes N] [--watch-backend] [--no-built-ins] [-n]`, `scan compare-with <dump>`, `watch` (ndjson ScanResult mỗi batch), `enrich <node>\|--stale` |
| Browse | `list [--kind] [--issue] [--tag] [--sort-by] [--limit]`, `show <path>`, `check [-n] [--analyzers]`, `graph [--format]`, `export <query> --format json\|md\|mermaid`, `orphans [reconcile\|undo-rename]` |
| Findings / Issues | `findings [filters]`, `findings prune/clear/resolve/dismiss [--class]/reopen/suppressions/undismiss`, `issues dismiss/undismiss/suppressions` |
| Sidecar | `bump <node> \| --pending [--staged]`, `sidecars annotate/refresh/prune`, `hooks install pre-commit-bump` |
| Actions | `actions list/show` |
| Jobs | `jobs submit/list/show/preview/claim/status/cancel/fail/prune`, `record` |
| Agent | `agent install/uninstall/status [--for]` — cài skill `sm-process-jobs` |
| History | `history [filters]`, `history stats [--period] [--top]` |
| Plugins | `plugins list/show/enable/disable/trust/untrust/config/doctor/create/upgrade`, `plugins slots list` |
| DB | `db reset [--state\|--hard]/backup/restore/shell/browser/dump/migrate` |
| Server | `serve [--port 4242] [--host 127.0.0.1] [--mcp] [--no-built-ins] [--no-plugins] [--open\|--no-open] [--dev-cors] [--no-ui] [--no-watcher]` |
| Activity | `activity install/uninstall/status <provider>` |
| Conformance | `conformance run [--scope spec\|provider:<id>\|all] [--case]` |

Scan exit: 0 sạch, 1 có issue `error`, 2 lỗi vận hành. Watcher không đổi exit code theo batch.

---

## 19. HTTP BFF + WebSocket

Mặc định `127.0.0.1:4242`, loopback-only, không auth. Envelope REST (`schemas/api/rest-envelope.schema.json`) có `schemaVersion`, `kind`, `items`/`item`, kèm `kindRegistry` + `providerRegistry` trên payload. Lỗi: `{ ok:false, error:{ code, message, details } }` (vd 400 `bad-query`, 403 `locked`, 404 `not-found`, 412 `confirm-required`). `:pathB64` = base64 của node path.

**Read:** `GET /api/health`, `/api/scan[?meta=1|fresh=1]`, `/api/nodes?kind=&hasIssues=&path=&limit=&offset=`, `/api/nodes/:pathB64[?include=body,raw]`, `/api/links?kind=&from=&to=`, `/api/issues?severity=&analyzerId=&node=`, `/api/folders`, `/api/branch?path=&exclude=&excludeRoot=&limit=`, `/api/graph?format=`, `/api/config`, `/api/config/resolution`, `/api/plugins`, `/api/jobs`, `/api/nodes/:pathB64/{findings,summary,prob-extensions}`, `/api/map-views`, `/api/preferences`, `/api/project-preferences`, `/api/project-ignore`, `/api/mcp/status`, `/api/agent/presence`, `/api/activity/*`, `/api/update-status`, `/api/annotations/registered`.

**Write:** `POST /api/scan`, `PATCH /api/plugins[/:id[/trust]]`, `PATCH /api/plugins/:pluginId/extensions/:extensionId`, `POST /api/nodes/:pathB64/jobs`, `POST /api/jobs` (nodeless), `POST /api/jobs/:id/cancel`, `/api/jobs/cancel-all`, `/api/jobs/prune`, `POST /api/actions/:pluginId/:actionId`, findings/issues dismiss/undismiss/resolve/reopen, `PUT/DELETE /api/favorites/:pathB64`, `PUT/DELETE /api/map-views/:slug`, `PATCH /api/project-ignore`, `PATCH /api/(project-)preferences`, `POST /api/activity`, `POST /api/job-events`, activity/agent install/uninstall.

**Map scope overrides** (`/api/branch`): tập `(path, include|exclude)`; trạng thái node = override của tổ tiên **gần nhất** (path dài nhất khớp, kể cả chính nó); không khớp → include.

**WebSocket** `GET /ws`: server-push only (scan batch, job events, `node.activity`, `agent.spawn`).
`GET *` → static UI, fallback `index.html` (SPA).

Rust: `axum` + `tower-http` (static, CORS dev), `tokio-tungstenite`/axum ws. **Để chạy UI Angular gốc, mọi shape response phải giữ nguyên.**

---

## 20. MCP server

Gắn vào `sm serve` tại `/mcp`, Streamable HTTP (`POST /mcp` JSON-RPC 2.0, `GET /mcp` SSE), bật bằng `mcp.server.enabled` hoặc `--mcp`. Capabilities: `tools {listChanged}`, `resources {subscribe, listChanged}`; không prompts/sampling. `serverInfo.name = "skill-map"`.

**Tools — map (read-only):** `query_graph {kind?, has?, path?, limit?}`, `get_node {path, includeBody?}`, `list_issues {severity?, analyzerId?, node?, limit?, offset?}`, `get_branch {path[], exclude?, excludeRoot?, limit?}`.
**Tools — queue:** `list_extensions`, `list_jobs`, `get_job`, `submit_job`, `claim_job {runner?, filter?, wait?}` (tool duy nhất trả nonce), `record_job`, `cancel_job`, `fail_job`.
**Tools — findings:** `list_findings`, `resolve_finding`, `dismiss_finding`, `reopen_finding`, `undismiss_finding`, `delete_finding`.
**Tools — issues:** `dismiss_issue`, `undismiss_issue`, `list_issue_suppressions`.
**Resources:** `skillmap://graph`, `skillmap://issues`, `skillmap://activity`, template `skillmap://node/{path}`.
Tool ghi mở DB ở posture WRITE (từ chối khi drift). Unknown path → JSON-RPC `-32602`.

Rust: crate `rmcp` (official Rust MCP SDK) hỗ trợ Streamable HTTP.

---

## 21. Live activity (tóm tắt — đọc `spec/provider-activity.md` khi tới P6)

Provider khai capability `activity`: manifest có `install {kind, configPath}` (project-local hook config của runtime, vd `.claude/settings.json` hooks); runtime method `mapEvent(raw) → [{kind, name, phase, owner?}]`. `sm activity install claude` ghi hook gọi bridge → `POST /api/activity` → BFF resolve `(kind, name)` → node qua cùng identifier contract → WS `node.activity` / `agent.spawn`. Trạng thái in-memory; chỉ session journal (khi user bấm record) được lưu `.skill-map/sessions/*.json` (không chứa nội dung hội thoại). Conversation capture có consent gate riêng (`activity.captureConversations`).

---

## 22. Hệ thống plugin & quyết định cho Rust

Bản gốc: plugin là thư mục `<root>/<id>/plugin.json` + mỗi extension `<plugin>/<kind>s/<name>/extension.json` (declarative: `version`, `description`, `stability`, `defaultEnabled`) + module JS (behavior). **Phải đọc/validate `extension.json` trước khi import code**; extension disabled thì không import. Id qualified `<plugin-id>/<ext-id>`. Trust import plugin lưu ở scope lock (grant gắn với filesystem identity của `.skill-map/`, không portable). Locked built-ins: `core/markdown`, `agent-skills/agent-skills`, `core/schema-violation`, `core/ascii`.

Provider kinds là structure-as-truth: `<plugin>/kinds/<kindName>/{schema.json, kind.json}`.

**Quyết định cần chốt cho skflow** (bản Rust không thể `import()` JS):

| Phương án | Ưu | Nhược |
|---|---|---|
| A. Chỉ built-in (compile vào binary) | Đơn giản, nhanh nhất | Không mở rộng runtime |
| B. Plugin **declarative-only** (provider kinds/classify bằng glob, resolution, reservedNames, formatter template) | Phủ ~80% nhu cầu provider mới, an toàn | Không có extractor/analyzer tuỳ ý |
| C. WASM (`wasmtime`, WIT interface cho extract/evaluate) | Sandbox, đa ngôn ngữ | Phức tạp nhất |
| D. Subprocess JSON-RPC (giống LSP) | Tái dùng plugin JS gốc qua node shim | Chậm, cần Node |

Khuyến nghị: **A cho P1–P5, B ở P6, C sau nếu cần**. Trait nội bộ nên thiết kế sẵn như contract để chuyển sang WASM sau.

```rust
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn manifest(&self) -> &ProviderManifest;          // detect, read, kinds, resolution, reserved_names, gated_by_active_lens
    fn classify(&self, path: &str, fm: &serde_json::Value) -> Option<KindId>;
}
pub trait Extractor: Send + Sync {
    fn id(&self) -> &str;
    fn precondition(&self) -> Option<&Precondition>;
    fn extract(&self, ctx: &mut ExtractCtx);          // emit_link / emit_signal / enrich_node / emit_contribution
}
pub enum AnalyzerPhase { Score, Detect, Aggregate }
pub trait Analyzer: Send + Sync {
    fn id(&self) -> &str;
    fn phase(&self) -> AnalyzerPhase { AnalyzerPhase::Detect }
    fn evaluate(&self, ctx: &mut AnalyzeCtx) -> Vec<Issue>;
}
pub trait Formatter: Send + Sync { fn id(&self) -> &str; fn format(&self, g: &Graph) -> String; }
```

---

## 23. Bất biến bảo mật (bắt buộc giữ)

1. Không đọc `$HOME` (ngoại lệ duy nhất có tài liệu: `~/.skill-map/settings.json`, codex home config khi provider khai).
2. Realpath containment cho symlink; hostile repo không được kéo `~/.ssh/id_rsa` vào graph.
3. `PROJECT_LOCAL_ONLY_KEYS` bị strip khỏi layer commit; grant trong scope lock không portable.
4. Extension không ghi FS / không gọi network (trừ `io: ['network']` + `allowNetworkActions` + `sm enrich`).
5. Nonce ≥128 bit, không log ở level ≥ info, không lộ qua read surface.
6. Prompt: nội dung node nằm trong `<user-content>`; findings/issues chèn vào prompt được đánh dấu là DATA.
7. Server loopback mặc định, CORS chỉ khi `--dev-cors`.
8. Thông báo lỗi parse YAML không nhúng raw input; strip control bytes.
9. Plugin bên thứ ba phải được `trust` trước khi chạy code.

---

## 24. Ánh xạ sang Rust (đề xuất workspace)

Điều chỉnh từ kế hoạch trong `README.md` của skflow:

```
skflow/
├── crates/
│   ├── skflow-spec      # include_str! toàn bộ spec/schemas/*.json; serde types Node/Link/Issue/ScanResult/Job…
│   ├── skflow-kernel    # orchestrator, registry, resolver, confidence fold, normalization, config layering, ports (traits)
│   ├── skflow-parse     # frontmatter YAML/TOML, strip-code-blocks, inverse mask, token grammars, backtick balance
│   ├── skflow-plugins   # 6 provider + extractors + analyzers + formatters built-in
│   ├── skflow-store     # SQLite adapter (rusqlite), migrations, repositories, atomic claim
│   ├── skflow-fs        # walker (ignore crate), symlink containment, watcher (notify), sidecar store
│   ├── skflow-jobs      # submit/claim/record, preamble, findings write-through
│   ├── skflow-server    # axum BFF, WS, MCP (rmcp), static UI
│   └── skflow-cli       # clap v4, exit codes, --json, help --format json/md
└── conformance/         # copy spec/conformance/cases + fixtures; runner Rust
```

| Nhu cầu | Crate gợi ý | Lưu ý |
|---|---|---|
| CLI | `clap` v4 (derive) | sinh `help --format json` từ `Command` |
| SQLite | `rusqlite` (bundled) | `RETURNING` cần SQLite ≥ 3.35; WAL |
| JSON Schema 2020-12 | `jsonschema` | validate với schema gốc |
| YAML | `serde_yaml_ng` / `saphyr` | **frontmatterHash cần output = js-yaml 5 dump(sortKeys, CORE_SCHEMA)** → có thể phải tự viết emitter canonical |
| TOML | `toml` | codex agents |
| Markdown | tự viết scanner fence/inline (CommonMark rules) hoặc `pulldown-cmark` với offset | phải khớp `findBacktickImbalance` / `stripCodeBlocks` |
| Regex lookaround | `fancy-regex` | xem §9.3 |
| Unicode | `unicode-normalization` + bảng category Mn | |
| Tokenizer | `tiktoken-rs` (`cl100k_base`) | so token count với `gpt-tokenizer` trên fixture |
| Hash | `sha2` | |
| Ignore | `ignore` crate (gitignore semantics, `!` re-include) | thứ tự layer §8 bước 2 |
| Watcher | `notify` + debounce 300ms | không polling fallback (giống gốc) |
| HTTP/WS | `axum`, `tower-http`, `tokio` | |
| MCP | `rmcp` | Streamable HTTP |
| Graph | `petgraph` (tuỳ chọn) | gốc không cần thuật toán đồ thị nặng; chủ yếu index map |
| Song song | `rayon` cho extract per-node | kết quả phải sort tất định trước khi persist |

**Tính tất định:** mọi output (links, issues, ops) phải sort ổn định — ScanResult phải so được byte-wise với bản gốc.

---

## 25. Kiểm thử & conformance

- `spec/conformance/` chứa **49 case** dữ liệu ngôn ngữ-trung lập (`conformance-case.schema.json`) + fixtures; mỗi provider có suite riêng. Port runner sang Rust trước, dùng làm định nghĩa "đúng".
- Golden test chéo: chạy `sm scan --json` (Node, `@skill-map/cli@1.12.7`) và `skflow scan --json` trên cùng fixture (`sm example` sinh sẵn một project mẫu có graph liên thông) → diff JSON đã sort.
- Bảng test cho: normalization (§10), 4 token grammar (§9.3), confidence fold (§11.3), frontmatter lanes (§12.1), backtick balance, YAML canonical hash, atomic claim race (2 thread claim cùng lúc).
- Bản gốc có 5 723 test unit/integration — lấy test case dạng bảng trong `src/kernel/util/__tests__` và `src/plugins/**/__tests__` làm nguồn vector.

---

## 26. Những gì tài liệu này CHƯA phủ chi tiết

Đã đọc lướt hoặc chưa đọc; cần mở trực tiếp khi tới pha tương ứng:
- `spec/provider-activity.md` (103 KB — activity bridge, session journal, capture levels)
- `spec/view-slots.md`, `spec/map-views.md` (closed catalog view slot cho UI)
- `spec/plugin-author-guide.md`, `plugin-kv-api.md`, `input-types.md` (plugin authoring, settings input types)
- `spec/job-events.md` (danh sách event + payload), `spec/prompt-preamble.md` (text preamble nguyên văn)
- `spec/skill-actions.md` (chạy skill `SKILL.md` của bên thứ ba như action)
- `spec/telemetry.md`, `spec/versioning.md`
- Nội dung prompt `prompt.md` + `report.schema.json` của từng `ai-*` extension (`src/plugins/core/**`)
- Rename heuristic (auto-rename high/medium/ambiguous) của scan
- Toàn bộ UI Angular (`ui/`) — khuyến nghị **giữ nguyên** UI gốc, chỉ thay backend

Mã nguồn đã clone sẵn tại thư mục scratchpad của phiên này để đối chiếu; khi cần lâu dài nên `git clone https://github.com/crystian/skill-map` vào `vendor/` (đã gitignore) hoặc thêm làm submodule.

---

## Phụ lục A — DDL SQLite (`src/migrations/001_initial.sql`, bỏ comment)

```sql
CREATE TABLE scan_nodes (
  path TEXT PRIMARY KEY, kind TEXT NOT NULL, provider TEXT NOT NULL,
  title TEXT, description TEXT, stability TEXT, version INTEGER,
  frontmatter_json TEXT NOT NULL, body_hash TEXT NOT NULL, frontmatter_hash TEXT NOT NULL,
  bytes_frontmatter INTEGER NOT NULL, bytes_body INTEGER NOT NULL, bytes_total INTEGER NOT NULL,
  tokens_frontmatter INTEGER, tokens_body INTEGER, tokens_total INTEGER,
  links_out_count INTEGER NOT NULL DEFAULT 0, links_in_count INTEGER NOT NULL DEFAULT 0,
  external_refs_count INTEGER NOT NULL DEFAULT 0, external_refs_json TEXT,
  scanned_at INTEGER NOT NULL, modified_at_ms INTEGER,
  virtual INTEGER NOT NULL DEFAULT 0, derived_from_json TEXT,
  sidecar_present INTEGER NOT NULL DEFAULT 0, sidecar_status TEXT,
  annotations_json TEXT, sidecar_root_json TEXT,
  CONSTRAINT ck_scan_nodes_stability CHECK (stability IS NULL OR stability IN ('experimental','stable','deprecated'))
);
CREATE INDEX ix_scan_nodes_kind ON scan_nodes(kind);
CREATE INDEX ix_scan_nodes_provider ON scan_nodes(provider);
CREATE INDEX ix_scan_nodes_body_hash ON scan_nodes(body_hash);
CREATE INDEX ix_scan_nodes_sidecar_status ON scan_nodes(sidecar_status);

CREATE TABLE scan_links (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_path TEXT NOT NULL, target_path TEXT NOT NULL, kind TEXT NOT NULL,
  confidence REAL NOT NULL, sources_json TEXT NOT NULL,
  original_trigger TEXT, normalized_trigger TEXT,
  location_line INTEGER, location_column INTEGER, location_offset INTEGER,
  occurrences_json TEXT, resolved_target TEXT, raw TEXT,
  CONSTRAINT ck_scan_links_kind CHECK (kind IN ('invokes','references','mentions','points')),
  CONSTRAINT ck_scan_links_confidence CHECK (confidence >= 0.0 AND confidence <= 1.0)
);
CREATE INDEX ix_scan_links_source_path ON scan_links(source_path);
CREATE INDEX ix_scan_links_target_path ON scan_links(target_path);
CREATE INDEX ix_scan_links_normalized_trigger ON scan_links(normalized_trigger);
CREATE INDEX ix_scan_links_resolved_target ON scan_links(resolved_target);

CREATE TABLE scan_issues (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  analyzer_id TEXT NOT NULL, severity TEXT NOT NULL, node_ids_json TEXT NOT NULL,
  link_indices_json TEXT, message TEXT NOT NULL, detail TEXT, fix_json TEXT, data_json TEXT,
  CONSTRAINT ck_scan_issues_severity CHECK (severity IN ('error','warn','info'))
);
CREATE INDEX ix_scan_issues_analyzer_id ON scan_issues(analyzer_id);
CREATE INDEX ix_scan_issues_severity ON scan_issues(severity);

CREATE TABLE state_jobs (
  id TEXT PRIMARY KEY, extension_id TEXT NOT NULL, extension_version TEXT NOT NULL,
  extension_kind TEXT NOT NULL, auto_fix INTEGER NOT NULL DEFAULT 0, finding_ids_json TEXT,
  node_id TEXT NOT NULL, content_hash TEXT NOT NULL, nonce TEXT NOT NULL,
  priority INTEGER NOT NULL DEFAULT 0, status TEXT NOT NULL, failure_reason TEXT, runner TEXT,
  ttl_seconds INTEGER, created_at INTEGER NOT NULL, claimed_at INTEGER, finished_at INTEGER,
  expires_at INTEGER, submitted_by TEXT,
  CONSTRAINT ck_state_jobs_extension_kind CHECK (extension_kind IN ('action','analyzer')),
  CONSTRAINT ck_state_jobs_status CHECK (status IN ('queued','running','completed','failed','cancelled')),
  CONSTRAINT ck_state_jobs_failure_reason CHECK (failure_reason IS NULL OR failure_reason IN ('runner-error','report-invalid','timeout','abandoned','job-file-missing','user-failed')),
  CONSTRAINT ck_state_jobs_runner CHECK (runner IS NULL OR runner IN ('agent','in-process'))
);
CREATE INDEX ix_state_jobs_status ON state_jobs(status);
CREATE UNIQUE INDEX ix_state_jobs_extension_node_hash
  ON state_jobs(extension_id, node_id, content_hash) WHERE status IN ('queued','running');

CREATE TABLE state_job_contents (content_hash TEXT PRIMARY KEY, content TEXT NOT NULL, created_at INTEGER NOT NULL);

CREATE TABLE state_executions (
  id TEXT PRIMARY KEY, kind TEXT NOT NULL, extension_id TEXT NOT NULL, extension_version TEXT NOT NULL,
  node_ids_json TEXT NOT NULL DEFAULT '[]', content_hash TEXT, status TEXT NOT NULL,
  failure_reason TEXT, exit_code INTEGER, runner TEXT,
  started_at INTEGER NOT NULL, finished_at INTEGER NOT NULL, duration_ms INTEGER,
  tokens_in INTEGER, tokens_out INTEGER, model TEXT, report_json TEXT, job_id TEXT,
  CONSTRAINT ck_state_executions_kind CHECK (kind IN ('action')),
  CONSTRAINT ck_state_executions_status CHECK (status IN ('completed','failed','cancelled'))
);
CREATE INDEX ix_state_executions_extension_id ON state_executions(extension_id);
CREATE INDEX ix_state_executions_started_at ON state_executions(started_at);
CREATE INDEX ix_state_executions_job_id ON state_executions(job_id);

CREATE TABLE state_summaries (
  node_id TEXT NOT NULL, kind TEXT NOT NULL, summarizer_action_id TEXT NOT NULL,
  summarizer_version TEXT NOT NULL, body_hash_at_generation TEXT NOT NULL,
  generated_at INTEGER NOT NULL, model TEXT, summary_json TEXT NOT NULL,
  PRIMARY KEY (node_id, summarizer_action_id)
);
CREATE INDEX ix_state_summaries_generated_at ON state_summaries(generated_at);

CREATE TABLE state_findings (
  id INTEGER PRIMARY KEY, node_id TEXT NOT NULL, extension_id TEXT NOT NULL,
  extension_version TEXT NOT NULL, origin TEXT NOT NULL, type TEXT NOT NULL,
  severity TEXT NOT NULL, message TEXT NOT NULL, detail TEXT, confidence REAL NOT NULL,
  model TEXT, resolution TEXT, resolution_actor TEXT, resolution_note TEXT, resolution_by TEXT,
  resolution_at INTEGER, body_hash_at_generation TEXT NOT NULL, generated_at INTEGER NOT NULL, job_id TEXT,
  CONSTRAINT ck_state_findings_origin CHECK (origin IN ('extension','kernel')),
  CONSTRAINT ck_state_findings_severity CHECK (severity IN ('info','warn','error')),
  CONSTRAINT ck_state_findings_resolution CHECK (resolution IS NULL OR resolution IN ('fixed','human-decision','dismissed')),
  CONSTRAINT ck_state_findings_resolution_actor CHECK (resolution_actor IS NULL OR resolution_actor IN ('human','fixer'))
);
CREATE INDEX ix_state_findings_node_id ON state_findings(node_id);
CREATE INDEX ix_state_findings_extension_id ON state_findings(extension_id);
CREATE INDEX ix_state_findings_generated_at ON state_findings(generated_at);

CREATE TABLE state_enrichments (
  node_id TEXT NOT NULL, provider_id TEXT NOT NULL, data_json TEXT NOT NULL,
  verified INTEGER, fetched_at INTEGER NOT NULL, stale_after INTEGER,
  PRIMARY KEY (node_id, provider_id),
  CONSTRAINT ck_state_enrichments_verified CHECK (verified IS NULL OR verified IN (0,1))
);
CREATE INDEX ix_state_enrichments_stale_after ON state_enrichments(stale_after);

CREATE TABLE state_plugin_kvs (
  plugin_id TEXT NOT NULL, node_id TEXT NOT NULL DEFAULT '', key TEXT NOT NULL,
  value_json TEXT NOT NULL, updated_at INTEGER NOT NULL,
  PRIMARY KEY (plugin_id, node_id, key)
);
CREATE INDEX ix_state_plugin_kvs_plugin_id ON state_plugin_kvs(plugin_id);

CREATE TABLE state_node_favorites (node_path TEXT PRIMARY KEY, favorited_at INTEGER NOT NULL);

CREATE TABLE state_activity_stats (
  node_path TEXT PRIMARY KEY, count INTEGER NOT NULL DEFAULT 0, first_seen_at INTEGER NOT NULL,
  last_start_at INTEGER NOT NULL DEFAULT 0, last_owner TEXT,
  owners_json TEXT NOT NULL DEFAULT '[]', recent_json TEXT NOT NULL DEFAULT '[]',
  tool_uses INTEGER NOT NULL DEFAULT 0, tokens INTEGER NOT NULL DEFAULT 0,
  summarized_runs INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE state_activity_pairs (
  parent TEXT NOT NULL, child_node_path TEXT NOT NULL,
  count INTEGER NOT NULL DEFAULT 0, last_start_at INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (parent, child_node_path)
);

CREATE TABLE config_preferences (key TEXT PRIMARY KEY, value_json TEXT NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE config_schema_versions (
  scope TEXT NOT NULL, owner_id TEXT NOT NULL, version INTEGER NOT NULL,
  description TEXT NOT NULL, applied_at INTEGER NOT NULL,
  PRIMARY KEY (scope, owner_id, version),
  CONSTRAINT ck_config_schema_versions_scope CHECK (scope = 'kernel')
);

CREATE TABLE scan_meta (
  id INTEGER PRIMARY KEY, roots_json TEXT NOT NULL, scanned_at INTEGER NOT NULL,
  scanned_by_name TEXT NOT NULL, scanned_by_version TEXT NOT NULL, scanned_by_spec_version TEXT NOT NULL,
  providers_json TEXT NOT NULL, stats_files_walked INTEGER NOT NULL, stats_files_skipped INTEGER NOT NULL,
  stats_duration_ms INTEGER NOT NULL, scan_ceiling INTEGER NOT NULL,
  scan_truncated INTEGER NOT NULL DEFAULT 0, max_render_nodes INTEGER NOT NULL,
  files_oversized INTEGER NOT NULL DEFAULT 0, oversized_files_json TEXT, tokenizer TEXT,
  active_provider TEXT, schema_fingerprint TEXT, result_fingerprint TEXT,
  CONSTRAINT ck_scan_meta_singleton CHECK (id = 1)
);

CREATE TABLE scan_extractor_runs (
  node_path TEXT NOT NULL, extractor_id TEXT NOT NULL, body_hash_at_run TEXT NOT NULL,
  sidecar_annotations_hash_at_run TEXT NOT NULL, settings_hash_at_run TEXT NOT NULL DEFAULT '',
  ran_at INTEGER NOT NULL,
  PRIMARY KEY (node_path, extractor_id)
);
CREATE INDEX ix_scan_extractor_runs_node ON scan_extractor_runs(node_path);
CREATE INDEX ix_scan_extractor_runs_extractor ON scan_extractor_runs(extractor_id);

CREATE TABLE node_enrichments (
  node_path TEXT NOT NULL, extractor_id TEXT NOT NULL, body_hash_at_enrichment TEXT NOT NULL,
  value_json TEXT NOT NULL, stale INTEGER NOT NULL DEFAULT 0, enriched_at INTEGER NOT NULL,
  is_probabilistic INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (node_path, extractor_id),
  CONSTRAINT ck_node_enrichments_stale CHECK (stale IN (0, 1)),
  CONSTRAINT ck_node_enrichments_is_probabilistic CHECK (is_probabilistic IN (0, 1))
);
CREATE INDEX ix_node_enrichments_node ON node_enrichments(node_path);
CREATE INDEX ix_node_enrichments_stale ON node_enrichments(stale);

CREATE TABLE scan_contributions (
  plugin_id TEXT NOT NULL, extension_id TEXT NOT NULL, node_path TEXT NOT NULL,
  contribution_id TEXT NOT NULL, slot TEXT NOT NULL, payload_json TEXT NOT NULL, emitted_at INTEGER NOT NULL,
  PRIMARY KEY (plugin_id, extension_id, node_path, contribution_id)
);
CREATE INDEX ix_scan_contributions_node_path ON scan_contributions(node_path);
CREATE INDEX ix_scan_contributions_plugin_id ON scan_contributions(plugin_id);

CREATE TABLE scan_contribution_errors (
  plugin_id TEXT NOT NULL, extension_id TEXT NOT NULL, node_path TEXT NOT NULL,
  reason TEXT NOT NULL, message TEXT NOT NULL, contribution_id TEXT, slot TEXT, emitted_at INTEGER NOT NULL
);
CREATE INDEX ix_scan_contribution_errors_plugin_id ON scan_contribution_errors(plugin_id);
CREATE INDEX ix_scan_contribution_errors_node_path ON scan_contribution_errors(node_path);

CREATE TABLE scan_link_scores (
  plugin_id TEXT NOT NULL, extension_id TEXT NOT NULL, source_path TEXT NOT NULL,
  target TEXT NOT NULL, kind TEXT NOT NULL, normalized_trigger TEXT,
  op_kind TEXT NOT NULL, op_value REAL NOT NULL, result_confidence REAL NOT NULL, emitted_at INTEGER NOT NULL
);
CREATE INDEX ix_scan_link_scores_source_path ON scan_link_scores(source_path);

CREATE TABLE scan_node_tags (node_path TEXT NOT NULL, tag TEXT NOT NULL, PRIMARY KEY (node_path, tag));
CREATE INDEX ix_scan_node_tags_tag ON scan_node_tags(tag);
CREATE INDEX ix_scan_node_tags_node_path ON scan_node_tags(node_path);
```

---

## Phụ lục B — Nguồn tham chiếu trong repo gốc

| Chủ đề | File |
|---|---|
| Kiến trúc tổng | `spec/architecture.md` |
| CLI | `spec/cli-contract.md` |
| DB | `spec/db-schema.md`, `src/migrations/001_initial.sql` |
| Job | `spec/job-lifecycle.md`, `spec/job-events.md`, `spec/prompt-preamble.md` |
| MCP | `spec/mcp-server.md` |
| Schemas | `spec/schemas/*.schema.json`, `spec/index.json` |
| Conformance | `spec/conformance/{cases,fixtures}` |
| Provider built-in | `src/plugins/{claude,codex,agent-skills,antigravity,opencode}/providers/*/index.ts`, `src/plugins/core/providers/core-markdown` |
| Token grammars | `src/kernel/util/{slash,at,dollar}-token.ts`, `strip-code-blocks.ts` |
| Defaults | `src/config/defaults.json` |

Giấy phép gốc: MIT © Crystian — giữ thông báo bản quyền khi tái sử dụng schema/DDL/preamble.
