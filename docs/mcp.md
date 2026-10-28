# MCP server reference

CodeGraph exposes the semantic graph to AI agents over the Model Context
Protocol. One binary serves any MCP client — Claude Code, Cursor, Codex CLI,
opencode, Hermes, Antigravity — over **stdio** or **Streamable HTTP**.

This page is the operator/dev reference: transports, client setup, the tool
catalog, and the token-saving conventions. The agent-facing usage guide that
ships **inside the binary** (returned as server `instructions` after
`initialize`) is [docs/codegraph.md](codegraph.md).

## Running the server

```sh
codegraph serve --mcp                                        # stdio (1 process = 1 session)
codegraph serve --mcp --http --addr 0.0.0.0:8123             # Streamable HTTP (POST/GET/DELETE + SSE)
```

| Flag | Default | Notes |
|---|---|---|
| `--http` | off | Streamable HTTP transport, mounted at both `/` and `/mcp` |
| `--addr` | `0.0.0.0:8123` | Listen address |
| `--allow-host <host>` | — | Extra accepted `Host` headers (repeatable). Built-in allowlist: `localhost`, `127.0.0.1`, `::1` — this is rmcp's DNS-rebinding protection |
| `--allow-any-host` | off | Accept any `Host` header (trusted LAN/docker only, never public) |
| `--api-key <key>` | — | Accepted and parsed, but **not yet enforced** (auth is a known TODO; treat the HTTP port as unauthenticated) |
| `--enable-observability` | `true` | Accepted; `/health`, `/metrics` endpoints are a TODO and not mounted yet |
| `--format` | `minimize` | Default response encoding (`minimize` \| `medium`); overridable per session and per call |
| `--path <dir>` | cwd | Pre-seeds the stdio session root. When the client starts the server from `/` (Claude Desktop) the session starts empty — the agent binds with `codegraph_init` |

Notes:

- **stdio**: one process = one session slot. Startup `--path` is only a
  pre-seed; an empty session is fine — the agent binds the workspace with
  `codegraph_init {"path": ...}`.
- **HTTP**: each connection (`mcp-session-id`) gets its own fresh
  `CodegraphServer` via the factory — nothing is shared between connections
  but the process. The agent binds the workspace root with `codegraph_init`
  inside that connection. Legacy session mode is enabled: clients negotiating
  a pre-2026-07-28 protocol still get sessions; SEP-2567 requests run
  stateless.
- `tools/list` responses carry `ttlMs: 0` + `cacheScope: public` per
  SEP-2549 / protocol 2026-07-28.

## Client configuration

stdio (any MCP client):

```json
{
  "mcpServers": {
    "codegraph": {
      "command": "codegraph",
      "args": ["serve", "--mcp", "--path", "/abs/path/to/project"]
    }
  }
}
```

HTTP (remote / Docker):

```json
{ "type": "http", "url": "http://<host>:8123/mcp" }
```

Per-client quirks worth knowing:

| Client | Config location | Note |
|---|---|---|
| Claude Code | `~/.claude/settings.json` → `mcpServers.codegraph` | Starts servers from `/` — rely on `codegraph_init`, or pass absolute `--path` |
| Cursor | `.cursor/mcp.json` | Wrong cwd otherwise — always inject an absolute `--path` |
| Codex | `~/.codex/config.toml` → `[mcp_servers.codegraph]` | TOML edits must preserve sibling tables |
| opencode | `opencode.jsonc` | Preserve comments when editing |

## Tool catalog (27 tools)

### Session / admin

| Tool | What it does |
|---|---|
| `codegraph_init` | Bind session to a workspace root (idempotent, creates `.codegraph/`); `index` defaults `false` (non-blocking); optionally sets session `detail`/`format` defaults |
| `codegraph_deinit` | Release the session (root → null); `.codegraph/` stays on disk; query tools refuse while unbound |
| `codegraph_index` | Full re-index of the bound workspace |
| `codegraph_status` | Index health: symbol/chain/edge/file counts |
| `codegraph_query_usage_report` | Server telemetry (calls/errors, answer bytes, estimated source bytes read); `reset: true` clears |

### Search

| Tool | What it does |
|---|---|
| `codegraph_search_symbol` | Name search; `match`: `contains` (default) / `prefix` / `suffix` / `exact` / `semantic` (opt-in KNN) / `hybrid` (RRF merge); kind filter; pagination |
| `codegraph_symbol` | Lookup by `id` or exact `name`; duplicates → `ambiguous` + match list, retry with `id` |
| `codegraph_search_by_annotation` | Symbols by annotation substring (e.g. `@RestController`), optional kind filter |
| `codegraph_search_by_call` | Functions calling a class/method name in their bodies — includes unresolved external calls, with per-call-site context |
| `codegraph_references` | Functions calling a library call whose name contains `query` |
| `codegraph_search_flow` | Functions whose call chain contains a pattern (comma tokens: marker names, symbol names, or numeric ids) |
| `codegraph_files` | Indexed files under a path prefix |
| `codegraph_dependencies` | Internal vs external module-prefix dependencies, sorted by call-site count |

