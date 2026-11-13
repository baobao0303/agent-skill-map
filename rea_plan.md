# skflow — Phân tích: dùng REA MCP để dựng lại `skill-map` bằng Rust

> Đầu vào: [`spec_doc.md`](spec_doc.md) (đặc tả dịch ngược từ `skill-map` 1.12.7).
> Mục tiêu: `skflow` — bản Rust viết mới hoàn toàn, **đủ chức năng trước**, rồi mới tối ưu.
> Vai trò REA: **thu bằng chứng từ bản gốc → biến thành danh sách nghĩa vụ (obligation) → kiểm chứng bản Rust tương đương**. REA **không sinh code Rust**; code do agent/dev viết từ `spec_doc.md`.

---

## 1. Vì sao dùng REA (và giới hạn của nó)

| REA làm được | REA không làm |
|---|---|
| Ghi lại hành vi thật của `sm` gốc (stdout, exit code, file tạo ra trong `.skill-map/`) dưới dạng Evidence có hash | Viết code Rust |
| So sánh từng lần chạy `sm` vs `skflow` theo chiều: terminal, exit, filesystem, process, HTTP/WebSocket | Chứng minh tương đương toàn cục — chỉ chứng minh từng *claim* đã khai báo |
| Biến spec thành **ledger nghĩa vụ** có kiểm tra "fail closed" (thiếu owner, thiếu fixture, thiếu verifier → báo đỏ) | Tự hiểu ngữ nghĩa nghiệp vụ |
| Đo **độ phủ tái dựng** theo boundary (CLI, BFF, MCP) kèm proof đóng gói | Chạy UI Angular tự động như người dùng (cần thêm CDP/browser) |
| Ghi lại câu hỏi chưa giải (residual unknown) và kiểm lại khi có bằng chứng mới | |

**Nguyên tắc:** bản gốc `@skill-map/cli@1.12.7` là **oracle**. Mọi hành vi skflow phải khớp oracle trên cùng fixture, trừ các khác biệt đã ghi rõ là *non-goal*.

---

## 2. Bộ công cụ REA dùng cho dự án

| Giai đoạn | Tool MCP | Dùng để |
|---|---|---|
| Nhận diện artifact | `inspect_artifact`, `analyze_javascript_application` (đã chạy: `ev_56894f0b…`), `inspect_analysis_view` | Định danh gói npm gốc (sha256), danh sách module, entrypoint |
| Lần theo tính năng | `trace_application_feature`, `trace_javascript_semantics` | Từ seed (`"scan"`, `"/api/nodes"`, `"skillmap://graph"`, `"state_jobs"`) tìm module trong bundle chịu trách nhiệm → điền `target.location` cho obligation |
| Thu hành vi gốc | **`capture_process_scenario`** | Chạy `sm <verb>` dưới PTY với fixture, quan sát `filesystem_observation_paths` (`.skill-map/`, `*.sm`), chuẩn hoá pid/port/path/thời gian |
| Thu hành vi HTTP/UI | `capture_process_scenario` (HTTP/WS evidence) + `observe_web_session` / `analyze_web_bundle` qua CDP | `sm serve` + gọi `/api/*`, `/ws`, `/mcp` |
| Lập nghĩa vụ | **`build_reconstruction_obligation_ledger`** | Mỗi tính năng trong spec = 1 obligation, gắn owner Rust + fixture + verifier |
| Ghi điểm chưa rõ | `record_unknown`, `verify_unknown_resolution` | vd hash frontmatter js-yaml, rename heuristic |
| So sánh | **`compare_process_captures`** | Gốc (left) vs skflow (right) cho từng scenario; `trace_spec` khi thứ tự sự kiện quan trọng (watch, job events) |
| Kiểm chứng | **`verify_reconstruction`** | Tập claim hành vi (`terminal`, `exit`, `filesystem`) trỏ tới evidence so sánh |
| Đo độ phủ | **`evaluate_reconstruction_coverage`** | Boundary `replacement.cli` / `.bff` / `.mcp` + proof: build, clean-install, authority-independence (skflow không cần Node) |
| Kiểm binary Rust | `inspect_rust_binary` | Danh sách crate thật trong `target/release/skflow` (cần `cargo auditable build`) |
| Lưu trữ | `export_evidence_bundle` | Ghi bundle evidence vào repo để tái lập |

