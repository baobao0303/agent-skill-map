# skflow — Thiết kế Rust (spec_rust)

> Biến [`spec_doc.md`](spec_doc.md) (hành vi của `skill-map`) thành thiết kế Rust cụ thể cho `skflow`.
> Kiểm chứng theo [`rea_plan.md`](rea_plan.md): mỗi mục ghi **obligation ID**; "xong" = khớp oracle `sm 1.12.7`.
> Ràng buộc từ [`.claude/CLAUDE.md`](.claude/CLAUDE.md): Rayon cho duyệt file song song, `pulldown-cmark` + `gray_matter` cho parse, **không thêm abstraction không được yêu cầu**.
> Phạm vi chi tiết: **P1 (scan core)**. P2–P5 ở mức khung (§14).

---

## 0. Những gì oracle đã xác nhận (khác/bổ sung so với spec_doc)

Đo trực tiếp trên `sm 1.12.7` (`rea/evidence/p1-oracle.json`, fixture `conformance/fixtures/example`):

| Điểm | Giá trị đo được | Nguồn |
|---|---|---|
| `bodyHash` | sha256 của **các byte sau dòng `---\n` đóng** (không gồm fence) | tính lại khớp `e05685ff…` |
| `bytes.frontmatter` | độ dài text giữa 2 fence, **không gồm `\n` cuối** (187) | golden |
| `bytes.total` | `frontmatter + body` (không tính 9 byte fence) | golden: 187 + 708 = 895, file 904 |
| `frontmatterHash` | `sha256(js-yaml dump(parsed, sortKeys, lineWidth -1, noRefs, CORE_SCHEMA))`; nếu parse ra `{}` mà raw khác rỗng → hash raw | bundle `chunk-RNMFOX2H.js` `canonicalFrontmatter`; tái tạo tay khớp `18f1a666…` |
| `tokens` | `count(frontmatterRaw)`, `count(body)` | bundle |
| Node ảo | `bodyHash`/`frontmatterHash` = 64 số `0`, `bytes` = 0 | golden `mcp://notion` |
| MCP config | `.mcp.json` ở root là nguồn của provider claude (ngoài `.claude/settings.json`) | `derivedFrom: [".mcp.json", …]` |
| `sm scan` lần đầu | tự tạo `.skill-map/{.gitignore, settings.json, skill-map.db, operations.log, backups/skill-map-pre-migrate-v1.db}` | capture `ev_de62bcdb…` |
| `settings.json` sau auto-detect | `{"activeProvider":"claude","activeProviderMarkers":["claude"]}` (indent 2) | file |
| `operations.log` | NDJSON: `{"at","op":"scan","target":"*","channel":"cli","outcome":"ok","detail":"nodes=9 issues=2"}` | file |
| `PRAGMA user_version` | `1`; `config_schema_versions` = `('kernel','kernel',1,'initial',<ms>)` | DB |
| Id trong output | **short id**: `analyzerId: "reference-broken"`, `sources: ["markdown-link"]`, `providers: [..., "markdown"]` | golden |
| Mọi lệnh | in `done in <N>ms` ở cuối (stderr) | capture; xem unknown `unk_4a63d144…` |
| `check` không DB | exit 5, `✕  DB not found at <cwd>/.skill-map/skill-map.db` + `   Run \`sm scan\` first.` | `ev_abdde906…` |

---

## 1. Package

Một crate duy nhất, dùng `Cargo.toml` sẵn có (`name = "skflow"`, edition 2024). Không tách workspace ở P1 — tách chỉ khi P5 (server) làm compile chậm rõ rệt. Kế hoạch 5 crate trong `README.md` cũ được thay bằng layout này.

```
skflow/
├── Cargo.toml
├── src/
│   ├── main.rs            # parse argv → dispatch verb → exit code
│   ├── cli.rs             # clap định nghĩa verb/flag; help --format json|md
│   ├── output.rs          # pretty/JSON/quiet/color; dòng "done in Nms"
│   ├── config.rs          # 4 layer, defaults.json nhúng, PROJECT_LOCAL_ONLY_KEYS
│   ├── scope.rs           # .skill-map/ provisioning, .gitignore, operations.log
│   ├── walk.rs            # ignore layers, size/ceiling, symlink containment (Rayon)
│   ├── frontmatter.rs     # tách fence (gray_matter), 4 lane chẩn đoán
│   ├── canon_yaml.rs      # emitter YAML tương thích js-yaml dump(sortKeys, CORE)
│   ├── mask.rs            # code region ranges (pulldown-cmark), strip + inverse mask
│   ├── tokens.rs          # cl100k_base
│   ├── norm.rs            # trigger normalization
│   ├── provider.rs        # 6 provider dạng bảng tĩnh + lens detect
│   ├── extract.rs         # 9 extractor + grammar
│   ├── resolve.rs         # dedupe, lift, code-region gate, confidence fold
│   ├── analyze.rs         # analyzer tất định P1
│   ├── sidecar.rs         # đọc .sm (P1 chỉ đọc; ghi ở P3)
│   ├── model.rs           # serde: Node, Link, Issue, ScanResult (đúng thứ tự key)
│   ├── format.rs          # ascii, json, mermaid, dot
│   ├── store.rs           # rusqlite, migrations, persist replace-all, read-back
│   └── schema/            # include_str! các JSON Schema + 001_initial.sql + defaults.json
└── conformance/           # fixtures + golden (đã có)
```

