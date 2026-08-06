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
| "Set up / (re)build the index" | `codegraph_init` (idempotent; index=true by default) |
| "Re-index the workspace" | `codegraph_index` |
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

## Trust the results

Codegraph returns AST-derived structural data. Do NOT re-verify with grep —
that's slower, less accurate, and wastes context.

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

Response shape:
```json
{
  "draft": true,
  "summary": {
    "files_in_diff": 2, "files_matched": 2, "symbols_affected": 1,
    "flows_affected": 1, "new_files": [], "unmatched_files": []
  },
  "files": [{
    "path": "src/foo.rs", "matched": true,
    "matched_path": "/abs/workspace/src/foo.rs",
    "added_lines": 3, "removed_lines": 2, "deleted": false,
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
  with no removed lines).

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

Response shape:
```json
{
  "draft": true, "entry": "compute", "base_ref": "HEAD",
  "affected_functions": ["compute", "cap"],
  "before": { "present": true, "return": 50, "sequence": ["if:1", "call:fetch"], "missing_mocks": [] },
  "after":  { "present": true, "return": 6,  "sequence": ["if:1", "call:fetch", "call:extra"], "missing_mocks": [] },
  "delta": { "sequence_added": ["call:extra"], "sequence_removed": [] }
}
```

What the trace captures (and what it doesn't): the sandbox follows flow
**structure** — mock call order, branch presence, loop iterations. Branch
decisions follow `branch_policy` (if_true/if_false; the guard text is NOT
evaluated), loops run up to `loop_cap`, and **numeric arithmetic on values is
not modeled**. So the reliable signal is `delta.sequence_added/removed` — e.g.
an MR that adds/removes a call, a branch, or switches a callee shows up as a
sequence delta; an MR that only changes an arithmetic expression does not.
A function that doesn't exist in `base_ref` (new in the MR) reports
`before.present: false`; a callee without a mock reports
`link_error: no mock configured for callee(s): …` (compile aborts before
running — supply it in `mocks` and retry).

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

Response shape:
```json
{
  "draft": true, "entry": "compute", "ref": "origin/main",
  "origin":       { "present": true, "return": 50, "sequence": ["if:1", "call:fetch"], "missing_mocks": [] },
  "working_tree": { "present": true, "return": 6,  "sequence": ["if:1", "call:fetch", "call:extra"], "missing_mocks": [] },
  "delta": { "sequence_added": ["call:extra"], "sequence_removed": [] }
}
```

Trace semantics and limitations are identical to `codegraph_diff_simulate`
above (structure-based, not arithmetic).