### Graph queries

| Tool | What it does |
|---|---|
| `codegraph_callers` | Transitive callers (`depth`, default 1) |
| `codegraph_callees` | Direct callees |
| `codegraph_impact` | Transitive impact radius (`max_depth`, default 3) |
| `codegraph_flow` | Call chain: markers + callee names + call sites (line / condition / effect / args) |
| `codegraph_class_methods` | Methods of a class/interface/enum |
| `codegraph_class` | Class details with fields and methods as separate lists |
| `codegraph_list_classes` | All class symbols (paginated) |
| `codegraph_list_interfaces` | All interface symbols (paginated) |
| `codegraph_function_scope` | A function's parameters and local variables |
| `codegraph_context` | Composed context: search + callers + callees + optional source (markdown) |

### Diff / sandbox

| Tool | What it does |
|---|---|
| `codegraph_diff` | Unified diff → read-only DRAFT graph-impact report |
| `codegraph_sandbox` | Compile an entry function + in-flow callees to machine code (Cranelift JIT), run with Rhai mocks |
| `codegraph_diff_simulate` | Diff → sandbox run on current index vs temp index from `base_ref` (`git archive`); compare traces |
| `codegraph_origin_simulate` | Ref (default `HEAD`) vs working-tree sandbox run, no diff needed |

See [docs/sandbox.md](sandbox.md) for the sandbox contract.

## Common arguments

- `detail`: `minimal` | `medium` (default) | `verbose` — per-call override of
  the session default set at `codegraph_init`.
- `format`: `minimize` (default) | `medium` — per-call override of session /
  startup default.
- `limit` / `offset`: pagination (defaults: `limit` 20 for most, 10 for
  `references`, 5 for `context`).
- `timeout_ms` (default 20000; `0` = no limit) + `resume`: broad searches
  (`codegraph_search_symbol`, `codegraph_search_flow`, `codegraph_references`,
  `codegraph_search_by_annotation`, `codegraph_search_by_call`,
  `codegraph_list_classes`, `codegraph_list_interfaces`) error on timeout with
  a `"resume": "<id>"` id — retry the **exact same call** plus the id to
  continue without re-scanning. Resume ids are short-lived, in-process, and
  tied to the query args; re-index or restart invalidates them.

## Response conventions (token-lean by design)

Full contract with examples lives in [docs/codegraph.md](codegraph.md#response-formats-binance-style-minimal);
summary:

- **`format=minimize`** (default): symbol items are fixed 14-element
  **positional arrays** `[id, name, kind, scope, scope_id, type_ref,
  type_name, file, line, end_line, signature, doc, annotations, language]`;
  `detail` is ignored.
- **`format=medium`**: keyed objects; `detail` selects fields
  (`minimal` = id/name/kind/file/line, `medium` adds `signature`, `verbose`
  = full Symbol).
- **Omission rule** (both formats): object keys holding default values
  (`null`, `false`, `""`, `[]`, `{}`, and `0` for the sentinels `scope_id` /
  `type_ref` / `end_line`) are omitted — *absent means default*. Arrays never
  drop positions; counts (`total`, `limit`, `offset`) always stay.
- **Paths are workspace-relative** in responses.
- **Disambiguation**: duplicate names return `ambiguous: true` + `matches`;
  retry with `id` alone.

## Architecture

`crates/codegraph-mcp` implements `rmcp::handler::server::ServerHandler` on the
official Rust SDK (`rmcp` v3.1.x; features `transport-io`, and `http` adds
`transport-streamable-http-server` + `axum`). Tool definitions are a static
`ToolDef` list (`tools.rs::tool_defs`) — the single source of truth for
`tools/list`; dispatch goes through `run_tool` → session admin tools →
`GraphApi` for queries → `SharedGraphIndex` + sboxes for the sandbox trio.
Session state (root binding, index handle, detail/format defaults, resumable
search cursors) lives in `session.rs`.

## Related

- Agent usage guide (embedded in the binary): [docs/codegraph.md](codegraph.md)
- Configuration: [docs/configuration.md](configuration.md)
- Sandbox & mocks: [docs/sandbox.md](sandbox.md)
