# Architecture Overview

This document provides a high‑level overview of CodeGraph's internal structure, its crates, and the data‑flow pipeline from source files to the MCP server.

## Crates

```
crates/
  codegraph-core/       Error + semgraph model (Symbol, SymbolKind, Chain, CallRecord, EffectType, ScopeLevel, markers)
  codegraph-extract/    tree-sitter native + 14 LangSpec declarative extractors + 5 hand‑written
  codegraph-graph/      GraphIndex (semgraph): registry + 2 engines (chain Search<u64> + name Search<u8>) + pluggable storage (SQLite / LMDB / Redis / Postgres / MySQL) + optional embedding vector index
  codegraph-context/    Markdown/JSON context formatter (symbol + callers + callees + source)
  codegraph-api/        GraphApi wrapper on SharedGraphIndex (async query surface)
  codegraph-sboxes/     Behavior sandbox: Cranelift JIT compile of function groups + Rhai mock runtime
  codegraph-mcp/        MCP server on the rmcp SDK (stdio + Streamable HTTP) + 24‑tool dispatch, session‑driven
  codegraph-bench/      Benchmarks (criterion search benches, storage benches, codspeed)
  codegraph/            CLI lifecycle (init/deinit/embed/serve --mcp) + watcher (notify + debounced full re‑index)
```

## Pipeline

```
files → ignore::WalkBuilder → rayon parse pool (tree‑sitter, 14 langs)
           ↓
      ParseResult (symbols local‑id, chains, CallRecords)
           ↓
      GraphIndex.ingest() — full re‑index:
        1. Reset (clear entities, engines)
        2. Register symbols → global IDs + remap scope/type_ref
        3. Remap chains (local→global), keep placeholder 0
        4. Resolve calls: structural hint → exact name → short name → best‑candidate
        5. Build edges + call records + call‑name index
        6. Persist entities + rebuild engines + bump version
           ↓
      GraphApi / SharedGraphIndex.ensure_fresh() (version probe)
           ↓
      MCP server / CLI lifecycle
```