---

## 3. Quy trình (vòng lặp cho mỗi pha trong `spec_doc.md` §2)

```
┌─ (A) ORACLE ────────────────────────────────────────────────────────┐
│ fixture  ──► capture_process_scenario( sm <verb> … )  ──► ev_orig_* │
└─────────────────────────────────────────────────────────────────────┘
          │
┌─ (B) LEDGER ────────────────────────────────────────────────────────┐
│ spec_doc §x ──► reviewed_obligations[] ──► build_reconstruction_     │
│                                            obligation_ledger         │
│   → danh sách đỏ = việc phải làm                                     │
└─────────────────────────────────────────────────────────────────────┘
          │
┌─ (C) BUILD ─────────────────────────────────────────────────────────┐
│ viết Rust theo spec_doc → cargo test (unit, có vector từ test gốc)  │
└─────────────────────────────────────────────────────────────────────┘
          │
┌─ (D) VERIFY ────────────────────────────────────────────────────────┐
│ capture_process_scenario( skflow <verb> … ) ──► ev_rust_*           │
│ compare_process_captures(ev_orig_*, ev_rust_*) ──► ev_cmp_*         │
│ verify_reconstruction(claims → ev_cmp_*)                            │
│ evaluate_reconstruction_coverage(boundary)                          │
│   fail / unknown → quay lại (C) hoặc record_unknown                 │
└─────────────────────────────────────────────────────────────────────┘
```

Một obligation chỉ được đánh "xong" khi **verifier pass** trên evidence so sánh còn hạn — không dựa vào cảm nhận.

---

## 4. Danh mục obligation (khởi điểm, ánh xạ từ `spec_doc.md`)

Mỗi dòng sẽ thành một `reviewed_obligation`. `layer` theo enum của REA. Case kinds: P = positive, N = negative, M = malformed, C = cancellation, T = teardown.

### P1 — Scan core
| ID | Spec | layer | Cases | Verifier | Oracle scenario |
|---|---|---|---|---|---|
| `cli.binary.help` | §18 | cli | P, N | packaged-process | `sm --help`, `sm help --format json`, verb lạ |
| `cli.exit-codes` | §18.2 | cli | P, N, M | packaged-process | DB vắng (5), flag sai (2), issue error (1) |
| `config.layering` | §5 | persistence | P, M | integration | settings.json + local + env; key cấm trong file commit bị strip |
| `scan.walk.ignore` | §8 | runtime | P, N | integration | `.skillmapignore`, `!re-include`, file > 1 MiB, symlink thoát root |
| `scan.lens.detect` | §7.1 | application | P, N | packaged-process | `.claude/` / `.codex/` / cả hai / không marker |
| `scan.classify` | §7.3 | application | P | unit | bảng path → kind cho 6 provider |
| `parse.frontmatter` | §12.1 | application | P, M | unit | 4 lane + 5 hint |
| `parse.backtick-balance` | §12.1 | application | P, M | unit | fence/inline lẻ |
| `extract.markdown-link` | §9.2, 9.4 | application | P, N | unit | relative / leading-`/` / image / scheme |
| `extract.tokens` | §9.3 | application | P, N | unit | 4 grammar, vector từ test gốc |
| `extract.code-region-gate` | §9.6 | application | P, N | integration | trigger trong backtick không resolve bị xoá |
| `norm.trigger` | §10 | application | P | unit | bảng ví dụ |
| `resolve.confidence` | §11 | application | P, N | integration | 1.0 / 0.25 / 0.1, fold op |
| `analyze.deterministic` | §12.2 | application | P, N | integration | mỗi analyzer 1 fixture |
| `scan.result.json` | §6.4 | protocol | P | packaged-process | `sm scan --json` khớp schema + khớp gốc sau chuẩn hoá |
| `graph.formatters` | §13.2 | cli | P | packaged-process | `sm graph --format ascii\|json\|mermaid\|dot` |
| `db.schema` | §17 | persistence | P | integration | DDL + `sm version` (`dbSchema`) |
| `cli.browse` | §18.4 | cli | P, N | packaged-process | `list`, `show`, `check`, `export` |

