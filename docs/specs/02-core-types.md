# Spec 02 — Core types (semgraph model)

**Status**: ✅ done — implemented in `crates/codegraph-core/src/semgraph.rs`
(wire-breaking replacement for the original `NodeKind`/`EdgeKind` design).

## Goal

One stable type vocabulary shared by extraction, storage, and MCP — the single
source for everything serialized into the DB and over the wire.

## Model

Every symbol gets a **global id** (`SymbolId = u64`, monotonic, starts at
`SYMBOL_BASE = 100`; ids `1..100` are reserved control-flow markers). A
function's call chain is a `Vec<u64>` mixing markers and callee symbol ids —
edges are *derived* from chains, not stored as a separate relation.

### Markers (`marker_name` / `marker_id` round-trip)

`LOOP=1`, `RECURSIVE_CALL=2`, `IF_TRUE=3`, `IF_FALSE=4`, `BRANCH_END=5`,
`RETURN=6`, `LOOP_BACK=7`, `SWITCH_CASE=8`, `SWITCH_END=9`, `BREAK=10`,
`CONTINUE=11`, `THROW=12`.

### `SymbolKind` (12 values, replaces the 22-value `NodeKind`)

`Function`, `Method`, `Class`, `Interface`, `Enum`, `Variable`, `Constant`,
`Parameter`, `Field`, `Module`, `File`, `Config` — serde snake_case, with
`as_str()` / `parse()`.

### `Symbol`

`{ id, name, kind, scope: ScopeLevel, scope_id, type_ref, type_name, file,
line, end_line, signature, doc, annotations, language }` — `scope_id` is the
containment link (method → class, param/local → function; `0` = global),
`line..=end_line` gives the body span (LOC = `end_line − line + 1`).

### Calls & effects

- `CallRecord` — unresolved call: `{ caller_id, call_name, position,
  arg_exprs, line, condition, is_loop_body, effect, effect_desc,
  target_class, target_method }` (structural resolution hints).
- `EdgeMeta` — resolved edge: `(caller_id, callee_id)` + `position`,
  `condition` (guard text), `effect`, `is_loop_body`, `is_recursive`.
- `EffectType` (10 values): `none`, `sql_query`, `sql_write`, `cache_read`,
  `cache_write`, `http_call`, `event_emit`, `file_read`, `file_write`, `log` —
  classified from callee names by `[[effect_rules]]` (see
  [configuration.md](../configuration.md)).
- `EffectCallPattern`: `Prefix` / `Contains` / `Exact` (untagged serde) —
  shared schema for `config.toml`.

### Query-result projections

`FlowResult` / `FlowCall`, `ResolveResult` (ambiguous-name protocol),
`SearchFlowResult`, `CallSiteResult`, `MemberInfo`, `ClassInfo`,
`FunctionScope`, `DependenciesReport`, `DbStats`, `FileInfo`,
`SymbolMatch` (contains/prefix/suffix/exact).

## Error

`thiserror` enum in `error.rs`: `Io`, `Db`, `Parse`, `Search`,
`DepthExceedsLimit`, `Invalid`, `NotInitialized`, `MissingMocks` (sandbox
link failure), `Other`; `Result<T>` alias exported crate-wide.