Mỗi file là một module phẳng; không trait cho thứ chỉ có một cài đặt. Provider/extractor/analyzer là **hàm + bảng tĩnh**, không trait object (plugin runtime là non-goal tới P6).

### 1.1 Dependencies

Phiên bản chốt lúc `cargo add` (bản ổn định mới nhất); ghi lại vào `Cargo.lock`.

| Crate | Dùng cho | Ghi chú |
|---|---|---|
| `clap` (derive) | CLI | |
| `serde`, `serde_json` (`preserve_order`) | model + frontmatter dạng `Value` giữ thứ tự key | thứ tự key frontmatter = thứ tự trong file |
| `gray_matter` | tách frontmatter | **chỉ để cắt fence**; số byte/hash tính theo §5 trên byte gốc |
| `serde_yaml_ng` | parse YAML → `serde_json::Value` | emit dùng `canon_yaml.rs`, không dùng serializer của crate |
| `toml` | Codex agents | |
| `pulldown-cmark` | vị trí fence/inline code (`into_offset_iter`) | |
| `fancy-regex` | 4 token grammar có lookaround | |
| `unicode-normalization` | NFD | |
| `unicode-general-category` | lọc `Mn` | |
| `rayon` | walk + node-build + extract song song | |
| `ignore` | gitignore semantics cho các layer ignore | dùng `gitignore::GitignoreBuilder`, tự ghép layer |
| `sha2`, `hex` | hash | |
| `tiktoken-rs` | token `cl100k_base` | xác nhận số token khớp `gpt-tokenizer` (obligation riêng) |
| `rusqlite` (`bundled`) | DB | cần SQLite ≥ 3.35 cho `RETURNING` (P4) |
| `jsonschema` | validate ScanResult / frontmatter kind | schema gốc nhúng bằng `include_str!` |
| `time` | ISO 8601 cho `operations.log` | |

---

## 2. CLI (P1) — obligation `cli.binary.help`, `cli.exit-codes`, `cli.browse`

Binary `skflow`. Verb và flag **giữ y tên của `sm`** để chạy cùng scenario trên cả hai.

| Verb P1 | Flag | Exit |
|---|---|---|
| `version` | `--json` → `{"sm","spec","dbSchema"}` (`sm` = version skflow; `spec` = `"1.12.7"` spec đang tuân theo; `dbSchema` = `"-"` khi chưa có DB) | 0 |
| `init` | `--no-scan`, `--force`, `-n/--dry-run` | 0/1/2 |
| `scan [roots…]` | `--json`, `--no-tokens`, `--dry-run`, `--changed`, `--full`, `--strict`, `--max-scan N`, `--max-nodes N`, `--no-built-ins`, `--no-plugins`, `--allow-empty`, `--yes` (P2: `--watch`, `--watch-backend`) | 0/1/2 |
| `check` | `-n <path>`, `--analyzers <ids>`, `--json` | 0/1/2/5 |
| `list` | `--kind`, `--issue`, `--tag`, `--sort-by`, `--limit`, `--json` | 0/2/5 |
| `show <path>` | `--json` | 0/5 |
| `graph` | `--format ascii\|json\|mermaid\|dot` (mặc định `ascii`) | 0/2/5 |
| `export <query>` | `--format json\|md\|mermaid` | 0/2/5 |
| `help [verb]` | `--format human\|md\|json` | 0/5 |

Global (mọi verb): `--json`, `-q/--quiet`, `--no-color`, `--db <path>`, `--log/--log-level <lvl>`, `-h/--help`; bare `-v/--version`. Env: `SKILL_MAP_JSON`, `NO_COLOR`, `SKILL_MAP_DB`, `SKILL_MAP_LOG_LEVEL` (giữ tên env của gốc để dùng chung scenario).

`help --format json` phải cùng shape với `conformance/golden/help.json` (79 verb, verb phẳng, `globalFlags` = `--json --quiet --no-color --db --log --help`). P1 khai báo đủ 79 verb; verb chưa cài trả exit 2 với `not implemented` — **khai `disposition: blocked` trong ledger**, không giả vờ chạy được.

```rust
// main.rs
fn main() -> std::process::ExitCode {
    let started = std::time::Instant::now();
    let args = cli::parse();
    let code = match cli::dispatch(&args) {
        Ok(code) => code,
        Err(e) => { output::error(&e); e.exit_code() }   // mặc định 2
    };
    output::elapsed(started, &args);                     // "done in Nms" → stderr, trừ --quiet
    std::process::ExitCode::from(code)
}
```

