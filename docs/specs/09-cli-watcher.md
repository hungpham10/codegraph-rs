# Spec 09 — CLI + file watcher

**Status**: ✅ done — implemented in `crates/codegraph`
(`src/main.rs`, `src/watcher.rs`).

## Goal

A deliberately minimal binary: workspace lifecycle + serving. All reading
and querying goes through MCP tools — the CLI intentionally has no
query/context/status subcommands.

## Subcommands

| Command | What it does |
|---|---|
| `codegraph init [--no-index]` | Create `.codegraph/` (idempotent; preserves an existing `config.toml`), self-heal `repo_id` for RDBMS backends, full re-index unless `--no-index`. Live progress bar by default (`--no-progress` to disable) |
| `codegraph deinit` | Remove `.codegraph/` |
| `codegraph embed [--model <m>] [--cache-dir <dir>]` | Pre-download the embedding model into the global cache (needs `fastembed` feature; default `bge-small-en-v1.5`) |
| `codegraph serve --mcp [--http …]` | Run the MCP server — stdio, or Streamable HTTP with `--addr` / `--allow-host` / `--allow-any-host` / `--api-key` / `--enable-observability` / `--format` (spec 07, [docs/mcp.md](../mcp.md)) |
| `codegraph install` | Multi-agent client installer (spec 08) |

Global `--path <dir>` overrides the workspace root (default cwd).

## File watcher

- Spawned by `serve` when the startup root is initialized **and** the
  backend has a local DSN (sqlite/lmdb/redis). `memory` and Postgres/MySQL
  get no watcher.
- `notify` + `notify-debouncer-full`, 500 ms debounce, recursive on the root.
- Ignores events under `.codegraph/`, `.git/`, and paths matched by the
  repo's `.gitignore` (note: it does not consult `.codegraphignore`).
- Any relevant event triggers a **full re-index** (`GraphIndex::open(dsn)` →
  `ingest`) — there is no incremental mode by design; simplicity over stale
  state. `SharedGraphIndex::ensure_fresh()` picks the new version up on the
  next query.

## Deviations from the original spec

- `sync`, `status`, `query`, `files`, `context`, `affected`, `watch`
  subcommands were cut — the MCP tools cover all of them, and `status` lives
  at `codegraph_status`.
- `init -i` became the inverse: indexing is the default, `--no-index` opts
  out.
- No per-event create/modify/delete handling — debounce then full re-index.
