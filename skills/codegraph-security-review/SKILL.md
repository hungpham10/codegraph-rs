---
name: codegraph-security-review
description: Security-focused analysis with CodeGraph — use when hunting unvalidated paths, panic/error paths, or simulating how a change alters runtime behavior in a sandbox.
---

# Security Review with CodeGraph

Use CodeGraph's marker search and sandbox simulation to find dangerous control-flow paths and prove (in a mocked sandbox) how a function behaves on different branches — without executing the real program.

## 1. Hunt patterns with `codegraph_search_flow`

Effects (semantic classes like `sql_query`, `http_call`, `panic`) and markers (`throw`, `try_start`, `loop`, …) are how call sites get classified — see the "Effects & markers" section in the codegraph-development-workflow skill. Rules are customized via `[[effect_rules]]` in `.codegraph/config.toml` or the `codegraph_config_effects` MCP tool (changes require a re-index to re-classify call records). Typical pattern uses: `panic` for crash paths, custom effects you declared (e.g. `sql_query`) as tokens in a pattern.

Pattern = comma-separated tokens: numeric symbol ids, marker names, or symbol names.

| Goal | Pattern |
|---|---|
| Error/panic paths without a nearby catch | `THROW` (then inspect flows for missing `CATCH`) |
| Exception handling structure | `THROW,CATCH` |
| Unbounded-looking loops | `LOOP` (review `loop_cap` and exit conditions) |
| Entry points into a subsystem | `CALL_ENTER` / specific callee names |
| Flows containing a known-dangerous call | `my_sql_exec,THROW` or the call name alone |

Use `codegraph_references {query: "dangerous_lib_call"}` to find every call site of a library function (includes unresolved external calls), then walk each site with `codegraph_flow` to check the guards above it.

For a dangerous **target** function, enumerate every path that can reach it with `codegraph_trace_full_flow {target_id}` — it returns all caller→target paths with call/return/throw/branch info. Defaults: `max_depth: 5` (caller hops), `max_paths: 50`, `max_loop_iter: 1` (loop body runs once — raise only if you must audit repeated iterations). Unvalidated-input review = for each returned path, check where the guard (if any) sits relative to the entry.

## 2. Simulate with `codegraph_graphcode_sandbox`

Run a function's compiled flow with mocks for callees:

```json
codegraph_graphcode_sandbox {
  "node": 1234,
  "args": [1, 2, 3],
  "mocks": {"read_config": "fn(args) { 0 }"},
  "branch_policy": "if_true",
  "loop_cap": 10
}
```

- `mocks` maps callee names to Rhai `fn` bodies — every callee reachable from the entry must have a mock or the call fails with `missing_mocks`; the response lists any callees that ran unmocked.
- `branch_policy: "if_true" | "if_false"` forces branch decisions (guards' *text* is not evaluated); sweep both to cover both sides.
- `loop_cap` bounds iterations — raise it only when auditing a loop body.
- Output: ordered mock invocations, branch decisions, entry return value. Anything that runs without a mock appears in `missing_mocks`.

## 3. Compare behavior across a change

- `codegraph_graphcode_diff_simulate {diff, entry, args, mocks}` — run the sandbox on the current index AND a temporary index built from `base_ref` (default HEAD = pre-change), then compare traces (mock call order, branch decisions). Requires a git repo.
- `codegraph_graphcode_origin_simulate {entry, ref: "origin/main"}` — same comparison without a diff (working tree vs a ref).

Use these to demonstrate: a guard now skipped, a call now unreachable, a new panic path, or changed call ordering.

## Review checklist

1. Enumerate `THROW` flows in changed code; verify each has a catch or documented escalation.
2. For each external input, walk `codegraph_flow` upward from the consuming call — is there a guard before it?
3. Simulate both branch policies on handlers; check mock-call order for resource leaks (e.g., lock taken, never released on the throw path).
4. `diff_simulate` every behavioral change; report trace deltas as evidence.
5. Contracts: marker ids and serialized types must not be renumbered; markers must not be inserted between `CALL_ENTER` and `CALL_EXIT`.

All simulate tools are **read-only** — the index is never mutated, no code is executed for real.
