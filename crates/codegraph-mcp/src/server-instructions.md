# Codegraph — code intelligence over an indexed semantic graph

A SQLite semantic graph of every symbol (function/method/class/…) and its call
chain. Reads are sub-millisecond. Consult it BEFORE editing code.

## Session
One session per process. Bind before querying:
- `codegraph_init {"path":"…"}` — bind root, create `.codegraph/` (idempotent),
  non-blocking, does NOT index (`index` defaults `false`). Then
  `codegraph_index {}` builds/refreshes the index. Re-run with a new `path` to
  re-point. Optional defaults: `"detail":"minimal|medium|verbose"`,
  `"format":"minimize|medium"`.
- `codegraph_deinit {}` — release session (index stays on disk). An unbound
  session refuses all query tools.
A startup `--path` is already bound.

## Answer directly
For "how does X work" / trace / where-is-X, answer directly with 2-3 calls:
`codegraph_context` first, then drill down (`codegraph_symbol`,
`codegraph_callers`/`codegraph_callees`). Don't delegate the lookup to a
file-reading subtask — codegraph IS the index.

## Tool selection
| Intent | Tool |
|---|---|
| symbol by id/name | `codegraph_symbol` |
| find symbols by name (match modes + semantic/hybrid) | `codegraph_search_symbol` |
| what (transitively) calls this? | `codegraph_callers` |
| what does this call directly? | `codegraph_callees` |
| change-impact radius | `codegraph_impact` |
| call chain (markers + callees + sites) | `codegraph_flow` |
| functions whose chain matches a pattern | `codegraph_search_flow` |
| composed context for a symbol/topic | `codegraph_context` |
| who calls library call `foo`? | `codegraph_references` |
| methods/fields of class X | `codegraph_class` |
| list all classes/interfaces/enums | `codegraph_list_types` (`kind`: class\|interface\|enum) |
| params/locals of function X | `codegraph_function_scope` |
| symbols annotated `@X` | `codegraph_search_by_annotation` |
| project dependencies | `codegraph_dependencies` |
| files under a path | `codegraph_files` |
| index health | `codegraph_status` |
| behavior sandbox (Rhai mocks) | `codegraph_sandbox` |
| MR impact (draft) | `codegraph_diff` |
| MR before/after trace compare | `codegraph_diff_simulate` |
| ref vs working-tree trace compare | `codegraph_origin_simulate` |

## Disambiguation
Duplicate names → `ambiguous:true` with a `matches` list. Retry with the
numeric `id` alone. `codegraph_symbol`, `codegraph_class`,
`codegraph_function_scope`, `codegraph_list_types`, and `codegraph_search_symbol`
accept `id`/`name`.

## Large indexes: timeout + resume
Broad searches accept `timeout_ms` (default `20000`; `0` = no limit). When the
budget runs out the tool ERRORS (no partial results) with `"resume":"<id>"` and a
progress count. Retry the SAME call + the SAME args + `"resume":"<id>"` to
continue — nothing re-scans. Resume ids are in-process and invalidated by
re-index or restart; passing one with changed args is rejected.

## Output detail
`detail` (per call, overrides session default): `minimal` = {id,name,kind,file,
line}; `medium` (default) = +signature; `verbose` = full Symbol.
`codegraph_symbol {"id":…}` returns the full symbol for one target. `file` paths
are relative to the workspace root.

## Response format (`minimize` = default)
- `minimize` — symbols are fixed-order positional arrays (schema below); no keys.
  Ignores `detail`.
- `medium` — objects keep keys; default-valued fields (`null`, `false`, `""`,
  `[]`, `{}`, and `0` for `scope_id`/`type_ref`/`end_line`) are omitted. Counts
  (`total`,`limit`,`offset`,…) always stay. **Absent = default.**

Symbol array (`minimize`), 14 fixed fields in order:
`0` id, `1` name, `2` kind, `3` scope, `4` scope_id(0=global), `5` type_ref(0=none),
`6` type_name, `7` file(rel root), `8` line, `9` end_line(0=none), `10` signature,
`11` doc, `12` annotations, `13` language. Never reorder or truncate.

## Behavior sandbox — `codegraph_sandbox`
Compiles an entry function + in-flow callees to machine code; runs against Rhai
mocks; returns the observed trace. Args: `node`/`name` (entry), `args` (`i64[]`),
`mocks` (callee → Rhai body or full `fn`), `branch_policy` (if_true|if_false),
`loop_cap`. Inline mocks win over `[sandbox].mock_dirs`. Before compiling, every
dispatched callee must have a mock or the call fails
`link failed: no mock configured for callee(s): …`. Response: `return`, ordered
`mocks`, condition decisions `conds`, and `missing_mocks` (mock those next).

## Diff draft — `codegraph_diff`
Reads a unified diff (MR / `.patch` / `git diff`) against the current index and
returns a DRAFT of graph changes (does NOT mutate the index). Arg: `diff`.
Reports touched symbols, flows with call sites on changed lines, and who
(transitively) calls them. Line numbers come from the new (b-) side (working tree
= "after the MR"). `impact:"removed"` = whole file deleted; `"modified"` = ≥1
line in the symbol's span changed.

## Diff / Origin simulation
`codegraph_diff_simulate` (needs `diff`): runs the entry flow twice — current
index (post-MR) and a temp index from `base_ref` (default `HEAD`, via
`git archive`) — and compares traces. `codegraph_origin_simulate` is the
standalone before/after of a flow at `ref` (default `HEAD`) vs the working tree.
Args: `entry` (function name), `base_ref`/`ref`, `args`, `mocks`,
`branch_policy`, `loop_cap`. The sandbox follows flow STRUCTURE: mock-call order,
branch presence, loop iterations. `branch_policy` resolves guards (guard text is
NOT evaluated), loops cap at `loop_cap`, and numeric arithmetic is NOT modeled.
The reliable signal is `delta.sequence_added/removed` — a call/branch/callee
change shows up; a pure arithmetic change does not. An unconfigured callee →
`link_error: no mock configured for callee(s): …`.
