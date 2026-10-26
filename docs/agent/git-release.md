# Git & release

## Commit

Conventional commits: `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`.
Subject tiếng Anh hoặc Việt đều được — nhất quán trong một PR.

## Trước khi mở PR

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace   # toàn bộ, không dùng --lib để kết luận
```

- Regression đúng vùng vừa sửa:
  - extract/config/marker → `cargo test -p codegraph-extract --test effects_config --test chains --test dynamic_dispatch`
  - trace/sandbox → `cargo test -p codegraph-graph --test trace_flow_tests` + `cargo test -p codegraph-sboxes`
  - backend → `cargo test -p codegraph-graph --features sqlite,lmdb`
  - 4 test `storage::redis` fail khi chạy song song toàn workspace (pre-existing, không phải port contention:
    chúng hardcode `redis://127.0.0.1:6379/15` và KHÔNG `#[ignore]`; cần redis-server). Chạy lại standalone để xác nhận.
- Thay đổi hành vi public (CLI, MCP tool, config schema, marker/ID) ⇒ cập nhật `docs/` trong cùng PR.
- Thêm/bớt MCP tool ⇒ cập nhật registry `crates/codegraph-mcp/src/tools.rs` **và** assert số tool trong `response_tests.rs`.
- Thêm config key ⇒ cập nhật `DEFAULT_CONFIG_TOML` + `docs/configuration.md`.
- Không force-push lên nhánh đang có người review; không commit `.codegraph/`.

## Mở PR

1. Tạo nhánh → commit (conventional commits: `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`; tiếng Anh hoặc Việt đều được).
2. Mở PR. Chờ GitHub Actions xanh (test matrix + clippy + web build).
3. Chờ review benchmark (Criterion/CodSpeed) và review Codecov — coverage không được giảm.
4. Chỉ merge sau khi 6–7 xanh.

## Release

CI xử lý khi push tag `vX.Y.Z` (`.github/workflows/release.yml`):

1. Build 5 target: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`.
2. Ký cosign (keyless, GitHub OIDC), đính `.sig` + `.crt`.
3. Publish Homebrew tap, AUR, `.deb`/`.rpm`.

Build local:

```bash
cargo build --release -p codegraph
# target/release/codegraph
```

Version nằm trong `[workspace.package]` của `Cargo.toml` — bump ở đó, không sửa từng crate.

## Debug

```bash
RUST_LOG=codegraph=debug codegraph init
RUST_LOG=codegraph_graph=trace codegraph init
RUST_LOG=codegraph_mcp=debug codegraph serve --mcp

CODEGRAPH_PROFILE=stderr codegraph serve --mcp
CODEGRAPH_PROFILE_FILE=/tmp/cgprof.log codegraph serve --mcp
```

`serve --mcp` dùng stdout cho JSON-RPC, nên profiling phải ghi ra file khi chạy dưới MCP host.
