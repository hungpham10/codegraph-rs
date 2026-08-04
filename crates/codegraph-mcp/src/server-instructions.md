# Codegraph — code intelligence over an indexed semantic graph

Codegraph is a SQLite semantic graph of every symbol (function/method/class/…)
and its call chain in the workspace. Reads are sub-millisecond. Consult it
BEFORE writing or editing code, not during.

## Answer directly — don't delegate exploration

For "how does X work", architecture, trace, or where-is-X questions, answer
DIRECTLY using 2-3 codegraph calls: `codegraph_context` first, then drill
down with `codegraph_symbol` or `codegraph_callers`/`codegraph_callees`.
Codegraph IS the pre-built search index — delegating the lookup to a separate
file-reading sub-task repeats work codegraph already did.

## Tool selection by intent

| Intent | Tool |
|---|---|
| "What is the symbol named X?" | `codegraph_search_symbol` (match: contains/prefix/suffix/exact, kind filter) |
| "What's the deal with this task / area?" | `codegraph_context` (primary) |
| "What calls this?" | `codegraph_callers` |
| "What does this call?" | `codegraph_callees` |
| "What would changing this break?" | `codegraph_impact` |
| "Show me this symbol's call chain." | `codegraph_flow` |
| "Find functions with a loop calling X." | `codegraph_search_flow` |
| "Who calls the library function foo?" | `codegraph_references` / `codegraph_search_by_call` |
| "What methods does class X have?" | `codegraph_class_methods` |
| "What fields/methods does class X have?" | `codegraph_class` |
| "List all classes / interfaces." | `codegraph_list_classes` / `codegraph_list_interfaces` |
| "What params/locals does function X have?" | `codegraph_function_scope` |
| "Which symbols are annotated @RestController?" | `codegraph_search_by_annotation` |
| "What does this project depend on?" | `codegraph_dependencies` |
| "Show me this symbol by id / exact name." | `codegraph_symbol` |
| "What's in directory X?" | `codegraph_files` |
| "Is the index ready / what's its size?" | `codegraph_status` |

## Disambiguating duplicate names

`codegraph_symbol`, `codegraph_class_methods`, `codegraph_class`, and
`codegraph_function_scope` accept an `id` (numeric symbol id) to disambiguate
when multiple symbols share a name. When a name is ambiguous the tool returns
`"ambiguous": true` with the full `matches` list — retry passing `id` ALONE.

`codegraph_search_symbol` supports four match modes: `contains` (substring
anywhere, default), `prefix`, `suffix` (e.g. `match="suffix", query="Service"`
finds every `*Service` class), and `exact`. Use `total` + `offset` to page.

## Trust the results

Codegraph returns AST-derived structural data. Do NOT re-verify with grep —
that's slower, less accurate, and wastes context.

## Symbols are numbers

Symbols are identified by numeric `id` (global registry, ≥ 100). Call-chain
patterns in `codegraph_search_flow` mix marker names (`LOOP`, `IF_TRUE`,
`IF_FALSE`, `BRANCH_END`, `RETURN`, `LOOP_BACK`, `SWITCH_CASE`, `SWITCH_END`,
`BREAK`, `CONTINUE`, `THROW`), symbol ids, and symbol names.
