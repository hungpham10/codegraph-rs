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
- **Multi-agent.** One binary serves any MCP client (Claude Code, Cursor, Codex, opencode, Hermes, Antigravity) over stdio or Streamable HTTP (`--http`) — the agent binds the workspace with `codegraph_init` and drives everything through tools.
- **30 MCP tools** including `codegraph_flow` (call chain), `codegraph_search_flow` (pattern search), `codegraph_references` (library call consumers), `codegraph_diff` (MR impact draft), and a behavior sandbox (`codegraph_sandbox`).

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
# 1. Init and index your project
cd ~/code/my-project
codegraph init

# 2. Serve it to your agent (Claude Code, Cursor, ...) over MCP (stdio)
codegraph serve --mcp

# ... or over Streamable HTTP (SSE), e.g. for a remote client / Docker container
codegraph serve --mcp --http --addr 0.0.0.0:8123
# point the client at: http://<host>:8123/mcp  →  {"type": "http", "url": "http://<host>:8123/mcp"}
```

The agent then binds the workspace with `codegraph_init {"path": ...}` and gets
tools like `codegraph_search`, `codegraph_symbol`, `codegraph_callers`,
`codegraph_flow`, `codegraph_search_flow`, `codegraph_impact`,
`codegraph_context` — all querying is done **over MCP**, not via CLI commands.
The file watcher debounces changes and triggers full re-indexes while you edit.

Over HTTP each connection (`mcp-session-id`) gets its own fresh server session
— the agent binds the workspace root with `codegraph_init` inside that
connection; nothing is shared between connections but the process. rmcp's
`allowed_hosts` check blocks foreign `Host` headers (DNS-rebinding protection):
loopback hosts pass by default; for LAN access pass `--allow-host <host-or-ip>`
(repeatable) or `--allow-any-host` on a trusted network.

## CLI reference

The CLI is deliberately minimal — it only manages the workspace lifecycle and
runs the MCP server. All reading/interacting goes through MCP tools.

| Command | What it does |
|---|---|
| `codegraph init [--no-index]` | Create `.codegraph/` and full re-index (skip with `--no-index`) |
| `codegraph deinit` | Remove `.codegraph/` |
| `codegraph serve --mcp` | Run as MCP server over stdio (used by agents) |
| `codegraph serve --mcp --http` | Run as MCP server over Streamable HTTP (SSE); `--addr` (default `0.0.0.0:8123`), `--allow-host <host>` (repeatable, LAN), `--allow-any-host` |

Global flag `--path <dir>` overrides the workspace root.

## Supported languages

14 languages with full tree-sitter extraction + marker/chain walkers:

**TypeScript · TSX · JavaScript · Python · Go · Rust · Java · C · C++ · C# · Ruby · PHP · Scala · Swift · Lua**

Each language emits:
- **Symbols**: Functions, methods, classes, interfaces, enums, variables, constants, parameters, fields, modules, files, configs
- **Chains**: `[func_id, MARKER, callee_id, MARKER, ...]` — markers: `LOOP=1`, `IF_TRUE=3`, `IF_FALSE=4`, `BRANCH_END=5`, `RETURN=6`, `LOOP_BACK=7`, `SWITCH_CASE=8`, `SWITCH_END=9`, `BREAK=10`, `CONTINUE=11`, `THROW=12`
- **Calls**: Resolved from placeholder `0` in chain → exact name → short name → best candidate (override +5, has-chain +5, same-file +3)
- **Effects**: Auto-classified from callee name (`requests.*` → `HttpCall`, `.Model(` → `SqlQuery`, `.Create(` → `SqlWrite`, `log/print` → `Log`, etc.)

## MCP tools

Agents see **30 tools** through the MCP server (search, callers/callees/impact/
flow, class queries, annotations, dependencies, diff draft/simulation, behavior
sandbox, usage report, plus the session tools `codegraph_init` /
`codegraph_deinit` / `codegraph_index`). Key ones:

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
| `codegraph_init` | Bind the session to a workspace root (non-blocking, does **not** index by default) |
| `codegraph_index` | Full re-index of the bound workspace |
| `codegraph_sandbox` | Compile a function group to machine code and run it against Rhai mocks |
| `codegraph_diff` | Draft report of what an MR/patch would change in the graph |

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
  codegraph-mcp/        MCP server on the rmcp SDK (stdio + Streamable HTTP) + 30-tool dispatch, session-driven
  codegraph-installer/  Agent config targets (Claude/Cursor/Codex/opencode/Hermes)
  codegraph/            CLI lifecycle (init/deinit/serve --mcp) + watcher (notify + debounced full re-index)
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
      MCP server  /  CLI lifecycle
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

### Postgres / MySQL (multi-tenant, sharded)

CodeGraph can store the index in PostgreSQL or MySQL instead of the local
SQLite file. Every table is partitioned by a leading `repo_id` (a `u64`
partition key), so each project root (`.codegraph/`) maps to its own
partition — re-indexing or deleting one repo never touches another. Sharding
is `repo_id % N` across the configured DSN list.

Build with the `rdbms` feature (it is **on by default** for the `codegraph`
binary):

```bash
cargo build --features rdbms          # default for `codegraph`
cargo build -p codegraph-mcp --features rdbms
```

`.codegraph/config.toml`:

```toml
[storage]
type = "postgres"
# type = "mysql"
# Shard DSNs — shard = repo_id % len(dsns). One entry = single shard.
dsns = [
  "postgres://user:pass@db1:5432/codegraph",
  "postgres://user:pass@db2:5432/codegraph",
]
# repo_id is generated automatically by `codegraph init` (self-heal) and
# written here. Do not edit it by hand.
# repo_id = 14028493579208694412
```

**Schema is applied manually** — the binary does not run migrations. Run the
SQL files from `sql/<engine>/` in order (currently `001-initial-schema.sql`
and `002-add-repos-registry.sql`) against every shard server before indexing:

```bash
psql "$DSN" -f sql/postgres/001-initial-schema.sql
psql "$DSN" -f sql/postgres/002-add-repos-registry.sql
# mysql:
#   mysql "$DB" < sql/mysql/001-initial-schema.sql
#   mysql "$DB" < sql/mysql/002-add-repos-registry.sql
```

Then `codegraph init` (CLI) or `codegraph_init` (MCP tool) generates the
`repo_id` and stores the index on the right shard automatically. See
`sql/README.md` for the full multi-tenant + sharding design.

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

After changing this setting, run `codegraph init` (or call `codegraph_index` over MCP) to re-index headers.

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
- `postgres` — PostgreSQL storage backend (multi-tenant, sharded)
- `mysql` — MySQL storage backend (multi-tenant, sharded)

The `codegraph` and `codegraph-mcp` binaries expose a convenience `rdbms`
feature that turns on both `postgres` and `mysql` (it is **on by default**
for `codegraph`):

```sh
# Full feature verification
cargo check --workspace --features sqlite
cargo check -p codegraph-graph --features redis
cargo check -p codegraph --features rdbms
```

## License

MIT. See [LICENSE](LICENSE).

## Acknowledgments

- The original TypeScript implementation by [@colbymchenry](https://github.com/colbymchenry).
- `tree-sitter` and all language grammar authors.
- `rusqlite`, `notify`, `clap`, `tokio`, `rayon`, `ignore`, `dashmap`, `parking_lot`.