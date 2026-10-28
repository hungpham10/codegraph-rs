# Sandbox & simulation (with mocking)

The behavior sandbox answers **"what does this flow actually do?"** before you
touch code: it compiles an entry function plus its in-flow callees to native
machine code (Cranelift JIT) and runs them against **Rhai mocks**, returning
the observed execution trace — mock call order, branch decisions, loop
iterations.

What the trace captures — and what it doesn't: the sandbox follows flow
**structure**. Branch decisions come from `branch_policy` (the guard *text* is
never evaluated), loops run up to `loop_cap`, and **numeric arithmetic on
values is not modeled**. The reliable signal is the call/branch **sequence**:
an MR that adds/removes a call, a branch, or switches a callee shows up as a
sequence delta; a pure arithmetic change does not.

## Configuration — `.codegraph/config.toml`

```toml
[sandbox]
mock_dirs = ["sandbox/mocks"]   # dirs (relative to workspace root) with *.rhai mocks
loop_cap = 10                   # max loop iterations per condition (termination guarantee)
branch_policy = "if_true"       # "if_true" (default) | "if_false" — anything else is an error
```

- Missing `[sandbox]` / config file → these defaults.
- Per-call arguments (`branch_policy`, `loop_cap`, `mocks`) override the
  config for that one run.
- `[[effect_rules]]` at the top level of the same file is shared with the
  extractor (schema in [configuration.md](configuration.md)).

## The mock contract (Rhai)

A mock is a Rhai function named after the callee, taking **one array of i64**
and returning an i64 (abstract value):

```rhai
// sandbox/mocks/order.rhai
fn get_stock(args) { 100 }
fn insert_order(args) { args[0] * 2 }      // bodies can read args
fn send_email(args) { 0 }
```

- Every `*.rhai` file under each configured `mock_dirs` entry is loaded and
  merged into one mock library.
- **Inline mocks** (the `mocks` tool argument) map callee name → source and
  deterministically **replace** file mocks of the same name:
  - body-only source (`"77"`, `"args[0] * 10"`) is auto-wrapped as
    `fn <name>(args) { <source> }`;
  - a full `fn <name>(args) { … }` script is used as-is.
- **Link-time fail-fast**: before compiling, the sandbox verifies that every
  callee the flow will dispatch to a mock has one configured (file or inline).
  Any unconfigured callee aborts with
  `link failed — no mock configured for callee(s): a, b` — supply exactly
  those in `mocks` (or a `*.rhai` file) and call again.
- A mock that is missing only at *run* time records the miss and returns `0`;
  misses surface as `missing_mocks` in the result.

## Run semantics

- `branch_policy`: `"if_true"` takes the then-branch of every `if`,
  `"if_false"` the else-branch. Guard text is never read.
- Loops: a per-condition counter stays true while `n <= loop_cap`.
- Switch: the first case is taken once, then false.
- Output trace: ordered mock invocations (`callee`, args, result), condition
  decisions (`if`/`loop`/`switch`, index, result), and a rendered
  `sequence` (`"if:1"`, `"loop:0"`, `"call:<name>"`, …).

## MCP tools

All three share `args` (i64 array of entry arguments), `mocks`, `branch_policy`,
`loop_cap`.

### `codegraph_sandbox`

Run one entry flow on the current index.

- `node` (symbol id) or `name` (substring → first Function/Method match).
- Returns `entry`, `entry_id`, `group` (entry + every resolvable chain
  callee), `args`, `return`, `mocks` (in order), `conds`, `missing_mocks`,
  `sequence`.

### `codegraph_diff`

Not a sandbox tool, but the companion that scopes them: parses a unified diff
(MR / patch / `git diff`) and returns a read-only **draft** of the graph
impact — touched symbols, flows carrying call sites on changed lines, marker
windows, and transitive callers. Line numbers use the **new** (b-) side of
each hunk. See [codegraph.md](codegraph.md#diff-draft--codegraph_diff) for the
response shape.

### `codegraph_diff_simulate`

For the functions a diff touches, run the entry flow **twice** — on the
current index (post-MR) and on a temporary index rebuilt from `base_ref`
(default `HEAD`, materialized via `git archive` — the workspace must be a git
repo) — then compare the traces.

- Args besides `diff`: `entry` (function name; default: first function
  affected by the diff), `base_ref`.
- Response: `{draft, entry, base_ref, affected_functions, before, after,
  delta: {sequence_added, sequence_removed}}`. A function missing from
  `base_ref` reports `before` without `present`; a callee without a mock
  reports `link_error`.
- Read-only; the temp tree is always removed.

### `codegraph_origin_simulate`

The standalone "before vs now" comparison, without a diff: run the sandbox on
an entry flow at a git `ref` (default `HEAD`, e.g. `origin/main`) and on the
current working tree, then compare. Use it to check whether local uncommitted
edits change a flow's behavior.

- `entry` (required): function name — resolved **by name** in each index
  (symbol ids differ between the ref tree and the working tree).
- Response: `{draft, entry, ref, origin, working_tree, delta}`.

## Where it lives

- Engine: `crates/codegraph-sboxes` — config (`src/config.rs`), Rhai mock
  library (`src/rhai.rs`), JIT codegen (`src/codegen.rs`), runtime + trace
  (`src/runtime.rs`, `src/trace.rs`).
- Example mocks: `crates/codegraph-sboxes/tests/mocks/*.rhai`; integration
  tests in `crates/codegraph-sboxes/tests/{control_flow,end_to_end}.rs`.
- MCP dispatchers: `crates/codegraph-mcp/src/tools.rs`
  (`dispatch_sandbox`, `dispatch_diff_simulate`, `dispatch_origin_simulate`).

## Related

- Agent-facing guide with worked examples: [docs/codegraph.md](codegraph.md)
- Full config reference: [docs/configuration.md](configuration.md)