Mã lỗi: một `enum CliError { Usage, NotFound, Io, Corrupt, … }` với `exit_code()`; không dùng `anyhow` ở biên CLI để exit code luôn tường minh.

---

## 3. Config & scope — `config.layering`

```rust
pub struct Config {
    pub value: serde_json::Value,                 // đã merge
    pub sources: BTreeMap<String, Layer>,         // dot-path → layer cuối ghi
    pub warnings: Vec<String>,                    // key bị strip
}
pub enum Layer { Default, Project, ProjectLocal, Env, Flag }
pub fn load(cwd: &Path, overrides: &Overrides) -> Result<Config, CliError>;
```

- `defaults.json` nhúng (nguyên văn từ `spec_doc.md` §5.2).
- Merge: object đệ quy, array thay thế.
- Strip `PROJECT_LOCAL_ONLY_KEYS` khỏi layer `project` kèm warning (danh sách ở `spec_doc.md` §5.3). Scope-lock grant: P3; ở P1 layer `project-local` được đọc đầy đủ.

`scope.rs` — khi scan lần đầu (hoặc `init`) tạo:

| File | Nội dung |
|---|---|
| `.skill-map/.gitignore` | **byte-identical** với bản của gốc (copy từ oracle vào `src/schema/scope.gitignore`) |
| `.skill-map/settings.json` | chỉ ghi khi auto-detect lens: `activeProvider` + `activeProviderMarkers`, JSON indent 2, `\n` cuối |
| `.skill-map/skill-map.db` | §11 |
| `.skill-map/operations.log` | append 1 dòng NDJSON mỗi scan (§0) |
| `.skill-map/backups/skill-map-pre-migrate-v<N>.db` | bản sao DB trước khi chạy migration (file rỗng nếu DB mới) |

---

## 4. Walk — `scan.walk.ignore`

```rust
pub struct WalkedFile { pub rel: String, pub abs: PathBuf, pub size: u64, pub mtime_ms: i64 }
pub struct WalkResult { pub files: Vec<WalkedFile>, pub skipped: u32, pub oversized: Vec<(String,u64)>, pub truncated: bool }
pub fn walk(roots: &[PathBuf], ignore: &IgnoreStack, cfg: &ScanCfg) -> WalkResult;
```

1. Ignore stack theo thứ tự: bundled defaults → `.gitignore` (chỉ khi `scan.respectGitignore`) → `config.ignore` → `.skillmapignore`. Layer sau được `!` re-include. Bundled defaults gồm `.skill-map/`, `node_modules/`, `.git/`, `sm-tutorial/` (lấy danh sách chính xác từ bundle gốc — obligation `scan.walk.ignore`).
2. Duyệt cây (`std::fs::read_dir` đệ quy, song song theo thư mục con bằng `rayon::scope`), chỉ giữ extension mà provider đang tham gia đọc (`.md`, `.toml` khi lens codex) + `.sm`.
3. Symlink: `canonicalize`; nếu realpath không nằm trong scan root nào → bỏ (trừ `scan.followExternalSymlinks`).
4. `size > scan.maxFileSizeBytes` → `oversized`, không đọc.
5. Quá `scan.maxScan` → `truncated = true`, dừng nhận file.
6. **Sort `files` theo `rel` (byte order) trước khi trả** — điều kiện bắt buộc để output tất định khi dùng Rayon.

`filesWalked`: golden ghi 16, trong khi fixture chỉ có 8 `.md` + 5 `.sm` (+ 3 `.json`, `public/` bị ignore). Cách đếm chưa rõ (có thể đếm theo từng provider pass) — xem §17 #2, chốt bằng fixture nhỏ trước khi code.

---

## 5. Frontmatter & byte accounting — `parse.frontmatter`, `parse.backtick-balance`

```rust
pub struct Parsed<'a> {
    pub declared: bool,
    pub raw: &'a str,          // giữa 2 fence, không gồm '\n' cuối  → bytes.frontmatter
    pub body: &'a str,         // sau dòng fence đóng               → bytes.body, bodyHash
    pub value: serde_json::Value,   // {} nếu không có / parse lỗi
    pub issue: Option<FmIssue>,     // parse-error | malformed{hint} | invalid
}
pub fn parse(text: &str) -> Parsed<'_>;
```

- Dùng `gray_matter` để nhận diện block; **cắt lại theo byte gốc** để đảm bảo `raw`/`body` đúng như §0 (gray_matter có thể chuẩn hoá xuống dòng).
- Không bao giờ panic/throw với input hỏng; 4 lane theo `spec_doc.md` §12.1, hint đóng: `early-close`, `paste-with-indent`, `byte-order-mark`, `leading-blank-line`, `missing-close`.
- Thông báo lỗi parse không chứa raw input; strip control byte.
- Backtick balance: fence (``` hoặc ~~~, ≤3 space indent, closer cùng ký tự và ≥ độ dài) rồi inline span (sau khi blank fence + mask `\``). Báo lỗi đầu tiên, `data: {kind: "fence"|"inline", line}`.

