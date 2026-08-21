# Development Guide

Building, testing, and contributing to CodeGraph.

## Prerequisites

- **Rust stable** ≥ 1.85 (edition 2024 used in `codegraph-graph`)
- `cargo` (from rustup)
- Optional: `clang` for some tree-sitter grammars (usually bundled)

```bash
# Verify toolchain
rustc --version
cargo --version
```

---

## Quick Commands

```bash
# Build everything
cargo build --workspace

# Build release binary (what users get)
cargo build --release -p codegraph

# Run all tests
cargo test --workspace

# Lint (CI gate)
cargo clippy --workspace --all-targets -- -D warnings

# Format
cargo fmt --all

# Check all feature combinations
cargo check --workspace --features sqlite
cargo check -p codegraph-graph --features redis
cargo check -p codegraph --features rdbms
cargo check -p codegraph --features fastembed
```

---

## Crate Overview

```
crates/
  codegraph-core/       Error types + semgraph model (Symbol, Chain, CallRecord, markers)
  codegraph-extract/    tree-sitter native + 14 LangSpec extractors + 5 hand-written
  codegraph-graph/      GraphIndex: registry + 2 engines + pluggable storage + embeddings
  codegraph-context/    Markdown/JSON context formatter
  codegraph-api/        GraphApi wrapper on SharedGraphIndex (async queries)
  codegraph-sboxes/     Behavior sandbox: Cranelift JIT + Rhai mock runtime
  codegraph-mcp/        MCP server (rmcp SDK) + 24 tools + session management
  codegraph-bench/      Benchmarks (criterion, codspeed, storage comparison)
  codegraph/            CLI (init/deinit/embed/serve) + watcher (notify + debounce)
```

---

## Per-Crate Test Commands

```bash
# Core model tests
cargo test -p codegraph-core

# Extraction: 30 tests (10 lib + 16 chains + 2 cpp + 2 extract)
cargo test -p codegraph-extract

# Graph: 60+ tests (search, storage, ingest, flow, reopen)
cargo test -p codegraph-graph

# API layer
cargo test -p codegraph-api

# MCP server + tools
cargo test -p codegraph-mcp

# Sandbox JIT: control flow + end-to-end traces
cargo test -p codegraph-sboxes

# Bench pipeline integration
cargo test -p codegraph-bench

# Installer
cargo test -p codegraph-installer
```

---

## Feature Flags

### `codegraph-extract` (language support)

| Feature | Languages |
|---------|-----------|
| `all-langs` (default) | All 14 |
| `lang-rust` | Rust |
| `lang-go` | Go |
| `lang-python` | Python |
| `lang-typescript` | TypeScript |
| `lang-javascript` | JavaScript |
| `lang-java` | Java |
| `lang-c` | C |
| `lang-cpp` | C++ |
| `lang-csharp` | C# |
| `lang-ruby` | Ruby |
| `lang-php` | PHP |
| `lang-scala` | Scala |
| `lang-swift` | Swift |
| `lang-lua` | Lua |

```bash
# Test single language
cargo test -p codegraph-extract --features lang-python
```

### `codegraph-graph` (storage + features)

| Feature | Description | Default on `codegraph` |
|---------|-------------|------------------------|
| `sqlite` | SQLite storage | ✅ |
| `lmdb` | LMDB storage | ✅ |
| `redis` | Redis storage (compile verify) | ❌ |
| `postgres` | PostgreSQL storage | via `rdbms` |
| `mysql` | MySQL storage | via `rdbms` |
| `bloom-search` | Bloom filter for chain search | ✅ |
| `fastembed` | ONNX embedding backend | ✅ (via codegraph-api) |
| `apple-accel` | macOS CoreML for ONNX | ❌ (macOS only) |

### `codegraph` binary

| Feature | Description | Default |
|---------|-------------|---------|
| `rdbms` | Enable `postgres` + `mysql` | ✅ |
| `fastembed` | Compile `codegraph embed` CLI | ❌ |
| `apple-accel` | macOS CoreML | ❌ |

### `codegraph-mcp` crate

| Feature | Description | Default |
|---------|-------------|---------|
| `rdbms` | Enable `postgres` + `mysql` | ❌ |

---

## Important Notes

### `codegraph-api` enables all `codegraph-graph` features

```bash
# This does NOT produce a slimmer binary — all storage drivers
# and embedding backend are still compiled in via codegraph-api
cargo build -p codegraph --no-default-features
```

To actually reduce binary size, you must build with minimal features on `codegraph-graph` AND avoid depending on `codegraph-api` (not practical for the main binary).

### `apple-accel` is macOS-only

```bash
# Works
cargo build --features fastembed,apple-accel --target x86_64-apple-darwin
cargo build --features fastembed,apple-accel --target aarch64-apple-darwin

# Fails
cargo build --features fastembed,apple-accel --target x86_64-unknown-linux-gnu
```

---

## Running Benchmarks

```bash
# Criterion benchmarks (statistical)
cargo bench -p codegraph-bench

# Single-pass measurement (JSON output)
cargo run -p codegraph-bench -- --json

# Storage backend comparison
cargo run -p codegraph-bench -- --storage sqlite,lmdb,memory

# CodSpeed (CI only — see .github/workflows/codspeed.yml)
cargo codspeed build -p codegraph-bench --features codspeed
```

---

## Release Process

Handled by CI (`.github/workflows/release.yml`):

1. Tag pushed: `vX.Y.Z`
2. Builds for all targets:
   - `x86_64-unknown-linux-musl`
   - `aarch64-unknown-linux-gnu`
   - `x86_64-apple-darwin`
   - `aarch64-apple-darwin`
   - `x86_64-pc-windows-msvc`
3. Signs with cosign (keyless, GitHub OIDC)
4. Attaches `.sig` + `.crt` to release
5. Publishes to Homebrew tap, AUR, .deb/.rpm

**Local release build**:
```bash
cargo build --release -p codegraph
# Binary at target/release/codegraph
```

---

## Project Structure

```
.
├── crates/                 # Workspace members
├── docs/                   # Documentation (this file + others)
│   ├── architecture.md
│   ├── comparison.md
│   ├── configuration.md
│   ├── development.md      # This file
│   ├── semantic-search.md
│   ├── storage-backends.md
│   ├── why-rust.md
│   └── specs/              # Detailed spec docs
├── scripts/                # Install scripts (sh/ps1)
├── sql/                    # Postgres/MySQL schemas
├── packaging/              # .deb/.rpm packaging
├── .github/workflows/      # CI/CD
├── Cargo.toml              # Workspace root
└── README.md               # Main entry point
```

---

## Contributing

1. Fork & branch
2. `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace`
4. Add tests for new functionality
5. Update relevant docs in `docs/`
6. PR with clear description

**Commit style**: Conventional commits (`feat:`, `fix:`, `docs:`, `refactor:`, `test:`)

---

## Debugging Tips

```bash
# Verbose logging
RUST_LOG=codegraph=debug codegraph init

# Specific crate
RUST_LOG=codegraph_graph=trace codegraph init

# MCP server debug
RUST_LOG=codegraph_mcp=debug codegraph serve --mcp

# Watcher debug
RUST_LOG=codegraph=debug codegraph serve --mcp
```

---

## Related Docs

- [Architecture](architecture.md) — Pipeline and crate relationships
- [Why Rust](why-rust.md) — Rewrite rationale and benchmarks
- [Configuration](configuration.md) — Config reference
- [Storage Backends](storage-backends.md) — Backend deep-dive
- [Semantic Search](semantic-search.md) — Embedding setup
- [README](../README.md) — Quick start