### P2 — Incremental & watch
| ID | Spec | layer | Cases | Verifier |
|---|---|---|---|---|
| `scan.incremental.cache` | §14 | persistence | P | integration (đếm file đọc lại = 0) |
| `scan.schema-drift` | §14 | persistence | P, N | packaged-process |
| `scan.watch` | §18.4 | process | P, C, T | packaged-process + `trace_spec` (ndjson theo batch, SIGINT dừng sạch) |

### P3 — Sidecar
| ID | Spec | layer | Cases | Verifier |
|---|---|---|---|---|
| `sidecar.read-overlay` | §15 | persistence | P, M | integration |
| `sidecar.hash-compat` | §12.1 | persistence | P | integration — **`.sm` do gốc ghi phải khớp hash skflow tính** |
| `sidecar.write-consent` | §15 | cli | P, N | packaged-process (`--yes`, non-TTY exit 2) |
| `sidecar.bump` | §15 | cli | P | packaged-process + filesystem |
| `issues.suppress` | §12 | application | P | integration |

### P4 — Job queue
| ID | Spec | layer | Cases | Verifier |
|---|---|---|---|---|
| `jobs.submit` | §16.2 | persistence | P, N | packaged-process (exit 3 duplicate) |
| `jobs.claim.atomic` | §16.3 | persistence | P, N | integration — 2 tiến trình claim song song, đúng 1 thắng |
| `jobs.record` | §16.4 | protocol | P, N, M | packaged-process (nonce sai → 4, report sai → `report-invalid`) |
| `jobs.preamble` | §16.5 | protocol | P | integration — `contentHash` khớp gốc |
| `findings.lifecycle` | §16.6 | persistence | P | integration |

### P5 — Server
| ID | Spec | layer | Cases | Verifier |
|---|---|---|---|---|
| `bff.read-routes` | §19 | protocol | P, N | protocol (HTTP evidence) |
| `bff.write-routes` | §19 | protocol | P, N | protocol (412 `confirm-required`) |
| `bff.ws` | §19 | protocol | P, T | protocol (WebSocket evidence) |
| `mcp.tools` | §20 | protocol | P, N | protocol (JSON-RPC) |
| `ui.compat` | §19 | renderer | P | renderer — UI Angular gốc chạy trên skflow |

### Non-goal (khai `disposition: out-of-scope` / boundary `non-goal`)
Telemetry Sentry/PostHog, update-check npm, `db browser`, nội dung `tutorial`/`example` (giai đoạn đầu).

---

## 5. Bộ fixture oracle

Lưu tại `conformance/fixtures/` (commit), mỗi fixture là một thư mục project:

| Fixture | Nguồn | Phủ |
|---|---|---|
| `example/` | sinh bằng `sm example` | graph liên thông, sidecar, agent/command/skill |
| `spec-cases/*` | copy `spec/conformance/{cases,fixtures}` (49 case) | kernel-agnostic |
| `lens-*` | tự dựng | claude / codex / antigravity / opencode / agent-skills / không marker / nhiều marker |
| `malformed-*` | tự dựng | frontmatter hỏng 5 kiểu, backtick lẻ, YAML BOM |
| `hostile-*` | tự dựng | symlink ra ngoài root, key cấm trong `settings.json` |
| `big/` | sinh | > `maxScan`, file > 1 MiB |

**Chuẩn hoá trước khi so** (khai trong `normalization` của `capture_process_scenario`): `scannedAt`, `durationMs`, `ran_at/created_at`, đường dẫn tuyệt đối, pid, port, nonce, job id (uuid). Phần còn lại phải khớp byte.

Thông số chạy chung:
```json
{
  "environment": { "SKILL_MAP_TELEMETRY": "0", "SM_NO_UPDATE_CHECK": "1", "NO_COLOR": "1" },
  "filesystem_observation_paths": [".skill-map", "."],
  "normalization": { "paths": true, "pids": true, "ports": true, "time_bucket_ms": 1000 },
  "timeout_ms": 60000
}
```