### 5.1 `canon_yaml.rs` — `sidecar.hash-compat`

Emitter tối thiểu tái tạo `js-yaml dump(v, {sortKeys:true, lineWidth:-1, noRefs:true, schema:CORE_SCHEMA})` cho các kiểu JSON:

| Kiểu | Quy tắc (cần test vector từ js-yaml) |
|---|---|
| object | key sort theo UTF-16 code unit; `key: value`; object rỗng `{}` |
| array | `- item` thụt 2 khi là value của key (`tools:\n  - Read`); array rỗng `[]` |
| string thường | plain nếu an toàn; ngược lại `'single'` hoặc `"double"` theo quy tắc js-yaml |
| string nhiều dòng | block literal `\|` (thêm `-` nếu không kết thúc bằng `\n`, `+` nếu nhiều `\n`) |
| số / bool / null | `1`, `1.5`, `true`, `null` |

Đã xác nhận một ca (`content-editor.md`). **Đây là module rủi ro cao nhất**: giữ unknown mở cho tới khi bảng vector (≥ 100 ca sinh bằng chính js-yaml trong `oracle/node_modules/@skill-map/cli/dist`) pass 100%.

---

## 6. Mask & code regions — `extract.code-region-gate`

```rust
pub struct Regions { pub fences: Vec<Range<usize>>, pub inline: Vec<Range<usize>>, pub html: Vec<Range<usize>> }
pub fn regions(body: &str) -> Regions;                   // pulldown-cmark into_offset_iter
pub fn strip(body: &str, r: &Regions) -> String;         // blank code + html, CÙNG ĐỘ DÀI (giữ '\n')
pub fn inverse(body: &str, r: &Regions) -> String;       // chỉ giữ code, phần còn lại blank
pub fn context_at(r: &Regions, off: usize) -> Option<CodeCtx>; // InlineCode | CodeBlock
```

Blank = thay mỗi byte không phải `\n` bằng space ASCII (giữ offset byte và số dòng). HTML: chỉ comment và tag token, không nội dung giữa tag.

---

## 7. Model & JSON — `scan.result.json`

Thứ tự field = thứ tự key quan sát được trong golden (serde xuất theo thứ tự khai báo). Field `None` → bỏ hẳn (`skip_serializing_if`).

```rust
#[derive(Serialize)] #[serde(rename_all = "camelCase")]
pub struct ScanResult {
    pub schema_version: u32,            // 1
    pub scanned_at: i64,
    pub roots: Vec<String>,             // ["."]
    pub providers: Vec<String>,         // ["claude","antigravity","codex","opencode","agent-skills","markdown"]
    pub scanned_by: ScannedBy,          // {name, version, specVersion}
    pub tokenizer: String,              // "cl100k_base"
    pub active_provider: Option<String>,
    pub scan_ceiling: u32,
    pub scan_truncated: bool,
    pub max_render_nodes: u32,
    pub oversized_files: Vec<Oversized>,
    pub nodes: Vec<Node>,
    pub links: Vec<Link>,
    pub issues: Vec<Issue>,
    pub stats: Stats,                   // filesWalked, filesSkipped, filesOversized, nodesCount, linksCount, issuesCount, durationMs
}

pub struct Node {
    pub path: String, pub kind: String, pub provider: String,
    pub body_hash: String, pub frontmatter_hash: String,
    pub bytes: Triple,
    pub links_out_count: u32, pub links_in_count: u32, pub external_refs_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")] pub r#virtual: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")] pub derived_from: Option<Vec<String>>,
    pub frontmatter: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")] pub modified_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")] pub tokens: Option<Triple>,
    #[serde(skip_serializing_if = "Option::is_none")] pub sidecar: Option<SidecarOverlay>,
    #[serde(skip_serializing_if = "Option::is_none")] pub external_refs: Option<Vec<ExternalRef>>, // {url, line, originalTrigger}
}

pub struct Link {
    pub source: String, pub target: String, pub kind: LinkKind,   // invokes|references|mentions|points
    pub confidence: f64,                                          // xuất 1 (không phải 1.0) như JS
    pub sources: Vec<String>,                                     // short extractor id
    #[serde(skip_serializing_if = "Option::is_none")] pub raw: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub trigger: Option<Trigger>,
    #[serde(skip_serializing_if = "Option::is_none")] pub location: Option<Loc>,     // {line, offset}
    #[serde(skip_serializing_if = "Vec::is_empty")]   pub occurrences: Vec<Occurrence>, // {extractor, originalTrigger, location:{line}, context?}
    #[serde(skip_serializing_if = "Option::is_none")] pub resolved_target: Option<String>,
}

pub struct Issue {
    pub analyzer_id: String,            // short id
    pub severity: Severity,             // error|warn|info
    pub node_ids: Vec<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")] pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub fix: Option<Fix>,       // {summary}
    #[serde(skip_serializing_if = "Option::is_none")] pub data: Option<serde_json::Value>,
}
```

