---
name: codegraph-orchestrator
description: Global coordination and structural codebase navigation via CodeGraph MCP
trigger: auto
paths:
  - "**/*"
---

# CodeGraph System Instructions

This project is backed by a **CodeGraph MCP server** — a local tree-sitter
semantic graph of every symbol and call chain in the workspace. Reads are
sub-millisecond and return structural information grep cannot match.

---

## 🚨 CRITICAL CONSTRAINTS (Read First)

- **NEVER use generic text-search, grep, or file-reading tools** when a
  symbol, reference, or definition can be located with CodeGraph.
- **Do NOT re-verify** CodeGraph results with file reads — the graph is the
  single source of truth for codebase structure.
- **Handle unbound sessions:** query tools refuse until the session is bound.
  Call `codegraph_init {"path": ...}` (non-blocking, does NOT index), then
  `codegraph_index {}` to build/refresh the index.
- **Minimize token overhead:** prefer targeted structural queries and
  `id`-based lookups over dumping file contents into context.

---

## 🛠️ Tool Selection Guide

Prefer codegraph for **structural** questions. Use filesystem tools only for
literal text queries or applying edits.

| Intent / Question | Recommended MCP Tool |
| :--- | :--- |
| *"Where is symbol X defined?"* | `codegraph_search_symbol` (contains/prefix/suffix/exact) |
| *"Show me this symbol by id / exact name"* | `codegraph_symbol` |
| *"What calls function Y?"* | `codegraph_callers` |
| *"What does Y call directly?"* | `codegraph_callees` |
| *"What breaks if I modify Z?"* | `codegraph_impact` |
| *"Show me Y's call chain"* | `codegraph_flow` |
| *"Find flows containing a pattern (loop + call)"* | `codegraph_search_flow` |
| *"Give me focused, aggregated context for a task"* | `codegraph_context` |
| *"Who calls the library function foo?"* | `codegraph_references` |
| *"What fields/methods does class C have?"* | `codegraph_class` |
| *"Which symbols are annotated @X?"* | `codegraph_search_by_annotation` |
| *"What files exist under path/?"* | `codegraph_files` |
| *"Is the index healthy?"* | `codegraph_status` |
| *"What does this MR change in the graph?"* | `codegraph_diff` |
| *"Simulate a flow's behavior with mocks"* | `codegraph_sandbox` / `codegraph_diff_simulate` |

---

## 💡 Rules of Thumb

1. **`codegraph_context` first** — it aggregates search + callers + callees
   in one call; don't chain searches manually.
2. **Query impact before editing** — `codegraph_impact` pinpoints downstream
   effects so you only touch relevant files.
3. **Trust the results** — AST-derived. If a lookup yields nothing, the
   symbol is not in the active workspace index.
4. **Duplicate names** → the tool returns `ambiguous: true` with matches;
   retry with the numeric `id` alone.

The full, always-current guide (timeout/resume protocol, response formats,
sandbox contracts) ships inside the binary as the server instructions — see
`docs/codegraph.md` in the CodeGraph repository.
