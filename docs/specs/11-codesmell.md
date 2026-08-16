# Spec 11 — CodeSmell (team-convention linter)

**Status**: ✅ done (MVP) — implemented in `crates/codesmell`
(lib engine + CLI binary). User guide: [docs/codesmell.md](../codesmell.md).

## Goal

A policy engine that answers *"does this code look and behave like code this
team would write?"* — run like a linter. LLM agents read the conventions pack
(`codesmell guide`) before writing code and fix every reported violation
(`codesmell check`) afterwards; each violation carries a `fix_hint` so the
agent repairs instead of guessing.

## Relationship to CodeGraph

CodeSmell never re-implements code analysis. Each run parses the repository
fresh into a `GraphIndex::in_memory()` via `Orchestrator::parse_project` —
CodeGraph stays the understanding layer; only the persistent storage layer is
dropped (no `.codegraph` index required, no staleness).

## Crate layout

- `policy.rs` — `.codesmell/policy.toml` model (walked up from cwd), severity
  `info | warning | required | blocking` with per-rule overrides, and
  `[[override]]` blocks scoped by path globs (file → directory → repository
  resolution).
- `index.rs` — `build_index(root)`: parse project → in-memory `GraphIndex`.
- `engine.rs` — `CheckScope` (`All` / `Paths` / `Diff`), candidate collection
  (functions + methods, narrowed by scope), `evaluate` → severity-sorted
  `CheckReport { violations, summary }`.
- `rules.rs` — the rule set (below).
- `guide.rs` — conventions-pack rendering + starter policy template +
  AGENTS.md snippet.
- `main.rs` — CLI: `check [paths] [--diff <file|->] [--format human|json]
  [--fail-on …]`, `guide [path]`, `init`, `policy`. Human output is
  rustc-style (`error[rule]: …` / `  --> file:line` / `  hint: …`); exit 1
  when violations ≥ the `--fail-on` threshold (default `required`).

## Rules (MVP)

| Rule id | Checks | Default severity |
|---|---|---|
| `style.function.max_lines` | Function LOC from `Symbol.line..=end_line` | warning |
| `style.function.max_parameters` | Parameter count parsed from the signature (depth-aware commas, `self` excluded) | warning |
| `style.function.max_nesting` | Nesting depth heuristic from chain markers | warning |
| `style.naming` | Name matches a glob per `SymbolKind`, optionally gated on `signature_contains` (e.g. async methods → `*Async`) | warning |
| `architecture.boundary` | Resolved call-graph edges across denied layer boundaries (layers = path globs) | blocking |
| `testing.missing_test` | Business logic (layer or min-size selectors) with no reference from `test_paths` files — via the call-name index | required |

`--diff` scopes evaluation to symbols overlapping diff hunks
(`parse_unified_diff`, hunk new-line ranges) — change-aware validation.

## Tests

Unit tests for policy merge/severity/param-count/nesting; integration tests
over fixture repos (`tests/fixtures/rustshop` expecting every rule category,
`cleanshop` expecting zero violations, diff-scope narrowing).

## Fast-follow (not built)

Convention discovery (statistics → candidate policies with confidence),
coverage threshold enforcement, policy history/evolution, runtime/engineering
policies (timeouts/retries via effect classification), stored-index reuse as
a performance optimization.