**Số:** JS in `1` cho `1.0` và `0.25` cho `0.25`. Dùng serializer số riêng: nếu `f.fract()==0 && |f|<2^53` → in số nguyên; còn lại dùng `ryu` (thứ tự chữ số khớp `Number.prototype.toString` cho các giá trị thực tế 0.1, 0.25, 0.5, 0.8, 0.85, 0.95).

**So sánh với oracle:** so JSON đã parse (giá trị + thứ tự mảng), sau khi bỏ `scannedAt`, `stats.durationMs`, `modifiedAtMs`. Thứ tự key object không bắt buộc nhưng vẫn giữ như trên.

### 7.1 Thứ tự tất định (quan sát từ golden, chốt thêm với fixture khác)

- **nodes:** theo thứ tự provider pass (`claude → antigravity → codex → opencode → agent-skills → markdown`), node ảo của provider đứng sau node thật của provider đó, trong mỗi nhóm sort theo `path`.
- **links:** nhóm theo `source` theo thứ tự nodes; trong một source sort theo short extractor id (`at-directive < markdown-link < mcp-tools < slash-command`), rồi theo vị trí xuất hiện.
- **issues:** theo thứ tự analyzer chạy (phase → id), trong analyzer theo node.

---

## 8. Providers & lens — `scan.lens.detect`, `scan.classify`

Bảng tĩnh, không trait:

```rust
pub struct Provider {
    pub id: &'static str,                 // "claude", …, "markdown"
    pub gated: bool,
    pub markers: &'static [&'static str],
    pub fallback: bool,                   // agent-skills
    pub subsumes: &'static [&'static str],// opencode → ["claude"]
    pub read: &'static [ReadRule],        // ext + parser + body_field
    pub kinds: &'static [Kind],           // name, identifiers, identifier_mismatch, ui
    pub resolution: &'static [(LinkKind, &'static [&'static str])],
    pub reserved: &'static [(&'static str, &'static [&'static str])],
    pub mcp_sources: &'static [(&'static str, McpDialect)],   // claude: ".mcp.json", ".claude/settings.json"
    pub classify: fn(&str) -> Option<&'static str>,
}
pub static PROVIDERS: [Provider; 6] = [CLAUDE, ANTIGRAVITY, CODEX, OPENCODE, AGENT_SKILLS, MARKDOWN];
```

Nội dung từng provider: `spec_doc.md` §7.3 (classify path lowercase, identifiers, resolution, reserved names — danh sách Claude đầy đủ).

Lens: đọc `activeProvider`; thiếu → detect marker (lọc fallback/subsumes) → 1 ứng viên: ghi `settings.json`; 0: `agent-skills` (không ghi); >1: TTY hỏi, non-TTY exit 2. Đổi lens → xoá zone `scan_*`. Drift marker → 1 warning/scan.

Gate: provider `gated` chỉ walk/classify khi `id == activeProvider`; `markdown` luôn chạy cuối. First-claim-wins bằng `HashSet<&str>` path đã claim (xử lý **tuần tự theo thứ tự provider**, song song bên trong một provider rồi merge theo path).

---

## 9. Extractors — `extract.markdown-link`, `extract.tokens`

Mỗi extractor là `fn(&NodeCtx, &mut Emit)`; danh sách tĩnh kèm gate lens. Body được truyền dạng `strip` hoặc `inverse` tuỳ extractor (§6).

| Short id | Lens | Miền | Kind | Emit conf. |
|---|---|---|---|---|
| `markdown-link` | all | strip | references | 0.95 |
| `backtick-path` | all | inverse | points | 0.85 |
| `external-url-counter` | all | strip | → `externalRefs` | — |
| `mcp-tools` | all | frontmatter `tools` | references → `mcp://<server>` | — |
| `slash-command` | claude, antigravity, opencode | strip | invokes | 0.8 |
| `backtick-slash` | claude, antigravity, opencode | inverse | invokes | 0.8 |
| `at-file` | codex, antigravity, claude | strip | references | — |
| `at-directive` | claude | strip | mentions (bare 0.5) / references (path 0.85) | |
| `backtick-mention` | claude | inverse | mentions | 0.5 |
| `dollar-skill` | codex | strip | invokes | 0.8 |
| `backtick-dollar` | codex | inverse | invokes | 0.8 |
| `tools-counter` | claude | frontmatter | contribution (P5) | — |

