# Spec 01 — Workspace bootstrap

**Status**: ✅ done — describes the shipped workspace.

## Goal

A Cargo workspace holding every CodeGraph component, with shared dependency
versions and release profiles centralized.

## Workspace layout (11 crates)

```
crates/
  codegraph-core/       Error + semgraph model (Symbol, SymbolKind, chains, markers, EffectType)
  codegraph-extract/    tree-sitter extractors (14 langs, feature-gated) + walker + Orchestrator
  codegraph-graph/      GraphIndex: registry + chain/name engines + pluggable storage + embeddings
  codegraph-context/    Markdown context composition (symbol + callers + callees + source)
  codegraph-api/        GraphApi — async query facade over SharedGraphIndex
  codegraph-sboxes/     Behavior sandbox: Cranelift JIT + Rhai mock runtime
  codegraph-mcp/        MCP server (rmcp SDK, stdio + Streamable HTTP), 27 tools
  codegraph-bench/      Criterion benches (search, storage, pipeline) + CodSpeed
  codegraph-installer/  Multi-agent client installer (Claude Code, Cursor, Codex, opencode)
  codegraph/            CLI binary (init/deinit/embed/serve) + file watcher
  codesmell/            Team-convention linter consuming CodeGraph facts (lib + CLI)
```

## Conventions

- Root `Cargo.toml`: `resolver = 2`, `[workspace.package]` (version, edition
  2021, license, repository), `[workspace.dependencies]` centralizing serde,
  tree-sitter + grammars, clap, tokio, notify, ignore, rayon, camino, axum,
  rmcp, globset, etc. Crates reference them via `version.workspace = true` /
  `{ workspace = true }`.
- Profiles: `release` = `lto="fat"`, `codegen-units=1`, `strip="symbols"`,
  `panic="abort"`; `release-small` inherits with `opt-level="z"`.
- `rust-toolchain.toml`: stable + rustfmt + clippy. MSRV 1.80 workspace-wide;
  `codegraph-graph` overrides to edition 2024 (needs ≥ 1.85).
- `.gitignore` keeps `/target` and `.codegraph/` out of VCS.

## Validation

CI (`ci.yml`) runs `cargo clippy --workspace --all-targets -- -D warnings`
(with `RUSTFLAGS: -D warnings`), `cargo test --workspace` with coverage, and a
slim no-default-features build check.

## Historical note

The original plan had 9 crates including `codegraph-db` and
`codegraph-resolve`; both were folded away — storage moved into
`codegraph-graph` behind a `Storage` trait (spec 03), and resolution became an
ingest phase of `GraphIndex` (spec 05). `codegraph-api`, `codegraph-sboxes`
and `codesmell` (spec 11) were added later.
