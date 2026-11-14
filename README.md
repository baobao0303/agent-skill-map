# agent-skill-map

> High-performance Rust CLI & Visual Pipeline Engine for AI Agent Skills, Prompt Topology & Dynamic Execution.
> Inspired by `skill-map` (https://skill-map.ai) and powered by `@foblex/flow` visual canvas primitives.

## 1. Build

```bash
cargo build --release
```

File chạy nằm ở `target/release/agent-skill`. Cho tiện, thêm nó vào PATH hoặc đặt alias:

```bash
alias agent-skill=/home/baoltb/Desktop/agent-skill-map/target/release/agent-skill
```

## 2. Dùng trên một project

Vào thư mục project muốn quét (ví dụ thư mục có `.claude/skills`, `.claude/agents`):

```bash
agent-skill scan
```
Quét toàn bộ và lưu đồ thị vào DB.

```bash
agent-skill list
```
Liệt kê các node đã quét.

```bash
agent-skill check
```
Xem các lỗi tìm được, ví dụ link gãy hoặc reference không tồn tại.

```bash
agent-skill graph
```
In đồ thị ra.

```bash
agent-skill watch
```
Theo dõi thay đổi và tự quét lại.

Các lệnh khác:
- `orphans`, `issues`: xử lý file mồ côi, file bị đổi tên, và tắt cảnh báo cho từng node.
- `bump`, `sidecars`: quản lý file sidecar `.sm`, gồm version và hash.
- `agent`, `jobs`, `record`, `findings`, `history`: hàng đợi job cho AI agent xử lý.
- `config`, `plugins`, `enrich`, `serve`: cấu hình, plugin và HTTP server cục bộ.

Lệnh nào cũng nhận `--json` để in ra máy đọc được, và `--db <file>` để đổi vị trí DB.
Xem chi tiết một lệnh bằng `agent-skill <lệnh> --help`.

## 3. Phát triển và kiểm tra (cho người sửa code)

Chạy test và lint:

```bash
cargo test && cargo clippy
```

Chạy bộ conformance chính thức (`agent-skill` là bản Rust, `sm` là bản gốc):

```bash
node conformance/run-spec.mjs agent-skill
```

So từng kịch bản giữa `sm` và `agent-skill` bằng các file trong `conformance/scenarios/`:

```bash
conformance/scenarios/jobs.sh agent-skill /tmp/w-agent-skill
```

Bản gốc `sm` 1.12.7 nằm trong `oracle/node_modules` và không được sửa.
Nếu clone sang máy mới mà thiếu thư mục này, cài lại:

```bash
cd oracle && npm i @skill-map/cli@1.12.7 --ignore-scripts
```

## 4. Tài liệu & Lưu ý

- Toàn bộ 51/51 conformance test cases và các kịch bản so sánh (`jobs.sh`, `bump.sh`, `sidecars.sh`, `issues.sh`, `orphans.sh`, `renames.sh`, `watch.sh`) đã khớp với `sm` byte-by-byte.
- Chi tiết thiết kế và trạng thái các pha: xem `spec_rust.md`.
