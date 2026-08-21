# CodeGraph

[![CI](https://github.com/hungpham10/codegraph-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/hungpham10/codegraph-rs/actions/workflows/ci.yml)
[![CodSpeed Badge](https://img.shields.io/endpoint?url=https://app.codspeed.io//badge.json)](https://app.codspeed.io//hungpham10/codegraph-rs?utm_source=badge)
[![codecov](https://codecov.io/gh/hungpham10/codegraph-rs/graph/badge.svg?token=PUSMFF0CM8)](https://codecov.io/gh/hungpham10/codegraph-rs)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

> **Local-first semantic code graph for AI agents** — tree-sitter parsing, global symbol IDs, call chains with control-flow markers, served over MCP. Single **~58 MB** static binary.

CodeGraph parses your codebase with tree-sitter, builds a **semantic graph** where every symbol gets a global ID and every function has a **call chain** (markers + callee IDs), stores everything under `.codegraph/` (SQLite by default), and exposes the graph to AI agents — Claude Code, Cursor, Codex CLI, opencode, Hermes, Antigravity — over the Model Context Protocol (MCP).

Agents that consult the semantic graph instead of grepping the filesystem make **fewer tool calls**, **explore faster**, and **stay within context**.

## Why CodeGraph?

- **Fewer tool calls** — agents navigate call chains (`codegraph_flow`), not grep
- **Local & fast** — full re-index 139 files in ~190 ms, nothing leaves your machine
- **Works everywhere** — 14 languages, 6 storage backends, 24 MCP tools, one binary
- **Semantic, not syntactic** — symbols have global IDs; edges derived from call chains with markers (`LOOP`, `IF_TRUE`, `RETURN`, …)

## ⚡ Quick Start

```bash
# 1. Initialize and index your project
cd ~/code/my-project
codegraph init

# 2. Serve to your agent over MCP (stdio)
codegraph serve --mcp

# ... or over Streamable HTTP (for remote/Docker)
codegraph serve --mcp --http --addr 0.0.0.0:8123
```

The agent binds the workspace with `codegraph_init {"path": ...}` and gets tools like `codegraph_search_symbol`, `codegraph_flow`, `codegraph_callers`, `codegraph_impact`, `codegraph_context` — all querying over MCP.

## 📊 Comparison — Why Not X?

| Tool | Type | Local-First | Semantic Graph | MCP Native | Multi-Storage | Binary Size |
|------|------|-------------|----------------|------------|---------------|-------------|
| **CodeGraph** | Code graph + MCP | ✅ | ✅ (tree-sitter semgraph) | ✅ Built-in | ✅ 6 backends | ~58 MB |
| Aider RepoMap | Repo map generator | ✅ | ❌ (ctags-based) | ❌ | ❌ | N/A |
| Sourcegraph Cody | Cloud code search | ❌ (self-host) | ✅ (CodeQL) | Via extension | ❌ | N/A |
| Bloop | Code indexer | ✅ | ❌ (search only) | ❌ | ❌ | ~30 MB |
| CodeQL | Semantic analysis | ✅/Cloud | ✅ (QL queries) | ❌ | ❌ | Heavy |
| Kythe | Code graph | ✅ | ✅ | ❌ | ❌ | Complex setup |
| LSP servers | Per-language IDE | ✅ | Per-lang only | ❌ | ❌ | Per-lang |
| ast-grep | Structural search | ✅ | ❌ (pattern match) | ❌ | ❌ | ~10 MB |
| context7 | Docs MCP | ❌ | N/A | ✅ | ❌ | N/A |

→ [Full comparison with decision matrix](docs/comparison.md)

## 🎯 Key Features

- **24 MCP tools** — `search_symbol`, `flow`, `callers`, `callees`, `impact`, `search_flow`, `context`, `references`, `diff`, `sandbox`, `mermaid`, and more
- **14 languages** — TypeScript · TSX · JavaScript · Python · Go · Rust · Java · C · C++ · C# · Ruby · PHP · Scala · Swift · Lua
- **6 storage backends** — SQLite (default), LMDB, Redis, Postgres, MySQL, Memory
- **Semantic search** — opt-in fastembed (BGE-small) for hybrid KNN + keyword search
- **Behavior sandbox** — JIT compile function groups + run against Rhai mocks
- **Full re-index always** — watcher debounces changes, re-indexes completely (simpler, no stale state)

## 📦 Install

**Automatic (recommended)**

```bash
# Linux / macOS
curl -fsSL https://raw.githubusercontent.com/hungpham10/codegraph-rs/main/scripts/install.sh | sh

# Windows (PowerShell)
irm https://raw.githubusercontent.com/hungpham10/codegraph-rs/main/scripts/install.ps1 | iex
```

**Other options**: [Homebrew](https://github.com/hungpham10/homebrew-codegraph) • [AUR](https://aur.archlinux.org/packages/codegraph-rs-bin) • [.deb/.rpm](https://github.com/hungpham10/codegraph-rs/releases/latest) • `cargo install --git https://github.com/hungpham10/codegraph-rs codegraph`

[Full install guide →](docs/specs/08-installer.md)

## 🔧 Configuration (Essentials)

```toml
# .codegraph/config.toml
[storage]
type = "sqlite"  # or lmdb, redis, postgres, mysql, memory

[embedding]
# backend = "fastembed"  # enable semantic/hybrid search
```

[Full config reference →](docs/configuration.md) | [Storage backends →](docs/storage-backends.md) | [Semantic search →](docs/semantic-search.md)

## 🏗️ Architecture

```
files → tree-sitter (rayon) → semgraph (global IDs + chains)
  → GraphIndex (2 engines + pluggable storage)
  → MCP server (24 tools) → AI Agent
```

[Architecture deep-dive →](docs/architecture.md)

## 📚 Documentation Map

| Topic | File |
|-------|------|
| Architecture & Pipeline | `docs/architecture.md` |
| Extraction & Languages | `docs/specs/04-extraction.md` |
| Storage & GraphIndex | `docs/specs/03-db-layer.md` |
| MCP Server & Tools | `docs/specs/07-mcp-server.md` |
| CLI & Watcher | `docs/specs/09-cli-watcher.md` |
| Semgraph Model | `docs/specs/02-core-types.md` |
| Installer Details | `docs/specs/08-installer.md` |
| **Full Comparison** | `docs/comparison.md` |
| Configuration Reference | `docs/configuration.md` |
| Storage Backends | `docs/storage-backends.md` |
| Semantic Search | `docs/semantic-search.md` |
| Why Rust (Rewrite Story) | `docs/why-rust.md` |
| Development Guide | `docs/development.md` |

## 🤝 Contributing

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

See [Development Guide](docs/development.md) for feature flags, per-crate tests, and release process.

## Sponsors

You can buy me a coffee by sending me money by MOMO
<p align="center">
  <a href="https://www.momo.vn/">
    <img src="assets/sponsor/MOMO.JPG" alt="MoMo Sponsor" width="200" />
  </a>
</p>

<p align="center">
  <a href='https://ko-fi.com/E1E11KPR01' target='_blank'><img height='36' style='border:0px;height:36px;' src='https://storage.ko-fi.com/cdn/kofi6.png?v=6' border='0' alt='Buy Me a Coffee at ko-fi.com' /></a>
</p>
---

## License

MIT. See [LICENSE](LICENSE).

## Acknowledgments

- Original TypeScript implementation by [@colbymchenry](https://github.com/colbymchenry)
- `tree-sitter` and all language grammar authors
- `rusqlite`, `notify`, `clap`, `tokio`, `rayon`, `ignore`, `dashmap`, `parking_lot`
