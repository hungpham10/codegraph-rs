# AGENTS.md — CodeGraph

Rust workspace: tree-sitter parse → semantic graph (global symbol ID + call chain) trong `.codegraph/`,
phục vụ AI agent qua MCP; CLI `codegraph`. 15 crate, binary duy nhất.

## Bắt buộc: CodeGraph MCP là công cụ ĐẦU TIÊN

Trước khi grep/read để hiểu code, gọi MCP:

| Câu hỏi | Tool |
|---|---|
| Symbol ở đâu / signature | `codegraph_search_symbol`, `codegraph_symbol` |
| Ai gọi / gọi ai | `codegraph_callers`, `codegraph_callees` |
| Sửa X ảnh hưởng gì | `codegraph_impact` |
| Luồng điều khiển / trace | `codegraph_flow`, `codegraph_trace_full_flow` |
| Tìm theo marker | `codegraph_search_flow` |
| Context gói gọn | `codegraph_context` |
| Review diff | `codegraph_graphcode_diff` |
| Bind / re-index | `codegraph_init` (index mặc định **false**), `codegraph_index` |

- Index là ảnh chụp. Vừa sửa code mà chưa `codegraph_index` → kết quả cũ; đọc file để xác nhận.
- Grep/read vẫn đúng cho: text literal, `docs/`, `config.toml`, file ngoài index.
- Search lớn: `timeout_ms` + retry đúng args kèm `resume`.
- Đừng tin số liệu trong docs (tool count, version, language count) — đọc source.

## Build & verify

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings   # gate CI
cargo fmt --all
```

- Toolchain: `codegraph-graph`, `codegraph-docs`, `codegraph-bench` dùng **edition 2024** → cần Rust ≥ 1.85.
  `rust-version = "1.80"` trong `Cargo.toml` là số sai, đừng tin.
- `web/` là UI Svelte (npm) — chỉ cần khi sửa UI: `scripts/build-web.sh` sinh
  `crates/codegraph-web/assets/` (không commit) rồi `rust-embed` nhúng lúc compile.
- Binary `codegraph` mà MCP host đang chạy là bản **đã cài**, không phải `target/debug`. Sửa tool surface
  (`crates/codegraph-mcp/src/tools.rs`) mà không rebuild/reinstall thì agent vẫn thấy tool cũ.

## Test quirks

- `cargo test -p codegraph` chạy **0 test** — CLI chưa có test. Đừng coi đó là đã verify.
- Backend cần service thật + env:
  `TEST_REDIS_DSN=redis://127.0.0.1:6379` (feature `redis`);
  `TEST_RDBMS_DSN=...` + `TEST_RDBMS_REPO_ID=1` (feature `postgres`/`mysql`).
  Chạy `#[ignore]`: `cargo test -p codegraph-graph --features postgres --test rdbms -- --ignored`.
  Lưu ý: 4 test `storage::redis::tests::*` trong `crates/codegraph-graph/src/storage/redis.rs` KHÔNG `#[ignore]`,
  hardcode `redis://127.0.0.1:6379/15` → fail `Connection refused` nếu không có redis-server (pre-existing,
  không phải port contention). Chạy lại standalone để xác nhận.
- `crates/codegraph-binary/tests/extract.rs` cần `radare2` trên PATH (`--ignored`).
- `crates/codegraph-mcp/src/response_tests.rs` assert số tool đăng ký — thêm/bớt tool phải sửa số đó.
- Test config/flow cần `install_current(...)` và reset `install_current(None)`; classifier là thread-local.

## Quy trình verify + PR (bắt buộc, đúng thứ tự)

1. `cargo fmt --all`
2. `cargo clippy --workspace --all-targets -- -D warnings`
   - storage feature-gated: `cargo clippy -p codegraph-graph --features postgres,mysql,redis --tests -- -D warnings`
3. `cargo test --workspace` — **toàn bộ** unit + integration (không dùng `--lib` để kết luận)
4. Regression đúng vùng vừa sửa:
   - extract/config/marker → `cargo test -p codegraph-extract --test effects_config --test chains --test dynamic_dispatch`
   - trace/sandbox → `cargo test -p codegraph-graph --test trace_flow_tests` + `cargo test -p codegraph-sboxes`
   - backend → `cargo test -p codegraph-graph --features sqlite,lmdb`
   - lưu ý: 4 test `storage::redis` fail khi chạy song song toàn workspace (port contention, pre-existing) → chạy lại standalone để xác nhận.
5. Tạo nhánh → commit (conventional commits) → mở PR.
6. Chờ GitHub Actions xanh (test matrix + clippy + web build).
7. Chờ review benchmark (Criterion/CodSpeed) và review Codecov — coverage không được giảm.
8. Chỉ merge sau khi 6–7 xanh.

## Gotchas kiến trúc

- Config chỉ có **một bảng** `[[effect_rules]]`: mỗi rule đặt `effect` và/hoặc `marker` (≥1).
  `[[flow_rules]]` và `codegraph_config_flow` đã gộp/xoá; key cũ bị bỏ qua + warn.
- `CALL_ENTER` đọc `chain[pos+1]` làm callee → không chèn marker vào giữa `CALL_ENTER…CALL_EXIT`.
- Marker/ID là contract serialize ổn định — không đổi số cũ (`crates/codegraph-core/src/semgraph.rs`).
- `.codegraph/` không commit. `codegraph clean` xoá index nhưng giữ `config.toml`; `deinit` xoá cả thư mục.
- Thêm language: module trong `crates/codegraph-extract/src/languages/` + đăng ký `languages.rs` +
  feature `lang-<x>` + fixture/test.

## Nguồn sự thật

| Thứ | File |
|---|---|
| MCP tool + schema | `crates/codegraph-mcp/src/tools.rs` |
| Config + validation | `crates/codegraph-extract/src/config.rs` |
| Marker/ID/type lõi | `crates/codegraph-core/src/semgraph.rs` |
| Parser registry | `crates/codegraph-extract/src/languages.rs` |
| CI thật (feature/service) | `.github/workflows/ci.yml` |

## Chi tiết

- [MCP workflow](docs/agent/mcp-workflow.md)
- [Rust conventions](docs/agent/conventions.md)
- [Testing](docs/agent/testing.md)
- [Extraction & config](docs/agent/extraction.md)
- [Git & release](docs/agent/git-release.md)
- [Architecture](docs/architecture.md) · [Development](docs/development.md) · [Configuration](docs/configuration.md)
