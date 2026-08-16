# Spec 04 — Extraction (tree-sitter, declarative LangSpec)

**Status**: ✅ done — implemented in `crates/codegraph-extract`
(`orchestrator.rs`, `walker.rs`, `config.rs`, `languages/*`).

## Goal

Parse a workspace in parallel and emit per-file `ParseResult`s
(symbols with local ids, chains, call records) for `GraphIndex::ingest`.

## Architecture

```
Orchestrator::with_registry()
  ├── walker::walk(root, parsers, config)     ignore-crate walk → FileMatch { path, parser }
  ├── parse_files (rayon par_iter)            thread-local EffectClassifier installed per job
  │      └── parse_one: fs::read (< 4 MiB, UTF-8) → parser.parse_file
  └── index_all(root, &mut GraphIndex)        parse + ingest (full re-index)
```

`ParseResult { path, language, bytes, lines, symbols, chains, calls }` — all
ids are **local per file** (start at `SYMBOL_BASE`); `ingest` remaps them to
global ids. Chain position `0` is a placeholder for an unresolved callee.

## Language support — 14 languages, one declarative `LangSpec`

`typescript` (ts/mts/cts + tsx), `javascript` (js/jsx/mjs/cjs), `python`,
`rust`, `go`, `java`, `c`, `cpp` (cpp/cc/cxx/hpp/hh/hxx + `.h` routing),
`csharp`, `ruby`, `php`, `scala`, `swift`, `lua`. Each is a feature flag
(`lang-*`, default `all-langs`) registering a parser in `registry()`.

A `LangSpec` (`languages/common.rs`) declares, per language:

- `decls` — (node kind → SymbolKind) mapping for declarations
- `func_kinds` / `class_kinds` / `param_kinds` / `annotation_kinds`
- `calls` — `CallRule`s with name/target extraction hooks
- marker rules — `if_kinds`, `loop_kinds`, `switch_*`, `return/break/
  continue/throw/try/except/finally_kinds`

`run_spec` walks the tree in two passes: a **symbol pass** (scope stack,
Method reclassification, in-file type resolution) and a **chain pass**
(markers + placeholder-0 call sites + `CallRecord`s). Annotations are
extracted for Java (`annotation`, `marker_annotation`) and C#/PHP/Swift
(`attribute`).

`.h` files route between C and C++ via `[languages] headers`
(auto/c/cpp — project hint then content sniffing, see
[configuration.md](../configuration.md)).

## Effects

`EffectClassifier` (thread-local, installed per rayon job) maps call names to
`EffectType` — config `[[effect_rules]]` first, then the built-in default
table. Schema and defaults: [configuration.md](../configuration.md).

## File walking

`ignore::WalkBuilder`: hidden files skipped; `.gitignore` +
`.git/info/exclude` (incl. parents) honored; `.codegraphignore` as a custom
ignore layer. Post-filter: known extensions only; files ≥ 4 MiB or non-UTF-8
skipped (counted in `ExtractStats.skipped`).

## Project config

`ExtractConfig::load(root)` reads `.codegraph/config.toml` (missing/invalid →
defaults): header language, effect classifier, storage settings.
`init_project` creates `.codegraph/` idempotently (`.gitignore` = `*`,
`version`, `config.toml` only if absent).

## Deviations from the original spec

- 14 languages (kotlin grammar incompatible with tree-sitter 0.25; Svelte/
  Vue/Liquid text extractors were not ported).
- No `.scm` query files — extraction is a declarative `LangSpec` + generic
  walker, not per-language queries.
- No sha256/incremental sync — always full re-index (spec 09).
- Emits symbols + chains + CallRecords, not Node/Edge rows.