Grammar (`fancy-regex`, biên dịch một lần bằng `std::sync::LazyLock`): đúng nguyên văn `spec_doc.md` §9.3. Test vector: lấy từ `vendor/skill-map/src/kernel/util/__tests__` + `src/plugins/**/__tests__`.

`Emit` gom `Vec<Signal>`/`Vec<Link>` cục bộ cho mỗi node (Rayon `par_iter` trên nodes), merge theo thứ tự node sau đó → tất định.

`location.line` tính **theo file** (cộng số dòng frontmatter + 2 fence); `location.offset` là offset byte trong file (golden: `line 22, offset 672`).

---

## 10. Resolve & analyze — `norm.trigger`, `resolve.confidence`, `analyze.deterministic`

Thứ tự (đơn luồng, sau khi đã có toàn bộ node + link):
1. Signal resolver (overlap cluster) → link.
2. `dedupe_links`: key `(source, kind, normalizedTrigger or target)`; union `sources`, nối `occurrences`.
3. Tách URL → `externalRefs`.
4. `lift`: `confidence = 1.0`, tìm `resolvedTarget` (path match → root fallback cho `points` → name index theo `resolution` của lens, ưu tiên non-reserved), tính `broken`, `reserved_targets`.
5. Code-region gate: xoá mentions/invokes không resolve mà mọi occurrence ở code.
6. Analyzer phase `score` → fold ops (set → Σdelta → floor → ceil → clamp) → `detect` → `aggregate`.
7. Drop issue bị suppress qua `annotations.issueSuppressions`.

`norm.rs`: NFD → bỏ `Mn` → lowercase → `[-_\s]+` → space → trim.

Analyzer P1 (tất định): `reference-broken` (score, −0.75), `name-reserved` (score, −0.9), `name-collision`, `name-mismatch`, `link-kind-conflict`, `link-self-loop`, `reference-redundant`, `extractor-collision`, `schema-violation`, `annotation-field-unknown`, `annotation-orphan`, `annotation-stale` (contribution, không issue), `node-stability`, `link-counter`, `issue-counter` (aggregate). `observed-*` → P6 (cần session journal).

**Text của message/fix phải khớp từng chữ** với gốc (vd `"Reserved name; this command name is shadowed by the claude runtime built-in"`, `"\`docs/BACKLOG.md\`:\nL8: Broken reference; target not found in the graph or on disk"`). Lấy nguyên văn từ `src/plugins/core/analyzers/*/*.texts.ts` của repo gốc vào `src/schema/texts.rs`.

---

## 11. Store — `db.schema`

- `rusqlite` + `include_str!("schema/001_initial.sql")` (DDL gốc nguyên văn, đủ 23 bảng kể cả `sqlite_sequence`).
- Mở DB: `PRAGMA journal_mode=WAL`; `user_version` 0 → backup (`backups/skill-map-pre-migrate-v1.db`) → chạy DDL → `user_version=1` → insert `config_schema_versions ('kernel','kernel',1,'initial',now)`.
- `schema_fingerprint` = sha256 của DDL đã bundle (so với gốc `2340cc3e…`; nếu dùng đúng file SQL gốc sẽ khớp — obligation `scan.schema-drift`).
- Persist scan: **một transaction**, `DELETE` toàn bộ `scan_*` rồi insert; `scan_meta` id=1.
- Read verbs (`check`, `list`, `show`, `graph`, `export`) đọc từ DB, không walk lại; DB vắng → exit 5 với đúng thông báo ở §0.

Mapping cột ↔ struct: `spec_doc.md` Phụ lục A; JSON lưu dạng `TEXT` (`frontmatter_json`, `sources_json`, …).

---

## 12. Formatters — `graph.formatters`

- `json` = `ScanResult` đọc lại từ DB (bằng `scan --json` trừ whitespace).
- `mermaid` (đã có golden capture `ev_2e947e48…`):
  ```
  flowchart LR
  %% skill-map graph: {nodes} nodes, {links} links, {issues} issues
    n{i}["{label}"]           ← mọi node + mọi target chưa resolve/trigger (vd "/publish", "@content-editor", "docs/BACKLOG.md")
    n{a} -->|"{kind}"| n{b}
    classDef kind_<k> fill:…,stroke:…,color:…;   ← chỉ kind có mặt, sort theo tên
    class n{i} kind_<k>;
  ```
  Đánh số node theo thứ tự label sort (path & trigger trộn, byte order). Màu classDef copy nguyên văn từ golden.
- `ascii` (mặc định), `dot`: capture oracle thêm trước khi code.

---

## 13. Pretty output

Đi theo text gốc (lấy từ captures): banner ASCII chỉ ở `example`/`init`; `check` in `sm check: N error · M warning`, nhóm theo node, ký hiệu `✕`/`⚠`, cột căn theo analyzer id dài nhất; tip cuối. Tên chương trình trong text: **giữ `sm`** trong P1 để so terminal, đổi thành `skflow` sau khi P1 xanh (khi đó thêm pattern normalization `sm`→`skflow` vào scenario).

