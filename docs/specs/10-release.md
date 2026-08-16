# Spec 10 — Release, packaging & CI

**Status**: ✅ done (evolved) — CI in `.github/workflows/`, packaging in
`scripts/` + `packaging/aur`.

## CI (`ci.yml`)

Triggers: push to `main`, PRs, manual. `RUSTFLAGS: -D warnings`.

- **clippy** — `cargo clippy --workspace --all-targets -- -D warnings` +
  a feature matrix run (`codegraph-graph` with postgres/mysql/redis).
- **test** — `cargo test --workspace --no-fail-fast` under coverage
  instrumentation (`grcov` → lcov → Codecov), plus ignored storage
  integration tests against real services (redis, postgres, mysql containers
  with schemas from `sql/`).
- **slim** — verifies `cargo build -p codegraph --no-default-features`
  compiles without the `rdbms` wiring.

The workspace's newer crates (`codesmell`) are covered automatically as
workspace members.

## Distribution

- GitHub Releases with per-platform archives (Linux x86_64 musl + aarch64,
  macOS x86_64 + arm64, Windows x86_64) — see the README install matrix.
- `scripts/install.sh` (curl | sh → `~/.local/bin`, override with
  `CODEGRAPH_INSTALL_DIR`) and `scripts/install.ps1` (Windows).
- Arch Linux AUR package (`packaging/aur`, `codegraph-rs-bin`).
- From source: `cargo build --release -p codegraph` or
  `cargo install --git … codegraph`.

## Binary size & features

Reality vs the original "<15 MB" target: ~**58 MB** stripped with **every**
backend bundled (SQLite, LMDB, Redis, Postgres/MySQL drivers, ONNX embedding
runtime) — accepted in exchange for a single zero-setup binary. The
`release-small` profile (`opt-level="z"`) remains for constrained targets.

Feature flags (see README "Development" for the full list): `rdbms` (default
on the binary), `fastembed` (embed CLI + backend), `apple-accel` (macOS
CoreML), per-language `lang-*` on `codegraph-extract`.

## crates.io (when publishing)

Manual `cargo publish` in dependency order — the original order referenced
crates that no longer exist; the current one is:

1. `codegraph-core`
2. `codegraph-extract` · `codegraph-graph` (extract depends on graph)
3. `codegraph-context` · `codegraph-sboxes` · `codegraph-api`
4. `codegraph-mcp` · `codegraph-installer`
5. `codesmell` (depends on core/extract/graph)
6. `codegraph` (binary — users install this)

## Deviations from the original spec

- musl is built via cross in CI; UPX was dropped (breaks macOS signing).
- No CHANGELOG auto-extraction job; release notes are written manually.
