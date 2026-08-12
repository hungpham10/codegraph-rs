# Codegraph — code intelligence over an indexed semantic graph

Codegraph is a SQLite semantic graph of every symbol (function/method/class/…)
and its call chain in the workspace. Reads are sub-millisecond. Consult it
BEFORE writing or editing code, not during.

## Session & workspace selection

Codegraph MCP manages **one session per process**. Bind it to a workspace root
before querying:

- `codegraph_init {"path": "/abs/path/to/project"}` — bind the session to
  that root and create `.codegraph/` (idempotent) if missing. Binding is fast
  and **non-blocking: it does NOT index by default** (`index` defaults to
  `false`). After binding, call `codegraph_index {}` to build/refresh the
  index (or pass `"index": true` to `codegraph_init` to index immediately).
  Re-running with a different `path` re-points the session. Optionally set
  the default output detail for list tools with
  `"detail": "minimal" | "medium" | "verbose"` (see below).
- `codegraph_deinit {}` — release the session (the `.codegraph/` and index files
  stay on disk). An unbound session **refuses every query tool** until
  `codegraph_init` binds it again.

Start with `codegraph_init {"path": ...}` for the project you are working on,
then `codegraph_index {}` if the index is empty/stale (check
`codegraph_status`). The `--path` given at server startup, if any, is already
bound.

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
| "Bind the session to a project (creates .codegraph/, non-blocking — does NOT index by default)" | `codegraph_init` (`path` required; `index` defaults to `false`) |
| "Build/refresh the index for the bound session" | `codegraph_index` |
| "Release the current session" | `codegraph_deinit` |
| "Run an entry function in the behavior sandbox" | `codegraph_sandbox` (per-function Rhai mocks) |
| "Diff này (MR/patch/git diff) ảnh hưởng gì tới graph?" | `codegraph_diff` (read-only draft) |
| "MR này đổi hành vi flow ra sao (trước vs sau)?" | `codegraph_diff_simulate` (sandbox before/after) |
| "Flow này ở `origin/main` đang chạy thế nào so với code local của tôi (chưa commit)?" | `codegraph_origin_simulate` (ref vs working tree) |

## Disambiguating duplicate names

`codegraph_symbol`, `codegraph_class_methods`, `codegraph_class`, and
`codegraph_function_scope` accept an `id` (numeric symbol id) to disambiguate
when multiple symbols share a name. When a name is ambiguous the tool returns
`"ambiguous": true` with the full `matches` list — retry passing `id` ALONE.

`codegraph_search_symbol` supports four match modes: `contains` (substring
anywhere, default), `prefix`, `suffix` (e.g. `match="suffix", query="Service"`
finds every `*Service` class), and `exact`. Use `total` + `offset` to page.

## Large indexes: timeout + resume

On very large indexes a broad search (`codegraph_search` / `codegraph_search_symbol`)
can exceed its time budget. Both tools accept `timeout_ms` (default `2000`;
`0` = no limit). When the budget runs out mid-search the tool **errors** and
does NOT return partial results — the message includes `"resume": "<id>"` and a
progress count:

```
codegraph_search_symbol timed out after 2000ms (collected 134 symbols so far).
Retry the same call with the same arguments plus "resume": "<id>" to continue
the search from where it stopped.
```

To explore effectively and continuously: **retry the exact same call with the
same arguments plus the `resume` id** — the search continues exactly where it
stopped (nothing is re-scanned, nothing is lost) and eventually returns the
full results. You can keep retrying as many times as needed; each retry that
times out yields a fresh resume id.

- Resume ids are **short-lived and in-process**: re-indexing the workspace
  (version bump) or restarting the server invalidates them. If a resume id is
  rejected, retry the search **without** `resume`.
- A resume id is tied to its query/mode/kind — passing it with different
  arguments is rejected; retry without `resume`.
- When `codegraph_search_symbol` completes with more pages available, the
  response includes a `resume` id in addition to `total`/`has_more` — pass it
  on the next call (with a new `offset`) to page further **without re-scanning**
  the index.
