---
name: codegraph-development-workflow
description: CodeGraph-first development workflow — use before grep/read when locating symbols, tracing call chains, or estimating change impact in a workspace indexed by CodeGraph MCP.
---

# CodeGraph-First Development Workflow

CodeGraph answers **structural** questions (where is this symbol, who calls it, what breaks if I change it) faster and cheaper than grep. Use it as the FIRST step for structural questions; fall back to grep/read only for the cases listed below.

## Standard loop

```
1. codegraph_init      {path: "<abs workspace root>"}   # bind session (does NOT index)
2. codegraph_context   {query: "..."}                    # symbol + callers + callees in one call
3. codegraph_search_symbol / codegraph_symbol           # locate + signature
4. codegraph_callers / codegraph_callees / codegraph_impact   # upstream / downstream / blast radius
   ... edit code ...
5. codegraph_index {}                                    # re-index only when you need fresh data
6. codegraph_flow / codegraph_search_flow                # verify control flow after the edit
```

### Tool cheatsheet

| Question | Tool |
|---|---|
| Find symbols by name (contains/prefix/suffix/semantic) | `codegraph_search_symbol` |
| Exact symbol by id or name | `codegraph_symbol` |
| Who calls this / what does it call | `codegraph_callers` / `codegraph_callees` |
| What breaks if I change X | `codegraph_impact` |
| Control flow of one function (branches, loops, effects) | `codegraph_flow` |
| Full execution trace to a target: all caller→target paths with call/return/throw/branch info | `codegraph_trace_full_flow` |
| Which functions match a flow pattern (LOOP, THROW, CALL_ENTER…) | `codegraph_search_flow` |
| Bundled context (callers + callees + optional source) | `codegraph_context` |
| Where a library call is used | `codegraph_references` |
| List functions/params/locals of a function | `codegraph_graphcode_function_scope` |
| Class members, all types, files, stats | `codegraph_graphcode_class` / `codegraph_graphcode_list_types` / `codegraph_graphcode_files` / `codegraph_graphcode_stats` |
| Find by annotation (@Override, @GetMapping…) | `codegraph_graphcode_search_by_annotation` |

There is no generic `codegraph_search` or `codegraph_flows` tool — symbol search is `codegraph_search_symbol` and flow-pattern search is `codegraph_search_flow`.

### Example calls

```json
codegraph_init        {"path": "/abs/path/to/workspace"}
codegraph_context     {"query": "parse config", "limit": 5}
codegraph_impact      {"node": 1234, "max_depth": 3}
codegraph_search_flow {"pattern": "THROW,CATCH"}
```

## Effects & markers (how call sites get classified)

Flow output annotates each call site with an **effect** (semantic class such as `sql_query`, `http_call`, `panic`, …) and the call chain carries **markers** (control-flow landmarks: `try_start`, `throw`, `loop`, `return`, …). Both come from pattern-match rules matched first-to-last:

- Built-in default rules are bundled with the classifier; projects can add overrides in `.codegraph/config.toml` under a single `[[effect_rules]]` table — each rule sets `effect` and/or `marker` (at least one), with a `matcher` of `exact` | `prefix` | `contains` (prefer exact/prefix; `contains` can over-match, e.g. `longjmp` also hits `siglongjmp`).
- The legacy `[[flow_rules]]` key is gone: old keys are ignored with a warning.
- Runtime changes go through the MCP tool `codegraph_config_effects` (`op`: `list` | `add` | `remove`). New rules apply to the current thread immediately, but call records are **re-classified only on the next `codegraph_index`** — re-index to see effects change in flow output.
- Markers/ids are a stable serialization contract — never renumber existing ones (`crates/codegraph-core/src/semgraph.rs`). Do not insert markers between `CALL_ENTER` and `CALL_EXIT` (`CALL_ENTER` reads the next chain position as the callee).

## When to fall back to grep/read

| Situation | Use |
|---|---|
| Text literals (strings, comments, config keys) | grep |
| `docs/`, `README.md`, `config.toml`, `.md` files | read |
| Files not indexed by the language parsers | read |
| You just edited code and haven't re-indexed | read to confirm |
| You need the full implementation of a large file | read after `codegraph_symbol` narrows the location |

## Critical rules

1. **The index is a snapshot.** Results are stale until you run `codegraph_index`. After your own edits, always re-index (or read the file) before trusting a query about the edited code.
2. **`codegraph_init` does NOT index by default** — it just binds the session. Call `codegraph_index {}` when you need a fresh index.
3. **Large indexes:** search tools take `timeout_ms` (default 20000). On timeout the error returns a `resume` id — retry the SAME call with the SAME arguments plus `resume`, instead of re-scanning from scratch.
4. **Unbound session:** if tools report the session has no root, call `codegraph_init` with an absolute path. Never substitute a filesystem scan.
5. Symbol ids are per-index. After re-indexing, do not reuse ids from a previous snapshot — re-resolve by name via `codegraph_search_symbol` or `codegraph_symbol`.

## Hygiene

- `codegraph_deinit` releases the session; `.codegraph/` stays on disk and is git-ignored — never commit it.
- `codegraph_query_usage_report` shows token savings per tool; use it to tune how often you reach for graph queries vs grep.