---

## 6. Sẵn sàng của môi trường (đã kiểm tra)

| Thứ | Trạng thái |
|---|---|
| Node.js | v26.8.2 ✓ (gốc cần ≥ 24) |
| npm | 11.19.1 ✓ |
| Rust / cargo | 1.97.1 ✓ |
| REA CLI | `~/.local/bin/rea` ✓ ; MCP `rea` đã kết nối |
| Gói gốc 1.12.7 | đã tải (tarball), **chưa cài dependency** |
| Repo gốc | đã clone (thư mục scratch phiên trước — nên chuyển vào `vendor/`) |

**Việc cần bạn đồng ý trước khi làm (A):** để chạy được oracle phải `npm install @skill-map/cli@1.12.7` vào một thư mục cô lập (vd `oracle/`, gitignore). Lệnh này tải ~vài chục package từ npm và chạy script build native (`@parcel/watcher`, `esbuild`). Khuyến nghị chạy với `--ignore-scripts` trước; nếu watcher cần native thì cho phép riêng. Có thể chạy trong **cua sandbox** nếu muốn cách ly hoàn toàn.

---

## 7. Ràng buộc từ `.claude/CLAUDE.md` và ảnh hưởng

| Quy định | Ảnh hưởng tới spec |
|---|---|
| Dùng **Rayon** cho duyệt file song song | Walk + extract song song; **phải sort tất định** trước khi persist/xuất JSON, nếu không `compare_process_captures` sẽ fail |
| Dùng **pulldown-cmark** | Hợp cho fence/inline-code detection và offset; nhưng strip/inverse-mask phải giữ *cùng độ dài* — dùng offset của pulldown-cmark để blank thay vì render lại |
| Dùng **gray_matter** | Tách frontmatter OK. Nhưng `frontmatterHash` cần YAML dump giống js-yaml 5 (`sortKeys`, `CORE_SCHEMA`) → gray_matter không làm được phần này; cần emitter canonical riêng. Obligation `sidecar.hash-compat` sẽ bắt lỗi này |
| Zero unrequested abstractions | Không làm plugin runtime (WASM/subprocess) ở P1–P5; chỉ built-in |

---

## 8. Rủi ro và cách REA giúp bắt

| Rủi ro | Phát hiện bằng |
|---|---|
| Hash frontmatter khác js-yaml | `sidecar.hash-compat` + `record_unknown` (severity high) cho tới khi pass |
| Regex lookbehind port sai | `extract.tokens` unit + so `ScanResult` trên fixture |
| Thứ tự không tất định (Rayon) | `compare_process_captures` fail ở chiều terminal/filesystem |
| Atomic claim sai | `jobs.claim.atomic` với 2 process song song |
| Shape JSON lệch làm UI gốc vỡ | `bff.read-routes` + `ui.compat` |
| Bản gốc cập nhật version | Pin oracle 1.12.7; khi nâng dùng `compare_application_versions` để xem bundle đổi gì |

---

## 9. Thứ tự thực hiện đề xuất

1. **Dọn nền:** chuyển repo gốc vào `vendor/skill-map` (gitignore), tạo `oracle/` cài `@skill-map/cli@1.12.7` (cần bạn duyệt §6).
2. **Oracle P1:** dựng 6–8 fixture, chạy `capture_process_scenario` cho các verb P1, `export_evidence_bundle` → `rea/evidence/p1-oracle.json`.
3. **Ledger P1:** `build_reconstruction_obligation_ledger` với 18 obligation P1 → danh sách đỏ.
4. **Viết spec Rust** `spec_rust.md`: chuyển `spec_doc.md` thành thiết kế crate/module/struct cụ thể theo CLAUDE.md, mỗi mục ghi obligation ID.
5. **Code P1** → `cargo test` → capture skflow → compare → verify → coverage `replacement.cli` (một phần).
6. Lặp lại cho P2…P5.

Kết quả mong đợi mỗi pha: một evidence bundle + ledger xanh + báo cáo coverage có thể commit và tái lập.
