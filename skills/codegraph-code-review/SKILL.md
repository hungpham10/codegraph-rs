---
name: codegraph-code-review
description: Review a diff, MR, or uncommitted changes with CodeGraph — use when asked to review code, assess a patch's blast radius, or explain what a change touches.
---

# Code Review with CodeGraph

Use CodeGraph to turn a raw diff into a structural review: which symbols changed, who depends on them, and which control-flow paths the change lands in. The index is never mutated — all review tools are read-only drafts.

## Review workflow

```
1. Get the diff:  git diff (uncommitted) or git diff base...head (MR/PR)
2. codegraph_graphcode_diff {diff: "<unified diff text>"}     # draft impact report
3. For each touched symbol: codegraph_impact {node}           # who transitively depends on it
4. codegraph_flow {node: <changed function>}                  # branches/guards around the change
5. codegraph_mermaid {node, kind: "impact"|"callers"}         # render a diagram for the review comment
6. Report findings, citing file:line from the diff report
```

### What `codegraph_graphcode_diff` reports

- **Touched symbols** — functions/methods/classes whose lines overlap the diff.
- **Flows containing changed call sites** — the control-flow window (IF_TRUE, LOOP, BRANCH_END…) around each affected call, so you can see which branch or loop a change lives in.
- **Which flows call the touched functions** — the caller-side blast radius.

Pass the full multi-file diff; added/removed/renamed files and `\ No newline at end of file` are handled.

## Guidelines

- **Re-index first if the working tree changed since the last index.** `codegraph_graphcode_diff` compares the diff against the *indexed* snapshot; stale index → stale touch lists. Run `codegraph_index {}` when unsure.
- For every touched function that is exported/public or has callers outside its module, run `codegraph_impact` and mention the top callers in the review.
- Use `codegraph_mermaid` with `kind: "impact"` for wide blast radius, `"callers"` for upstream only, `"flow"` to walk a single function's logic (guards, effects, marker positions).
- When the change alters behavior, follow up with `codegraph_graphcode_diff_simulate` (see codegraph-security-review skill) to compare traces before/after.
- Review checklist, in order:
  1. Correctness of the diff itself (read the files).
  2. Hidden dependents (`codegraph_impact`) the author may not have considered.
  3. Control-flow placement: does the change run on every loop iteration / only in the error branch?
  4. Contract-sensitive edits: marker ids and serialized types must not be renumbered (check the repo's conventions doc if present).
- Cite findings as `file:line` from the diff and the touched-symbol list, not from memory.

### Example calls

```json
codegraph_graphcode_diff {"diff": "diff --git a/src/foo.rs ...\n"}
codegraph_impact         {"node": 1234, "max_depth": 3, "detail": "medium"}
codegraph_mermaid        {"node": 1234, "kind": "impact", "depth": 2}
```

## Effects & markers in review

Flow output annotates each call site with an **effect** (semantic class: `sql_query`, `http_call`, `panic`, …) and call chains carry **markers** (`try_start`, `throw`, `loop`, `return`, …). Classification comes from pattern rules — defaults plus project overrides in `.codegraph/config.toml` under one `[[effect_rules]]` table (legacy `[[flow_rules]]` is gone), editable at runtime via `codegraph_config_effects`. Rule changes re-classify call records only on the next `codegraph_index`.

Review implications:
- If a diff adds a call to an external system (DB, HTTP, exec), check the diff report's flow window for the **expected effect** on that call site. A missing or wrong effect usually means the classifier needs a new `[[effect_rules]]` entry (matcher `exact`/`prefix` preferred over `contains`) — not a code change.
- If a diff edits control flow, check marker placement: markers must not be inserted between `CALL_ENTER` and `CALL_EXIT`, and existing marker ids must never be renumbered (serialized contract — `crates/codegraph-core/src/semgraph.rs`).
- Flag rule edits in the diff that change effect semantics of existing call patterns; those silently alter `codegraph_search_flow` results downstream.

Note: `codegraph_graphcode_diff` returns a **draft** — it does not apply anything. Combine with reading the actual files before approving a change.
