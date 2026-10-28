# Spec 06 — GraphIndex, GraphApi & context builder

**Status**: ✅ done — implemented in `crates/codegraph-graph`
(`GraphIndex`, `SharedGraphIndex`, `diff.rs`), `crates/codegraph-api`
(`GraphApi`), `crates/codegraph-context`.

## GraphIndex

The main index: an in-memory registry (source of truth) over pluggable
storage (spec 03), plus two search engines:

- **Chain engine** `Search<u64>` — radix over call chains; callers are found
  by substring-searching `[callee_id]` across chains (KMP + optional bloom
  filters), not by BFS over a stored edge table.
- **Name engine** `Search<u8>` — radix over lowercase symbol names for
  contains/prefix/suffix/exact.

Public query surface (sync unless noted):

| Method | Notes |
|---|---|
| `symbol_by_id` / `resolve_by_name_or_id` | id-first; duplicate names → `ambiguous` + matches |
| `callers(id, depth)` (async) / `callees(id)` (async) | transitive BFS on the chain engine / direct chain read |
| `flow(id)` (async) | chain + rendered descriptions + call sites |
| `search_flow(pattern)` (async) | functions whose chain contains ids/markers |
| `callers_by_call_name(query, limit)` (async) | call-name index, includes unresolved calls |
| `function_scope(id)` | parameters + locals via `scope_index` |
| `members_of` / `list_methods_of_class` / `get_class_info` | class structure |
| `list_symbols_by_kind` / `search_by_annotation` (+ `_resumable` variants) | paginated, deadline-aware |
| `files()` / `dependencies_report()` / `stats()` | topology + health |
| `diff_assess(&ParsedDiff, root)` (async) | unified diff → read-only `DiffReport` (touched symbols, affected flows with marker windows, transitive callers) — the engine behind `codegraph_diff` |

`SharedGraphIndex` (Arc + RwLock) adds `ensure_fresh()`: probes the storage
version and rebuilds when the index was re-written (watcher / another
process).

## GraphApi (`crates/codegraph-api`)

Async facade over `Arc<SharedGraphIndex>` — every method ensures freshness
then delegates. Adds pagination (`Pagination`), resumable searches with
server-side cursors (`SearchSessionStore`, `SearchCursor` +
`SearchCursorPhase`) and the timeout/resume protocol surfaced by the MCP
tools, plus `context_markdown` (delegating to codegraph-context).

## codegraph-context

Composes the "give me context for X" answer used by `codegraph_context`:
search candidates → for each, the symbol + direct callers + direct callees +
optional on-disk source slice → markdown serialization. Token-lean by design.

## Tests

`crates/codegraph-api/tests/api.rs` seeds a synthetic graph via
`GraphIndex::open(tempfile)` + hand-built `ParseResult`s (symbols, chains,
call records) and exercises the whole query surface including resume
round-trips with `TIMEOUT_EXPIRE_IMMEDIATELY`; `codegraph-graph` keeps
inline unit tests for engines and ingest invariants.
