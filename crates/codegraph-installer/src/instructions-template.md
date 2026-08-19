# CodeGraph

This project has a CodeGraph MCP server configured. CodeGraph is a tree-sitter
semantic graph of every symbol and call chain in the workspace. Reads are
sub-millisecond and return structural information grep cannot.

## When to prefer codegraph

Use codegraph for **structural** questions — what calls what, what would
break, where is X defined, what is X's signature. Use native grep/read only
for literal text queries.

| Question | Tool |
|---|---|
| "Where is X defined?" | `codegraph_search_symbol` |
| "Show me X by id / exact name" | `codegraph_symbol` |
| "What calls Y?" | `codegraph_callers` |
| "What does Y call?" | `codegraph_callees` |
| "What would break if I changed Z?" | `codegraph_impact` |
| "Show me Y's call chain" | `codegraph_flow` |
| "Give me focused context for a task" | `codegraph_context` |
| "What files exist under path/" | `codegraph_files` |
| "Is the index healthy?" | `codegraph_status` |

## Rules of thumb

- **Trust codegraph results.** They come from a full AST parse. Do NOT
  re-verify with grep.
- **Don't grep first** when looking up a symbol by name.
- **`codegraph_context` is one call** — don't chain search + symbol yourself.
- **Duplicate names** return `ambiguous: true` — retry with the numeric `id`.

## If no index exists yet

Query tools refuse until the session is bound. Call
`codegraph_init {"path": ...}` (non-blocking — does NOT index by default),
then `codegraph_index {}` to build the index. On the CLI, `codegraph init`
creates `.codegraph/` and indexes in one step.
