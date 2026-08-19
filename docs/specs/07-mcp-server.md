# Spec 07 — MCP server

**Status**: ✅ done — implemented in `crates/codegraph-mcp` on the official
Rust SDK (`rmcp` v3.1.x). The hand-rolled JSON-RPC design was replaced by the
SDK once it matured; the 9-tool surface grew to 27.

Full operator reference: [docs/mcp.md](../mcp.md). Agent-facing guide embedded
in the binary: [docs/codegraph.md](../codegraph.md).

## Architecture

`CodegraphServer` implements `rmcp::handler::server::ServerHandler` and holds
a `Session` (root binding + index handle + detail/format defaults), usage
telemetry, and a `SearchSessionStore` for resumable search cursors.

- `get_info()` advertises `enable_tools()` + instructions from
  `SERVER_INSTRUCTIONS` — `include_str!("../../../docs/codegraph.md")`, so
  the shipped guide and the repo doc are the same file.
- `list_tools()` returns the static `ToolDef` list (`tools.rs::tool_defs`) —
  single source of truth — with `ttl_ms(0)` + `cache_scope(Public)`
  (SEP-2549 / protocol 2026-07-28).
- `call_tool()` rejects unknown names as `method_not_found`, then dispatches:
  admin tools (`init`/`deinit`/`index`) on the session; queries through
  `GraphApi`; the sandbox trio directly over `SharedGraphIndex` + sboxes.

## Transports

- **stdio** (`stdio.rs`) — `serve(rmcp::transport::io::stdio())`; one process
  = one session. Startup `--path` is only a pre-seed (Claude Desktop starts
  from `/` — the empty session binds via `codegraph_init`).
- **Streamable HTTP** (`http.rs`, feature `http`) — `StreamableHttpService`
  on axum, mounted at `/` and `/mcp`; a factory builds a fresh
  `CodegraphServer` per `mcp-session-id`; DNS-rebinding protection via
  `with_allowed_hosts`; `with_legacy_session_mode(true)` keeps sessions for
  pre-2026-07-28 clients. Known TODOs: bearer auth (`--api-key` parsed but
  unenforced) and `/health`//`/metrics` endpoints (flag accepted, not
  mounted).

## Tools (27)

Session/admin: `codegraph_init`, `codegraph_deinit`, `codegraph_index`,
`codegraph_status`, `codegraph_query_usage_report`.
Search: `codegraph_search_symbol` (contains/prefix/suffix/exact + opt-in
semantic/hybrid), `codegraph_symbol`, `codegraph_search_by_annotation`,
`codegraph_search_by_call`, `codegraph_references`, `codegraph_search_flow`,
`codegraph_files`, `codegraph_dependencies`.
Graph: `codegraph_callers`, `codegraph_callees`, `codegraph_impact`,
`codegraph_flow`, `codegraph_class_methods`, `codegraph_class`,
`codegraph_list_classes`, `codegraph_list_interfaces`,
`codegraph_function_scope`, `codegraph_context`.
Diff/sandbox: `codegraph_diff`, `codegraph_sandbox`,
`codegraph_diff_simulate`, `codegraph_origin_simulate` — full contract in
[docs/sandbox.md](../sandbox.md); engine in `codegraph-sboxes`.

## Token-lean output conventions

- `detail` minimal/medium/verbose (session default at `codegraph_init`,
  per-call override) and `format` minimize/medium (startup → session → call).
- `minimize` (default): symbols as fixed 14-element positional arrays;
  `medium`: keyed objects with `omit_defaults` (absent = default);
  paths relativized to the workspace root.
- Broad searches take `timeout_ms` + `resume`: on timeout the tool errors
  with a resume id; retry the same call + id to continue (short-lived,
  in-process cursors in `SearchSessionStore`).

## Tests

Inline unit tests in `tools.rs` cover the formatting helpers (detail/format
parsing, positional-array schema index-by-index, `omit_defaults`,
relativization, tools/list cache fields); `http.rs` has an axum `oneshot`
smoke test asserting `initialize` returns `serverInfo.name = "codegraph"`.
Query behavior is tested one layer down in `crates/codegraph-api`.