---

## 14. Khung các pha sau

| Pha | Module mới | Ghi chú thiết kế |
|---|---|---|
| P2 | `cache.rs`, `watch.rs` (`notify` + debounce 300 ms) | cache key 3 chân; rename heuristic; ndjson ScanResult mỗi batch |
| P3 | `sidecar.rs` ghi, `consent.rs`, `lock.rs` | `applyPatch` (deep-merge, `null` = xoá, `.tmp` + rename, lock theo path); `bump`, `sidecars *`, `issues dismiss` |
| P4 | `jobs.rs`, `preamble.rs` | atomic claim 1 câu SQL `UPDATE … RETURNING`; nonce 128 bit (`getrandom`); preamble nguyên văn |
| P5 | `server.rs` (axum + tokio), `mcp.rs` (`rmcp`) | giữ đúng shape REST envelope để chạy UI Angular gốc; tokio chỉ vào ở pha này |

---

## 15. Ánh xạ obligation → module (P1)

| Obligation | Module | Test |
|---|---|---|
| `cli.binary.help` | `cli.rs` | golden `help.json` (so cấu trúc) |
| `cli.exit-codes` | `main.rs`, `cli.rs` | scenario: no-DB=5, flag sai=2, issue error=1 |
| `config.layering` | `config.rs` | unit + scenario key cấm trong `settings.json` |
| `scan.walk.ignore` | `walk.rs` | fixture ignore / oversize / symlink |
| `scan.lens.detect` | `provider.rs`, `scope.rs` | fixture `lens-*` |
| `scan.classify` | `provider.rs` | unit bảng path → kind |
| `parse.frontmatter` | `frontmatter.rs` | fixture `malformed-*` |
| `parse.backtick-balance` | `frontmatter.rs` | unit |
| `sidecar.hash-compat` | `canon_yaml.rs` | vector js-yaml (≥100) |
| `extract.markdown-link` | `extract.rs` | unit + golden |
| `extract.tokens` | `extract.rs` | vector test gốc |
| `extract.code-region-gate` | `mask.rs`, `resolve.rs` | unit |
| `norm.trigger` | `norm.rs` | bảng §10 spec_doc |
| `resolve.confidence` | `resolve.rs` | golden (1 / 0.25) |
| `analyze.deterministic` | `analyze.rs` | mỗi analyzer 1 fixture |
| `scan.result.json` | `model.rs` | so `conformance/golden/example/scan.json` |
| `graph.formatters` | `format.rs` | golden mermaid |
| `db.schema` | `store.rs` | `user_version`, fingerprint |
| `cli.browse` | `cli.rs`, `store.rs` | capture `list/show/check/export` |

---

## 16. Thứ tự code đề xuất trong P1

1. `model.rs` + `norm.rs` + `canon_yaml.rs` (thuần, test được ngay; khoá rủi ro hash sớm)
2. `frontmatter.rs` + `mask.rs` + `tokens.rs`
3. `provider.rs` + `walk.rs` + `scope.rs` + `config.rs`
4. `extract.rs` → `resolve.rs` → `analyze.rs`
5. `store.rs` + `format.rs` + `cli.rs` + `output.rs`
6. Chạy scenario REA cho `skflow`, `compare_process_captures` với `ev_de62bcdb…` (scan), `ev_3c782716…` (check), `ev_2e947e48…` (graph), `ev_abdde906…` (check no-DB)

## 17. Câu hỏi còn mở

| # | Câu hỏi | Ảnh hưởng |
|---|---|---|
| 1 | Dòng `done in Nms` — so terminal thế nào? (`unk_4a63d144…`) | nếu không giải, claim chỉ tính dimension exit + filesystem + JSON |
| 2 | `filesWalked = 16` đếm những gì (fixture có 8 `.md` + 5 `.sm` + 3 `.json`; 8 node thật + 1 node ảo)? | cần thêm fixture nhỏ để xác định |
| 3 | Danh sách ignore mặc định chính xác | đọc từ bundle trước khi viết `walk.rs` |
| 4 | Token count `tiktoken-rs` vs `gpt-tokenizer` có lệch không | so trên fixture; nếu lệch phải port bảng BPE |

---

## 18. Trạng thái tại `f1c042a`+ và việc còn lại (cập nhật 2026-11-13)

### 18.1 Đã xong (đã kiểm differential với `sm` 1.12.7)

