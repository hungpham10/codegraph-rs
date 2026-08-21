# Why Rust? — The Rewrite Story

CodeGraph is a from-scratch Rust rewrite of the previous TypeScript implementation.

## The Old Stack (TypeScript)

| Component | Technology | Pain Points |
|-----------|------------|-------------|
| Runtime | Node.js (embedded) | ~50 MB baseline, multi-second cold start |
| Parsing | 20+ tree-sitter WASM grammars | WASM overhead, no parallel parsing |
| Storage | Native SQLite addon (better-sqlite3) | Node-gyp builds, platform issues |
| Distribution | Single binary via `pkg` | ~140 MB, not truly static |

**Result**: ~140 MB binary, 2–3 second startup, complex build pipeline.

---

## The Rust Rewrite

### What Changed

| Before (TS) | After (Rust) | Impact |
|-------------|--------------|--------|
| Node runtime | **None** — static binary | -80 MB, sub-ms startup |
| WASM grammars | **Statically-linked tree-sitter C** | Native speed, rayon parallelism |
| Native SQLite addon | **Bundled `rusqlite` (bundled feature)** | No system deps, no node-gyp |
| `pkg` bundler | **`cargo build --release` + `strip`** | Standard Rust toolchain |

### Build Optimizations

```toml
# Cargo.toml (workspace)
[profile.release]
lto = "fat"           # Cross-crate optimization
codegen-units = 1     # Maximum optimization
strip = true          # Strip symbols
panic = "abort"       # Smaller binary, no unwinding
```

### Results

| Metric | TypeScript | Rust | Improvement |
|--------|------------|------|-------------|
| Binary size | ~140 MB | **~58 MB** | **2.4× smaller** |
| Cold start | ~2–3 s | **<100 ms** | **20–30× faster** |
| Indexing (139 files) | ~1 s | **~190 ms** | **~5× faster** |
| Memory (idle) | ~80 MB | **~15 MB** | **5× less** |
| Dependencies | 500+ npm packages | **~100 crates** | Simpler supply chain |

---

## Why These Choices?

### `tree-sitter` (C) over WASM

- **Parallel parsing**: `rayon` thread pool across files — WASM can't do true parallelism
- **Zero-copy**: Parse trees reference source bytes directly
- **No WASM overhead**: Function calls, memory copies eliminated
- **Grammar updates**: `tree-sitter` C libs updated independently

### `rusqlite` (bundled SQLite) over native addon

- **Pure Rust + bundled C**: `rusqlite` with `bundled` feature compiles SQLite from source
- **No system SQLite needed**: Works on minimal containers (distroless, scratch)
- **WAL mode**: Concurrent readers during write
- **No node-gyp**: Eliminates entire class of build failures

### Single Binary Philosophy

```
codegraph binary contains:
  ├── tree-sitter parsers (14 languages, statically linked)
  ├── SQLite (bundled, WAL mode)
  ├── LMDB (bundled via lmdb-rkv)
  ├── Redis client (async, tokio)
  ├── Postgres/MySQL drivers (sqlx, compiled in)
  ├── ONNX Runtime + fastembed (BGE-small model loader)
  └── MCP server (rmcp SDK)
```

**No**:
- External processes
- Shared libraries (except libc)
- Runtime downloads (model cached separately)
- Daemon/background service

---

## Trade-offs

| Gain | Cost |
|------|------|
| Fast startup | Longer compile time (~3–5 min clean) |
| Small binary | Larger binary than minimal CLI (~58 MB) |
| Parallel parsing | More complex build (C dependencies) |
| No runtime deps | Can't hot-reload grammars (rebuild needed) |
| Type safety | Learning curve for contributors |

---

## Verification

```bash
# Build release
cargo build --release -p codegraph

# Check size
ls -lh target/release/codegraph
# ~58 MB

# Verify static linking
ldd target/release/codegraph
# Should show only libc, libdl, libpthread, libm, libgcc_s

# Benchmark startup
time target/release/codegraph --version
# <100 ms

# Benchmark indexing
cd /path/to/project
time target/release/codegraph init
# ~190 ms for ~139 files
```

---

## Related Docs

- [Architecture](architecture.md) — Crate structure and pipeline
- [Development](development.md) — Build, test, feature flags
- [Configuration](configuration.md) — Storage/embedding backends
- [README](../README.md) — Quick start