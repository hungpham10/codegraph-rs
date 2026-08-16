# CodeSmell — team convention linter

CodeSmell is a team-specific engineering convention and code-quality policy
engine. It runs like a linter (`codesmell check`) and answers one question:

> Does this code look and behave like code that this team would normally write
> and maintain?

It is designed for both developers and LLM coding agents: the agent reads the
conventions pack **before** writing code (`codesmell guide`) and fixes every
reported violation **after** (`codesmell check`). Each violation carries a
`fix_hint`, so the LLM repairs code instead of guessing.

CodeSmell does not re-implement code analysis — it consumes the facts produced
by CodeGraph (symbols, kinds, line spans, call graph, call sites). Each run
parses the repository fresh into an in-memory CodeGraph, so there is no index
to maintain and no staleness: just run it.

```text
Code → CodeGraph (in-memory) → Facts → CodeSmell → Violations + fix hints
```

## Install / build

```bash
cargo build -p codesmell        # binary at target/debug/codesmell
cargo install --path crates/codesmell
```

## Quick start

```bash
codesmell init                  # write .codesmell/policy.toml (commented starter)
codesmell guide                 # print the conventions pack for the LLM / team
codesmell check                 # lint the whole repository
codesmell check src/services    # lint a subtree
git diff | codesmell check --diff -   # lint only changed symbols
codesmell policy                # print the effective resolved policy
```

Suggested `AGENTS.md` / `CLAUDE.md` snippet (also printed by `codesmell init`):

```markdown
## CodeSmell conventions
Run `codesmell guide` before writing or modifying code, and `codesmell check`
(or `codesmell check --diff -`) after, then fix every violation by severity.
```

## Policy file

`.codesmell/policy.toml` (TOML; loaded by walking up from the current
directory). Every section is optional — an empty policy simply checks nothing.

```toml
version = 1

[style.function]
max_lines = 60          # function body length (end_line - line + 1)
max_parameters = 4      # real parameters parsed from the signature (self excluded)
max_nesting = 4         # best-effort depth from control-flow markers

[[style.naming.rule]]
kind = "class"          # SymbolKind: function/method/class/...
pattern = "*Service"    # glob matched against the symbol name
paths = ["src/services/**"]   # optional scoping

[[style.naming.rule]]
kind = "method"
pattern = "*Async"            # async methods must end in Async
signature_contains = "async"  # condition: only applies to async declarations

[[architecture.layer]]
name = "controller"
paths = ["src/controllers/**", "**/*Controller.java"]

[[architecture.layer]]
name = "repository"
paths = ["src/repositories/**", "**/*Repository.java"]

[[architecture.boundary]]
deny = ["controller -> repository"]      # enforced
allow = ["controller -> service"]        # informational in MVP

[testing]
require_tests_for_changed_logic = true
test_paths = ["tests/**", "**/*_test.go", "**/*_test.rs", "**/test_*.py", "**/*Test.java"]
logic_selectors = [{ layers = ["service"] }, { min_lines = 20 }]

[testing.coverage]   # reserved — parsed but not enforced yet
line = 80

[severity]                    # per-rule severity overrides
"style.function.max_lines" = "warning"

[[override]]                  # scoped relaxation (e.g. legacy code)
paths = ["legacy/**"]
[override.style.function]
max_lines = 120
```

## Rules

| Rule id | Category | Checks | Default severity |
|---|---|---|---|
| `style.function.max_lines` | Style | Function length in lines | `warning` |
| `style.function.max_parameters` | Style | Parameter count from the signature (`self` excluded) | `warning` |
| `style.function.max_nesting` | Style | Nesting depth heuristic from flow markers | `warning` |
| `style.naming` | Style | Name matches the configured glob per kind | `warning` |
| `architecture.boundary` | Architecture | Resolved call graph edges across denied layer boundaries | `blocking` |
| `testing.missing_test` | Testing | Business logic with no reference from test paths | `required` |

Severity model (least → most serious): `info`, `warning`, `required`,
`blocking`. Layers are mapped from file-path globs; boundary edges are
evaluated against the resolved call graph (caller file layer → callee file
layer). "Logic" for the testing rule is anything matching a `logic_selectors`
entry (a layer, or a minimum function size).

## Scope resolution

Policies resolve per file, following file → directory → repository order:
`[[override]]` blocks whose `paths` globs match a file are shallow-merged over
the base policy (scalars replace; rules/layers are appended). Use this to
relax rules in legacy areas without weakening them repo-wide.

## CLI reference

```
codesmell check [paths...] [--diff <file|->] [--format human|json] [--fail-on warning|required|blocking]
codesmell guide [path]
codesmell init
codesmell policy
```

- `--format json` emits the full report (`violations[]` with
  `rule/severity/file/line/symbol/message/fix_hint`, plus a `summary`), for
  machine and LLM consumption.
- `--fail-on` sets the severity threshold that makes the process exit `1`
  (default: `required`, i.e. missing tests and boundary violations fail;
  warnings alone do not).
- `--diff` accepts a unified diff (file path or `-` for stdin) and evaluates
  only symbols overlapping the diff hunks — cheap change-aware validation.

Human output is rustc-style:

```
error[architecture.boundary]: `place_order` (controller) calls `save_order` (repository): edge `controller -> repository` is denied
  --> src/controllers/order_controller.rs:6
  hint: route `place_order` through an allowed layer instead of calling `repository` directly
```

## Agent workflow

```text
codesmell guide            (read conventions)
  → LLM writes code
  → codesmell check --diff -
  → fix violations (sorted blocking → info, each with a fix_hint)
  → re-check until clean
```

## Relationship with CodeGraph

CodeGraph understands the software (AST, symbols, call graph, flows);
CodeSmell evaluates those facts against the team's engineering conventions.
CodeSmell builds a fresh in-memory CodeGraph per run — the persistent
`.codegraph` index is not required.

## Not implemented yet (fast-follow)

- Convention discovery (statistics → candidate policies with confidence)
- Coverage threshold enforcement
- Policy history / evolution
- Runtime/engineering policies (timeouts, retries via effect classification)