| Pha | Nội dung | Bằng chứng |
|---|---|---|
| P1 | scan / check / list / graph ×4, store SQLite, formatters | `rea/compare/SUMMARY.md` (REA 27/31, phần còn lại chỉ khác chia frame PTY / BOM) |
| P2 | cache tăng dần theo node (mtime gate, `scan_extractor_runs`), rename high/medium/ambiguous, orphan, migrate `state_*`, `scan --full/--changed/--allow-empty/--watch`, `watch` | `conformance/scenarios/renames.sh`, `watch.sh` — byte-identical |
| P3 | `bump`, `sidecars refresh/annotate/prune`, `issues dismiss/undismiss/suppressions`, `orphans` / `reconcile` / `undo-rename`, consent gate (`scope.lock.json`, tương thích grant của `sm`) | `conformance/scenarios/{bump,sidecars,issues,orphans}.sh` — giống hệt |
| P4 (một phần) | `agent install/uninstall/status`; `jobs submit/list/show/preview/claim/status/cancel/fail`; catalog extension (`src/schema/catalog.json` trích bằng `tools/extract-catalog.mjs`) | bộ conformance spec: **asm 24/51** (`node conformance/run-spec.mjs asm`), `sm` 51/51 |

### 18.2 P4 — còn dở

- **`record`** (chưa có): kiểm nonce (exit 4), validate report theo schema của extension (fail `report-invalid`), ghi `state_executions`, write-through `state_summaries` / `state_findings` / tags, envelope ndjson `--json` (`run.started` …). Nền đã có: `validate::validate_schema` + registry các schema spec (`report-base`, `summaries/markdown`, `findings/report`) — commit WIP sau `f1c042a`. Nguồn gốc: `RecordCommand`, `RECORD_TEXTS` (dist/cli.js ~18883), `recordJobTerminal`, `insertExecution`, `upsertSummaryForNode`, `writeFindingsForNode` (dist/sqlite-*.js).
- **`findings`** (list + `resolve/reopen/dismiss/undismiss/suppressions/prune/clear`).
- **`history`**, **`history stats`**.
- Fixer jobs: mục `## Findings to resolve` / `## Issues to resolve`, `--finding`, supersede job queued, `--auto-fix`; `jobs prune`; tự gỡ suppression khi submit finder; job `skill:`; đẩy `job.*` lên server (`pushJobEvent`, cần P5).
- Case conformance đang trượt vì phần này: `history-record-schema`, `history-stats-schema`, `record-findings-envelope-gate`, `record-findings-report`, `record-nonce-mismatch`, `record-report-schema-gate`, `record-run-envelope`, `record-tags-report` (cần thêm `plugins enable`).

### 18.3 P5 — server (chưa làm)

- `serve`: HTTP BFF (route REST, envelope `rest-envelope.schema.json`), WebSocket, file `.skill-map/serve.json` (`serve-info.schema.json`), nhận `POST /api/job-events`.
- MCP server (`spec/mcp-server.md`).
- Chạy giao diện Angular gốc (`oracle/.../cli/dist/ui`) trên server Rust (axum + tokio, chỉ đưa tokio vào ở pha này).
- Case: `rest-envelope-schema`, `serve-info-schema`, `skill-actions-catalog`.

### 18.4 P6 — plugin và activity (chưa làm)

- Chạy thật plugin JS drop-in (hiện `catalog.rs` chỉ đọc theo quy ước `prompt.md` + `report.schema.json`), kiểm manifest, slot/contribution UI.
- `plugins list/show/doctor/enable/disable/trust/untrust/create/slots/upgrade/config`.
- `activity install/uninstall/status`, analyzer `observed-*`, `enrich`.
- Case: `score-phase-confidence`, `plugin-*`, `view-*`, `extension-*-manifests`, `extension-manifest-enable-gate`, `catalog-slots-input-types`, `enrich-report-schema`, `github-enrichment-recorded`.

### 18.5 Lệnh khác chưa có

`config list/get/show/set/reset`, `init`, `doctor`, `show`, `export`, `db backup/restore/reset/shell/dump/migrate`, `scan compare-with`, `actions list/show`, `hooks install`, `conformance run`, `help` (theo định dạng gốc), `tutorial`, `example`. Case liên quan: `project-config-schema`, `conformance-result-schema`, `elapsed-time-reporting`.

### 18.6 Lệch đã biết so với bản gốc

- Đếm token file có BOM lệch 1 (tiktoken-rs không coi U+FEFF là khoảng trắng).
- Key YAML > 1024 ký tự; cách viết một số lỗi YAML ngoài trường hợp dấu `:` không quote.
- Analyzer `schema-violation` (và `observed-*`) chưa port.
- Cache: khi chỉ một extractor đổi setting (partial hit), `asm` chạy lại cả node.
- Chưa tự dựng lại DB khi schema fingerprint lệch (`rebuildOnDrift`).
- Chưa thử câu hỏi xác nhận ở TTY thật (mới kiểm đường non-TTY).

### 18.7 Cách kiểm khi làm tiếp

```bash
cargo build --release && cargo test --release
node conformance/run-spec.mjs asm            # cả bộ; thêm <case-id> để chạy riêng
node conformance/run-spec.mjs sm <case-id>   # xem bản gốc pass
conformance/scenarios/<name>.sh <sm|asm> <workdir>   # differential từng nhóm verb
```