- `codegraph_search` on success returns a plain array (no `resume` field); if
  you need more results, narrow the query or use `codegraph_search_symbol`.

## Trust the results

Codegraph returns AST-derived structural data. Do NOT re-verify with grep —
that's slower, less accurate, and wastes context.

## Output detail & token usage

Symbols in list-tool responses (`codegraph_search`, `codegraph_callers`,
`codegraph_callees`, `codegraph_impact`, `codegraph_search_symbol`,
`codegraph_search_by_annotation`, `codegraph_list_classes`,
`codegraph_list_interfaces`, and the symbol embedded in `codegraph_flow`) are
compacted by default to keep responses token-lean. Under `format=medium` the
`detail` level selects which fields appear; under `format=minimize` (default)
`detail` is ignored — see [Response formats](#response-formats-binance-style-minimal).

- **Session-wide default** is set at bind time: `codegraph_init {"path": ...,
  "detail": "minimal"}` (or re-run `codegraph_init` to change it later).
- **Per-call override** — any list tool accepts a `detail` arg that wins over
  the session default for that one call.

Levels:
- `minimal` — `{id, name, kind, file, line}`. Fewest tokens; best for
  scanning long lists.
- `medium` (default) — adds `signature` (the declaration line). Enough for
  most reasoning.
- `verbose` — the full `Symbol` (doc comments, annotations, scope, type_ref,
  end_line, language). Use only when you actually need those fields;
  `codegraph_symbol {"id": ...}` returns the full symbol for a single target.

`file` paths in responses are **relative to the workspace root** (the `root`
returned by `codegraph_init`). To keep context lean, prefer smaller `limit`
values and `id`-based lookups over re-running broad searches.

## Response formats (Binance-style minimal)

Every response is minimal by default. A `format` knob selects between two
styles — set at server startup (`codegraph serve --mcp --format=...`, default
`minimize`), per session (`codegraph_init {"format": ...}`), or per call
(`"format": ...` arg on any tool, which wins over both):

- **`minimize`** (default) — symbol items are **positional arrays** with a
  fixed, documented order (see the schema below). No keys, no per-item JSON
  overhead — this is the "remove the key, keep only the value" style.
- **`medium`** — objects keep their keys; fields whose value is the default
  (`null`, `false`, `""`, `[]`, `{}`, and numeric `0` for the sentinels
  `scope_id` / `type_ref` / `end_line`) are **omitted entirely**. Counts and
  totals (`total`, `limit`, `offset`, `symbols`, `files`, ...) always stay,
  even when `0`, so summary responses stay readable.

The omission rule applies to **every object in both formats** — wrapper
metadata such as `resume: null`, `has_more: false`, `truncated: false`,
`deleted: false` disappears when it holds the default value. **Absent means
default.** Arrays never omit positions.

### Symbol array schema (`format=minimize`)

Each symbol is a fixed 14-element array. The order is part of the contract —
never reorder or truncate it:

| # | field | type | absent = |
|---|-------|------|----------|
| 0 | `id` | number | — |
| 1 | `name` | string | — |
| 2 | `kind` | string (`function`, `method`, `class`, …) | — |
| 3 | `scope` | string (`global`, `object_field`, `local`, `parameter`) | — |
| 4 | `scope_id` | number | `0` = global |
| 5 | `type_ref` | number | `0` = none |
| 6 | `type_name` | string \| `null` | `null` = none |
| 7 | `file` | string | relative to workspace root |
| 8 | `line` | number | — |
| 9 | `end_line` | number | `0` = not recorded |
| 10 | `signature` | string \| `null` | `null` = none |
| 11 | `doc` | string \| `null` | `null` = none |
| 12 | `annotations` | array | `[]` = none |
| 13 | `language` | string | — |

`format=minimize` **ignores** `detail` — the schema is always these 14 fields.
Use `format=medium` (optionally with `detail=verbose`) when you want a lean
projection or a fully self-describing object instead.

### Example

`codegraph_search_symbol {"query": "greet"}` (minimize, default):

```json
{
  "results": [
    [100, "greet", "function", "global", 0, 0, null, "app.py", 1, 2,
     "def greet(name: str) -> str:", null, [], "python"]
  ],
  "total": 1,
  "limit": 20,
  "offset": 0
}
```

`codegraph_search_symbol {"query": "greet", "format": "medium"}`:

```json
{
  "results": [
    { "id": 100, "name": "greet", "kind": "function",
      "file": "app.py", "line": 1, "signature": "def greet(name: str) -> str:" }
  ],
  "total": 1,
  "limit": 20,
  "offset": 0
}
```

## Symbols are numbers

Symbols are identified by numeric `id` (global registry, ≥ 100). Call-chain
patterns in `codegraph_search_flow` mix marker names (`LOOP`, `IF_TRUE`,
`IF_FALSE`, `BRANCH_END`, `RETURN`, `LOOP_BACK`, `SWITCH_CASE`, `SWITCH_END`,
`BREAK`, `CONTINUE`, `THROW`), symbol ids, and symbol names.

## Behavior sandbox — `codegraph_sandbox`

Compiles an entry function (plus its in-flow callees) to machine code and runs
it against **Rhai mocks**, returning the observed call trace. Use it to
simulate "what does this flow actually do" before touching code.

Arguments:
- `node` (or `name`): the entry function symbol id or name.
- `args`: array of `i64` entry arguments (default `[]`).
- `mocks`: object mapping callee name → Rhai source. The source is either a
  mock body (`77` → becomes `fn <name>(args) { 77 }`) or a full
  `fn <name>(args) { … }` script. Inline mocks **win over** mocks loaded from
  `mock_dirs` in `.codegraph/config.toml`. Mock contract: `args` is a single
  array of `i64`.
- `branch_policy`: optional `"if_true"` / `"if_false"` condition resolution
  override (defaults to `.codegraph/config.toml`).
- `loop_cap`: optional integer loop-iteration cap.

The response reports `return`, the mocked calls in order (`mocks`), condition
decisions (`conds`), and any callee that ran without a mock (`missing_mocks`) —
mock those next. `.codegraph/config.toml` `[sandbox]` sets defaults
(`mock_dirs`, `branch_policy`, `loop_cap`); the per-call arguments override
them.

**Link-time mock check:** before compiling, the sandbox verifies that every
callee the flow will dispatch to a mock has one configured (file `mock_dirs` or
a `mocks` override). Any unconfigured callee fails the call with
`link failed: no mock configured for callee(s): …` listing the exact functions
to mock — supply them in `mocks` (or a `*.rhai` file) and call again.

## Diff draft — `codegraph_diff`

Analyzes a unified diff (MR diff, `.patch` file content, or `git diff` output)
against the current index and returns a **DRAFT** of how the graph would
change — it does NOT mutate the index. Use it to review an MR's logic impact
before merging: which symbols are touched, which flows carry call sites on the
changed lines, and who (transitively) calls the touched functions.

Arguments:
- `diff`: the unified diff text. Supports multi-file diffs, added/removed/
  renamed files, and `\ No newline at end of file`.

Response shape (default-valued fields omitted per the omission rule):
```json
{
  "draft": true,
  "summary": {
    "files_in_diff": 2, "files_matched": 2, "symbols_affected": 1,
    "flows_affected": 1
  },
  "files": [{
    "path": "src/foo.rs", "matched": true,
    "matched_path": "/abs/workspace/src/foo.rs",
    "added_lines": 3, "removed_lines": 2,
    "symbols": [{ "symbol": { "id": 141, "name": "foo", "file": "src/foo.rs", "line": 10, "end_line": 25 }, "impact": "modified" }],
    "flows": [{
      "flow": { "id": 141, "name": "foo", "file": "src/foo.rs", "line": 10 },
      "affected_calls": [{ "position": 3, "callee": "bar", "to_id": 155, "line": 12, "markers": ["IF_TRUE"] }],
      "marker_window": ["IF_TRUE", "BRANCH_END"],
      "called_by": [{ "id": 100, "name": "main", "file": "src/main.rs" }]
    }]
  }]
}
```

Key points:
- Line numbers come from the **new** (b-) side of each hunk, which is what the
  current index reflects (working tree = "after the MR").
- `impact: "removed"` means the whole file was deleted; `"modified"` means at
  least one line inside the symbol's span changed.
- `affected_calls` lists the flow's call sites sitting on changed lines;
  `markers` is the guard-marker run directly before each call site (e.g. the
  `IF_TRUE`/`LOOP` surrounding it), and `marker_window` is the deduped marker
  span of the whole affected region.
- A file that doesn't match anything in the index lands in
  `summary.unmatched_files` (never indexed) or `summary.new_files` (added file
  with no removed lines). Both keys are **omitted when empty** (`[]`), like
  `deleted: false` and any other default value.

## Diff simulation — `codegraph_diff_simulate`

Chains `codegraph_diff` with the sandbox: for the functions a diff touches, it
runs the entry flow TWICE — on the current index (post-MR) and on a temporary
index rebuilt from a git ref — then compares the traces.

Arguments (besides `diff`):
- `entry`: function name to simulate (default: first function affected by the
  diff).
- `base_ref`: git ref for the BEFORE state (default `HEAD`; the pre-MR tree is
  materialized with `git archive`, so the workspace must be a git repo).
- `args`, `mocks`, `branch_policy`, `loop_cap`: same contract as
  `codegraph_sandbox`.

Response shape (default-valued fields omitted):
```json
{
  "draft": true, "entry": "compute", "base_ref": "HEAD",
  "affected_functions": ["compute", "cap"],
  "before": { "present": true, "return": 50, "sequence": ["if:1", "call:fetch"] },
  "after":  { "present": true, "return": 6,  "sequence": ["if:1", "call:fetch", "call:extra"] },
  "delta": { "sequence_added": ["call:extra"] }
}
```

What the trace captures (and what it doesn't): the sandbox follows flow
**structure** — mock call order, branch presence, loop iterations. Branch
decisions follow `branch_policy` (if_true/if_false; the guard text is NOT
evaluated), loops run up to `loop_cap`, and **numeric arithmetic on values is
not modeled**. So the reliable signal is `delta.sequence_added/removed` — e.g.
an MR that adds/removes a call, a branch, or switches a callee shows up as a
sequence delta; an MR that only changes an arithmetic expression does not.
A function that doesn't exist in `base_ref` (new in the MR) reports `before`
**without** a `present` field (absent = not present; only `reason` remains). A
callee without a mock reports
`link_error: no mock configured for callee(s): …` (compile aborts before
running — supply it in `mocks` and retry). `missing_mocks` and empty
`sequence_removed` are omitted when empty.

## Origin/ref simulation — `codegraph_origin_simulate`

The standalone "before" half of `codegraph_diff_simulate`, WITHOUT a diff: run
the sandbox on an entry flow at a git ref (default `HEAD`, e.g. `origin/main`)
and on the current working tree, then compare the traces. Use it to see whether
your local uncommitted edits change a flow's behavior, or to inspect what a flow
does on a specific branch/commit before you touch anything.

Arguments:
- `entry` (required): function name — resolved by NAME in each index (symbol ids
  differ between the ref tree and the working tree).
- `ref`: git ref for the ORIGIN state (default `HEAD`; materialized with
  `git archive`, so the workspace must be a git repo).
- `args`, `mocks`, `branch_policy`, `loop_cap`: same contract as
  `codegraph_sandbox`.

Response shape (default-valued fields omitted):
```json
{
  "draft": true, "entry": "compute", "ref": "origin/main",
  "origin":       { "present": true, "return": 50, "sequence": ["if:1", "call:fetch"] },
  "working_tree": { "present": true, "return": 6,  "sequence": ["if:1", "call:fetch", "call:extra"] },
  "delta": { "sequence_added": ["call:extra"] }
}
```

Trace semantics and limitations are identical to `codegraph_diff_simulate`
above (structure-based, not arithmetic).
