# CodeGraph — documentation index & roadmap

All documentation lives in `docs/`. The README is the landing page; everything
detailed is here. Each spec records the *what/why* of a shipped component —
when behavior changes, update the spec with it.

## Documentation map

| Document | Audience | Covers |
|---|---|---|
| [codegraph.md](codegraph.md) | Agents | Usage guide **embedded into the binary** as MCP server instructions: session binding, tool selection by intent, timeout/resume, response formats, sandbox/diff contracts |
| [configuration.md](configuration.md) | Users / ops | `.codegraph/config.toml` reference: languages, `[[effect_rules]]` + defaults, storage backends & sharding, `[sandbox]`, `[embedding]`, ignore files, CLI flags, watcher |
| [mcp.md](mcp.md) | Users / ops | Running the MCP server (stdio + Streamable HTTP), client configuration per agent, the 27-tool catalog, common arguments, token conventions |
| [sandbox.md](sandbox.md) | Users / agents | Behavior sandbox: `[sandbox]` config, the Rhai mock contract with examples, run semantics, `codegraph_sandbox` / `codegraph_diff_simulate` / `codegraph_origin_simulate` |
| [codesmell.md](codesmell.md) | Users / agents | The CodeSmell team-convention linter: `.codesmell/policy.toml` schema, rules, CLI, agent workflow |
| [benchmarks/storage-perf.md](benchmarks/storage-perf.md) | Devs | Storage benchmark snapshot (in-memory vs sqlite vs lmdb) |
| [specs/](specs/) | Devs | Per-component design specs, kept in sync with the code |

## Specs

| # | Spec | Status |
|---|---|---|
| 01 | [Workspace bootstrap](specs/01-bootstrap.md) — 11-crate layout, conventions | done |
| 02 | [Core types](specs/02-core-types.md) — semgraph model (Symbol, chains, markers, effects) | done |
| 03 | [Storage layer](specs/03-db-layer.md) — `Storage` trait, sqlite/lmdb/redis/postgres/mysql, sharding | done |
| 04 | [Extraction](specs/04-extraction.md) — tree-sitter, declarative `LangSpec`, walker, effects | done |
| 05 | [Call resolution](specs/05-resolution.md) — ingest resolve phases, scoring, call-name index | done |
| 06 | [GraphIndex & context](specs/06-graph-context.md) — engines, queries, GraphApi, diff engine | done |
| 07 | [MCP server](specs/07-mcp-server.md) — rmcp, stdio + HTTP, 27 tools, token conventions | done |
| 08 | [Installer](specs/08-installer.md) — multi-agent client setup, idempotence | done |
| 09 | [CLI & watcher](specs/09-cli-watcher.md) — init/deinit/embed/serve, debounced full re-index | done |
| 10 | [Release & CI](specs/10-release.md) — CI jobs, distribution, features, publish order | done |
| 11 | [CodeSmell](specs/11-codesmell.md) — convention linter over in-memory CodeGraph facts | done (MVP) |

## Roadmap / fast-follow

- **CodeSmell**: convention discovery (statistics → candidate policies with
  confidence), coverage threshold enforcement, policy history/evolution,
  runtime/engineering policies (timeouts/retries via effect classification).
- **MCP HTTP hardening**: enforce `--api-key` bearer auth, mount `/health` /
  `/metrics` observability endpoints (flags currently accepted but inert).
- **Watcher**: honor `.codegraphignore` (today only `.gitignore` is
  consulted for event filtering).
- **Embeddings**: make `codegraph-api`'s unconditional feature pull-in
  optional so slim builds can drop the ONNX runtime.

## History

The Rust rewrite plan that originally lived here (Node/Edge model,
`codegraph-db`, `codegraph-resolve`, hand-rolled JSON-RPC, <15 MB target) is
superseded — each deviation is recorded in the "Deviations" section of the
relevant spec. The rewrite itself completed: single ~58 MB binary with every
backend bundled, semgraph model, 27-tool MCP server on the rmcp SDK.
