# Testing

## Gate trước khi xong việc

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI chạy đúng 3 bước này; clippy là gate cứng (`-D warnings`).

## Chạy theo crate

```bash
cargo test -p codegraph-core
cargo test -p codegraph-extract
cargo test -p codegraph-graph
cargo test -p codegraph-api
cargo test -p codegraph-mcp
cargo test -p codegraph-sboxes
cargo test -p codegraph-bench
cargo test -p codegraph-installer
cargo test -p codegraph-extract --features lang-python   # một ngôn ngữ
```

## Vị trí test

| Loại | Vị trí |
|---|---|
| Unit test | `#[cfg(test)] mod tests` trong file nguồn |
| Integration test | `crates/<crate>/tests/*.rs` |
| Fixture source | `crates/codegraph-extract/tests/fixtures/` |
| End-to-end trace / config | `crates/codegraph-extract/tests/*_config.rs` |

## Lưu ý

- `cargo test -p codegraph` chạy **0 test** — CLI chưa có test. Đừng coi đó là đã verify.
- 4 test `storage::redis` fail khi chạy `cargo test --workspace` song song (port contention, pre-existing). Chạy standalone để xác nhận: `cargo test -p codegraph-graph --test redis`.
- Backend cần service thật + env:
  `TEST_REDIS_DSN=redis://127.0.0.1:6379` (feature `redis`);
  `TEST_RDBMS_DSN=...` + `TEST_RDBMS_REPO_ID=1` (feature `postgres`/`mysql`).
  Chạy `#[ignore]`: `cargo test -p codegraph-graph --features postgres --test rdbms -- --ignored`.
  Lưu ý: 4 test `storage::redis::tests::*` trong `crates/codegraph-graph/src/storage/redis.rs` KHÔNG `#[ignore]`,
  hardcode `redis://127.0.0.1:6379/15` → fail `Connection refused` nếu không có redis-server (pre-existing,
  không phải port contention).
- `crates/codegraph-binary/tests/extract.rs` cần `radare2` trên PATH (`--ignored`).
- `crates/codegraph-mcp/src/response_tests.rs` assert số tool đăng ký — thêm/bớt tool phải cập nhật số đó.
- Test config/flow cần `install_current(...)` và reset `install_current(None)`; classifier là thread-local.
- Test extraction dùng fixture nhỏ; thêm fixture mới thay vì nhúng source string dài.
- Logic mới có nhánh/loop/parser/config phải có ít nhất 1 test. Thay đổi chỉ rename/comment thì không cần.
- Test cần root tạm: dùng `tempfile::tempdir()` + `crate::project::init_project`.
