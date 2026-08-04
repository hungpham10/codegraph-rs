# CodeGraph

[![CI](https://github.com/cleboost/codegraph/actions/workflows/ci.yml/badge.svg)](https://github.com/cleboost/codegraph/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

> Local-first code intelligence for AI agents. Built in Rust. Single static
> binary, ~5 MB. Tree-sitter **semantic graph** (semgraph) in SQLite, served over MCP.

CodeGraph parses your codebase with tree-sitter, builds a **semantic graph** where every symbol gets a global ID and every function has a **call chain** (markers + callee IDs), stores everything in a single `.codegraph/db.sqlite`, and exposes the graph to AI agents — Claude Code, Cursor, Codex CLI, opencode, Hermes — over the Model Context Protocol (MCP).

Agents that consult the semantic graph instead of grepping the filesystem make **fewer tool calls**, **explore faster**, and **stay within context**.

## Highlights

- **Semgraph model**: Symbols have global IDs (≥100); call chains mix markers (`LOOP`, `IF_TRUE`, `RETURN`, …) and callee IDs. Edges derived from chains. No more `NodeKind`/`EdgeKind` — wire breaking to `SymbolKind`.
- **One binary.** Rust + statically-linked SQLite + native tree-sitter grammars. No Node runtime, no `.wasm`, no `node_modules`.
- **Small.** ~5 MB stripped (vs ~140 MB for the previous TypeScript build).
- **Fast.** Full re-index a 139-file project in ~190 ms (release, parallel rayon).
- **Local.** Index lives in `.codegraph/db.sqlite` next to your code. Nothing leaves the machine.
- **Full re-index always.** No incremental sync — watcher debounces and re-indexes completely (simpler, no stale state).
- **Multi-agent.** A single `codegraph install` configures Claude Code, Cursor, Codex, opencode, Hermes and Antigravity CLI in one go.
- **11 MCP tools** including `codegraph_flow` (call chain), `codegraph_search_flow` (pattern search), `codegraph_references` (library call consumers).

## Install

<details>
<summary><strong>Automatic (recommended)</strong></summary>

**Linux / macOS**

```sh
curl -fsSL https://raw.githubusercontent.com/Cleboost/codegraph-rs/main/scripts/install.sh | sh
```

Drops `codegraph` into `~/.local/bin`. Override with `CODEGRAPH_INSTALL_DIR`.

**Windows (PowerShell)**

```powershell
irm https://raw.githubusercontent.com/Cleboost/codegraph-rs/main/scripts/install.ps1 | iex
```

Installs to `%LOCALAPPDATA%\codegraph\bin` and adds it to the user PATH.

**Arch Linux (AUR)**

```sh
yay -S codegraph-rs-bin
```

</details>

<details>
<summary><strong>Manual</strong></summary>

1. Download the archive for your platform from the [latest release](https://github.com/Cleboost/codegraph-rs/releases/latest):

   | Platform | File |
   |---|---|
   | Linux x86_64 | `codegraph-x86_64-unknown-linux-musl.tar.gz` |
   | Linux aarch64 | `codegraph-aarch64-unknown-linux-gnu.tar.gz` |
   | macOS x86_64 | `codegraph-x86_64-apple-darwin.tar.gz` |
   | macOS arm64 | `codegraph-aarch64-apple-darwin.tar.gz` |
   | Windows x86_64 | `codegraph-x86_64-pc-windows-msvc.zip` |

2. Extract and place the `codegraph` binary somewhere on your `PATH`.

</details>

<details>
<summary><strong>From source</strong></summary>

Requires Rust stable (≥ 1.80).

```sh
git clone https://github.com/Cleboost/codegraph-rs
cd codegraph-rs
cargo build --release -p codegraph
# binary at target/release/codegraph
```

Or via Cargo directly:

```sh
cargo install --git https://github.com/Cleboost/codegraph-rs codegraph
```

</details>

## Quick start

```sh
# 1. Init, index, and configure your agents in one step
cd ~/code/my-project
codegraph init

# 2. Use it
codegraph query UserService
codegraph context "auth middleware"
```

Your agent now has tools like `codegraph_search`, `codegraph_symbol`, `codegraph_callers`, `codegraph_flow`, `codegraph_search_flow`, `codegraph_impact`, `codegraph_context` available over MCP. The file watcher debounces changes and triggers full re-indexes while you edit.

## CLI reference

| Command | What it does |
|---|---|
| `codegraph init [--no-index]` | Create `.codegraph/`, full re-index, and configure agents; `--no-index` skips indexing |
| `codegraph uninit` | Remove `.codegraph/` |
| `codegraph index` | **Full re-index** of the workspace (reset → parse all → ingest) |
| `codegraph status` | Show counts (symbols, chains, edges, files), no schema version |
| `codegraph query <q>` | Substring search across symbol names (case-insensitive) |
| `codegraph files [path]` | List indexed files under a prefix |
| `codegraph context <target>` | Build markdown context (symbol + callers + callees + optional source) |
| `codegraph serve --mcp` | Run as MCP server over stdio (used by agents) |
| `codegraph visualize` | Local web UI (2D/3D graph + table) at `http://127.0.0.1:7421` |

Global flag `--path <dir>` overrides the workspace root.

`visualize` is enabled by default. For a slimmer binary without the embedded web UI: `cargo build -p codegraph --no-default-features`.

## Supported languages

14 languages with full tree-sitter extraction + marker/chain walkers:

**TypeScript · TSX · JavaScript · Python · Go · Rust · Java · C · C++ · C# · Ruby · PHP · Scala · Swift · Lua**

Each language emits:
- **Symbols**: Functions, methods, classes, interfaces, enums, variables, constants, parameters, fields, modules, files, configs
- **Chains**: `[func_id, MARKER, callee_id, MARKER, ...]` — markers: `LOOP=1`, `IF_TRUE=3`, `IF_FALSE=4`, `BRANCH_END=5`, `RETURN=6`, `LOOP_BACK=7`, `SWITCH_CASE=8`, `SWITCH_END=9`, `BREAK=10`, `CONTINUE=11`, `THROW=12`
- **Calls**: Resolved from placeholder `0` in chain → exact name → short name → best candidate (override +5, has-chain +5, same-file +3)
- **Effects**: Auto-classified from callee name (`requests.*` → `HttpCall`, `.Model(` → `SqlQuery`, `.Create(` → `SqlWrite`, `log/print` → `Log`, etc.)

## MCP tools

Agents see **11 tools** through the MCP server:

| Tool | Use case |
|---|---|
| `codegraph_search` | Find symbols by name (substring, case-insensitive) |
| `codegraph_symbol` | Look up a symbol by id or exact name; duplicate names → `ambiguous=true` with full match list; retry with `id` |
| `codegraph_callers` | What (transitively) calls this function? (BFS on chain engine) |
| `codegraph_callees` | What does this function call directly? (read chain, skip markers) |
| `codegraph_impact` | Transitive impact radius = callers up to `max_depth` |
| `codegraph_flow` | Full call chain: markers + callee names + call sites (line/condition/effect/args) |
| `codegraph_search_flow` | Find functions whose chain contains a pattern (comma-separated: marker names, symbol names, or numeric IDs) |
| `codegraph_context` | Composed context for a symbol or topic (search + callers + callees + optional source) |
| `codegraph_references` | Functions that call a library call matching `query` (includes unresolved external calls) |
| `codegraph_files` | List indexed files under a path prefix |
| `codegraph_status` | Index health: symbol/chain/edge/file counts |

Read the [server instructions](crates/codegraph-mcp/src/server-instructions.md) that ship with the binary — they tell your agent when to reach for which tool.

### `codegraph_search_flow` pattern examples

```json
{ "pattern": "LOOP, validate, save, LOOP_BACK" }       // Python for-loop calling validate then save
{ "pattern": "IF_TRUE, UserService, save" }            // If-branch calling UserService.save
{ "pattern": "121, 122" }                              // Chain containing symbol ID 121 then 122
{ "pattern": "RETURN, helper" }                        // Function returning via helper call
```

Tokens can be: marker names (`LOOP`, `IF_TRUE`, `IF_FALSE`, `BRANCH_END`, `RETURN`, `LOOP_BACK`, `SWITCH_CASE`, `SWITCH_END`, `BREAK`, `CONTINUE`, `THROW`), symbol names (resolved exact, ambiguous picks first), or numeric symbol IDs.

### Disambiguation

When `codegraph_symbol` or `codegraph_search` returns duplicate names:
```json
{
  "ambiguous": true,
  "matches": [ { "id": 121, "name": "process_user", "file": "a.py" }, { "id": 126, "name": "process_user", "file": "b.rs" } ]
}
```
→ LLM retries with `codegraph_symbol` + specific `id`.

## Architecture

```
crates/
  codegraph-core/       Error + semgraph model (Symbol, SymbolKind, Chain, CallRecord, EffectType, ScopeLevel, markers)
  codegraph-extract/    tree-sitter native + 14 LangSpec declarative extractors + 5 hand-written
  codegraph-graph/      GraphIndex (semgraph): registry + 2 engines (chain Search<u64> + name Search<u8>) + sqlite storage
  codegraph-context/    Markdown/JSON context formatter (symbol + callers + callees + source)
  codegraph-api/        GraphApi wrapper on SharedGraphIndex (async query surface)
  codegraph-mcp/        Hand-rolled JSON-RPC 2.0 server (stdio) + 11 tool dispatch
  codegraph-installer/  Agent config targets (Claude/Cursor/Codex/opencode/Hermes)
  codegraph/            CLI (clap) + watcher (notify + debounced full re-index)
```

Pipeline:
```
files → ignore::WalkBuilder → rayon parse pool (tree-sitter, 14 langs)
           ↓
      ParseResult (symbols local-id, chains, CallRecords)
           ↓
      GraphIndex.ingest()  — full re-index:
           1. Reset (clear entities, engines)
           2. Register symbols → global IDs + remap scope/type_ref
           3. Remap chains (local→global), keep placeholder 0
           4. Resolve calls: structural hint → exact name → short name → best-candidate
           5. Build edges + call records + call-name index
           6. Persist entities + rebuild engines + bump version
           ↓
      GraphApi / SharedGraphIndex.ensure_fresh() (version probe)
           ↓
      MCP server  /  CLI commands  /  Web UI
```

## Configuration

A `.codegraph/` directory is created next to your project:

```
.codegraph/
  db.sqlite        SQLite v1 (WAL mode, single file — entities + radix streams)
  config.toml      Language enable/disable, walker include/exclude
  .gitignore       Pre-filled so the index is never committed
  version          Codegraph version that created the directory
```

### config.toml example

```toml
# Language toggles (all 14 enabled by default)
[languages]
rust = true
go = true
python = true
typescript = true
javascript = true
java = true
c = true
cpp = true
csharp = true
ruby = true
php = true
scala = true
swift = true
lua = true

# Walker filters (same syntax as .gitignore)
[walker]
include = ["**/*"]
exclude = [
  ".git/**",
  ".codegraph/**",
  "target/**",
  "node_modules/**",
  "*.min.js",
  "*.lock"
]
```

### C vs C++ headers (`.h`)

By default, `.h` files are resolved automatically:
- **C++ project** (`.cpp`/`.hpp` present, no `.c`) → parsed as C++
- **C project** (`.c` present, no C++ sources) → parsed as C
- **Mixed C/C++** → each `.h` inspected for C++ syntax (`namespace`, `class`, `template`, …)

Override in `.codegraph/config.toml`:
```toml
[languages]
headers = "auto"   # "auto" (default), "c", or "cpp"
```

After changing this setting, run `codegraph index` to re-index headers.

## Why Rust?

This project is a from-scratch Rust rewrite of the previous TypeScript implementation. The old binary embedded a Node.js runtime, 20+ tree-sitter WASM grammars, and a native SQLite addon — about **140 MB on disk**, with a multi-second cold start.

The Rust port:
- Drops the Node runtime → static binary
- Replaces WASM grammars with statically-linked tree-sitter C libraries
- Bundles SQLite as a static C library (no system dependency)
- Parses in parallel via `rayon`
- Builds with `lto="fat"`, `codegen-units=1`, `strip`, `panic=abort`

Result: **~5 MB** stripped, **sub-second** startup, **~5× faster** indexing on the same workspace.

## Semgraph model (wire-breaking)

The semantic graph model replaces the old `Node`/`Edge`/`NodeKind`/`EdgeKind`:

| Old | New (semgraph) |
|-----|----------------|
| `NodeKind` (22 values) | `SymbolKind` { Function, Method, Class, Interface, Enum, Variable, Constant, Parameter, Field, Module, File, Config } |
| `EdgeKind` (12 values) | Derived from chain: every symbol element = callee; `EdgeMeta` { position, condition, effect, is_loop_body, is_recursive } |
| `NodeId = i64` (rowid) | `SymbolId = u64` (global registry, monotonic, starts at 100) |
| FTS5 search | Radix `Search<u8>` on lowercase names (in-memory, rebuilt on open/ingest) |
| `callers` BFS on edges | Substring search on chain engine `Search<u64>` (KMP via shortcuts) |
| Incremental sync | **Full re-index** (watcher debounces → `ingest` resets everything) |

See `crates/codegraph-core/src/semgraph.rs` for the full model.

## Development

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

Per-crate test runs:

```sh
cargo test -p codegraph-core
cargo test -p codegraph-extract    # 30 tests: 10 lib + 16 chains + 2 cpp + 2 extract
cargo test -p codegraph-graph      # 60+ tests: search, storage, ingest, flow, reopen
cargo test -p codegraph-api
cargo test -p codegraph-mcp
cargo test -p codegraph-viz
cargo test -p codegraph-installer
```

Feature flags on `codegraph-extract`:
- Default: `all-langs` (enables all 14)
- Individual: `lang-rust`, `lang-go`, `lang-python`, `lang-typescript`, `lang-javascript`, `lang-java`, `lang-c`, `lang-cpp`, `lang-csharp`, `lang-ruby`, `lang-php`, `lang-scala`, `lang-swift`, `lang-lua`

```sh
# Test single language
cargo test -p codegraph-extract --features lang-python
```

Feature flags on `codegraph-graph`:
- `sqlite` — sqlite storage backend (enabled on `codegraph`, `codegraph-mcp`, `codegraph-viz`)
- `redis` — redis storage backend (compile-only verify, runtime needs server)

```sh
# Full feature verification
cargo check --workspace --features sqlite
cargo check -p codegraph-graph --features redis
```

## License

MIT. See [LICENSE](LICENSE).

## Acknowledgments

- The original TypeScript implementation by [@colbymchenry](https://github.com/colbymchenry).
- `tree-sitter` and all language grammar authors.
- `rusqlite`, `notify`, `clap`, `tokio`, `rayon`, `ignore`, `dashmap`, `parking_lot`.