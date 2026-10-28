# Configuration Reference

Complete reference for `.codegraph/config.toml`.

## File Location

```
.codegraph/
  config.toml      # Main configuration
  .gitignore       # Pre-filled (index never committed)
  version          # Codegraph version that created the directory
```

## Full Configuration Example

```toml
# Language toggles (all 14 enabled by default)
[languages]
rust = true
go = true
python = true
typescript = true
javascript = true
java = true
c = true
cpp = true
csharp = true
ruby = true
php = true
scala = true
swift = true
lua = true
svelte = true
astro = true

# C/C++ header handling
# headers = "auto"   # "auto" (default), "c", or "cpp"

# Walker filters (gitignore syntax)
[walker]
include = ["**/*"]
exclude = [
  ".git/**",
  ".codegraph/**",
  "target/**",
  "node_modules/**",
  "*.min.js",
  "*.lock"
]

# Storage backend
[storage]
type = "sqlite"  # "sqlite" | "lmdb" | "redis" | "memory" | "postgres" | "mysql"
# DSN override (optional; defaults derived from type)
# dsn = "sqlite:///path/to/db.sqlite"
# dsn = "lmdb:///path/to/db.lmdb"
# dsn = "redis://localhost:6379"
# For postgres/mysql: use dsns (shard list) + repo_id — see Storage Backends doc

# Semantic search (vector KNN) — OFF by default
[embedding]
# backend = "fastembed"              # "hashing"/unset = off
# model = "bge-small-en-v1.5"         # 384-dim, default
# cache_dir = "~/.cache/codegraph/embeddings"
# SQLite-only: HNSW ANN via sqlite-vss
# vss_extension = "~/.cache/codegraph/embeddings/vss"
# execution_provider = "coreml"       # macOS hardware acceleration

# Document graph — ingest structured docs (yaml/json/toml/hcl/nginx/markdown)
[docgraph]
# enabled = true          # default: on when `paths` is non-empty
# paths = ["infra/*.tf", "config/**/*.yaml"]   # globs from project root; "path:format" overrides detection
# doc_base = 1000000000   # doc node id base (default 1e9)
# pattern_base = 3000000000  # mined-pattern id base (default 3e9)
# bloom_cap = 64
# aliases = [["instances", "replicas"]]

# Binary graph — symbols extracted from ELF/PE/Mach-O binaries
[bingraph]
# enabled = true
# bin_base = 2000000000   # binary symbol id base (default 2e9)
# [bingraph.storage]      # sqlite only; other backends fall back to memory
# type = "sqlite"

# Binary analysis with radare2
[binary]
# enabled = true       # on by default; skipped with a warning if r2 is not on PATH
# depth = "aaa"        # "aaa" (full) | "fast" (af + aar + aac)
# cfg_markers = true   # build IF/LOOP/SWITCH markers from each function's CFG (pdfj)
# cache = true         # cache analysis by (path, mtime, size)
```

---

## Section Reference

### `[languages]`

Enable/disable individual language extractors. All 14 are enabled by default.

| Key | Language | Tree-sitter Grammar |
|-----|----------|---------------------|
| `rust` | Rust | tree-sitter-rust |
| `go` | Go | tree-sitter-go |
| `python` | Python | tree-sitter-python |
| `typescript` | TypeScript | tree-sitter-typescript |
| `javascript` | JavaScript | tree-sitter-javascript |
| `java` | Java | tree-sitter-java |
| `c` | C | tree-sitter-c |
| `cpp` | C++ | tree-sitter-cpp |
| `csharp` | C# | tree-sitter-c-sharp |
| `ruby` | Ruby | tree-sitter-ruby |
| `php` | PHP | tree-sitter-php |
| `scala` | Scala | tree-sitter-scala |
| `swift` | Swift | tree-sitter-swift |
| `lua` | Lua | tree-sitter-lua |
| `svelte` | Svelte | tree-sitter-svelte-next (script re-parse JS/TS) |
| `astro` | Astro | tree-sitter-astro-next (frontmatter/script re-parse JS/TS) |

**Headers**: The `headers` key controls `.h` file parsing:
- `"auto"` (default): Detect from project — C++ if `.cpp`/`.hpp` present, C if `.c` present, inspect each `.h` for C++ syntax in mixed projects
- `"c"`: Force all `.h` as C
- `"cpp"`: Force all `.h` as C++

### `[walker]`

Controls which files are indexed. Uses `ignore::WalkBuilder` (same syntax as `.gitignore`).

| Key | Type | Description |
|-----|------|-------------|
| `include` | `Vec<String>` | Patterns to include (default `["**/*"]`) |
| `exclude` | `Vec<String>` | Patterns to exclude (default excludes `.git`, `.codegraph`, `target`, `node_modules`, `*.min.js`, `*.lock`) |

**Note**: Walker filters apply *before* language detection. Excluded files are never parsed.

### `[storage]`

Selects the storage backend for the semantic graph.

| `type` | Description | Default DSN | Notes |
|--------|-------------|-------------|-------|
| `sqlite` | SQLite WAL mode, single file | `sqlite://<root>/.codegraph/db.sqlite` | Default, recommended for most uses |
| `lmdb` | Memory-mapped KV, directory | `lmdb://<root>/.codegraph/db.lmdb` | Mmap-friendly for large indexes |
| `redis` | Redis backend | **Required** — no sensible default | Needs running Redis server |
| `memory` | Ephemeral in-process | N/A | Nothing persisted; for testing |
| `postgres` | PostgreSQL, sharded by `repo_id` | **Required** via `dsns` | Multi-tenant; see below |
| `mysql` | MySQL, sharded by `repo_id` | **Required** via `dsns` | Multi-tenant; see below |

**DSN override**: Set `dsn` to override the default for any backend.

**Postgres/MySQL (multi-tenant, sharded)**:
```toml
[storage]
type = "postgres"  # or "mysql"
dsns = [
  "postgres://user:pass@db1:5432/codegraph",
  "postgres://user:pass@db2:5432/codegraph",
]
# repo_id is auto-generated by `codegraph init` and written here
# repo_id = 14028493579208694412
```

- Sharding: `shard = repo_id % len(dsns)`
- Schema **not auto-applied** — run SQL files from `sql/postgres/` or `sql/mysql/` manually before indexing
- See `sql/README.md` for full schema and sharding design

### `[embedding]`

Enables optional semantic search (vector KNN over symbol embeddings).

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `backend` | `String` | unset (off) | `"fastembed"` to enable; `"hashing"` for deterministic fallback |
| `model` | `String` | `"bge-small-en-v1.5"` | ONNX model name (384-dim) |
| `cache_dir` | `String` | `"~/.cache/codegraph/embeddings"` | Global model cache directory |
| `vss_extension` | `String` | unset | SQLite-only: path to sqlite-vss extension for HNSW ANN |
| `execution_provider` | `String` | unset | `"coreml"` for macOS Apple Neural Engine/GPU |

**Behavior**:
- **Off by default** — no embedding model runs unless `backend = "fastembed"`
- Release binary bundles fastembed (ONNX runtime) — no rebuild needed
- With embeddings enabled, `codegraph_search_symbol` gains `match` modes: `"semantic"` (vector KNN) and `"hybrid"` (RRF merge of substring + semantic)
- Vectors persisted with index — restarts reuse without re-embedding
- **Error on load failure** — if model fails to load, opening index errors out (no silent fallback)

**Pre-download model** (for offline indexing):
```bash
codegraph embed --model bge-small-en-v1.5
```
Requires binary built with `--features fastembed`.

### `[docgraph]`

Configures ingest of structured documents into the document graph at `codegraph init`.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | `bool` | on when `paths` non-empty | Turn doc ingest on/off |
| `paths` | `Vec<String>` | `[]` | Globs from project root (`config/**/*.yaml`); a `:format` suffix (`"deploy/README:hcl"`) overrides extension detection |
| `storage` | table | derived from `[storage]` | `{ type, dsn }` override; default dataset is `.codegraph/docs.sqlite` for sqlite |
| `doc_base` | `u64` | `1e9` | ID base for doc nodes |
| `pattern_base` | `u64` | `3e9` | ID base for mined structural patterns |
| `bloom_cap` | `usize` | `64` | Bloom-filter cap |
| `aliases` | `Vec<(String, String)>` | `[]` | Key normalization aliases, e.g. `["instances", "replicas"]` |

### `[bingraph]`

Binary graph dataset — binary symbols live separately from the code index so search/list query the SQL index lazily without rebuilding RAM.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | `bool` | `true` | Toggle binary graph |
| `storage` | table | — | `{ type, dsn }`; **sqlite only** — other backends fall back to in-memory with a warning |
| `bin_base` | `u64` | `2e9` | ID base for binary symbols (kept clear of docs 1e9/3e9 and code IDs) |

### `[binary]`

Controls radare2 analysis of binaries found in the repo (requires `radare2` on `PATH`; skipped with a warning if missing — see `codegraph doctor`).

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enabled` | `bool` | `true` | Analyze binaries during index |
| `depth` | `"aaa"` \| `"fast"` | `"aaa"` | `"aaa"` full analysis; `"fast"` runs `af` + `aar` + `aac` (better for large binaries) |
| `cfg_markers` | `bool` | `true` | Build IF/LOOP/SWITCH markers from each function's CFG (`pdfj`) |
| `cache` | `bool` | `true` | Cache analysis by `(path, mtime, size)` to avoid re-running `aaa` |

---

## `[[effect_rules]]`

Declares per-call semantic behavior (effect) and/or control-flow role (marker).
One table only — `[[flow_rules]]` (the old flow-only table) is ignored with a warning
on load; its rules are merged into `[[effect_rules]]`.

```toml
[[effect_rules]]
call   = { exact = "PG_TRY" }
marker = "try_start"

[[effect_rules]]
call   = { contains = "ereport" }
effect = "panic"
marker = "throw"
label  = "Error"

[[effect_rules]]
call   = { prefix = "db." }
effect = "sql_query"
```

Each rule needs **at least one** of `effect` or `marker`:

| Key | Values | Notes |
|-----|--------|-------|
| `call` | `{ exact = "…" }`, `{ prefix = "…" }`, `{ contains = "…" }` | Matcher schema, shared with effect classification. First match wins in declaration order. |
| `effect` | `sql_query`, `sql_write`, `cache_read`, `cache_write`, `http_call`, `event_emit`, `file_read`, `file_write`, `log`, `panic` | External side effect. `effect = "none"` shadows a built-in default. Omitted = rule is marker-only. |
| `marker` | `try_start`, `try_end`, `catch_start`, `throw`, `if_true`, `if_false`, `branch_end`, `loop`, `loop_back`, `break`, `continue`, `return` | Control-flow marker whitelist. `CALL_ENTER`/`CALL_EXIT` are not allowed (would break the call graph). |
| `label` | string | Exception type / branch label. Valid only with `throw` | `catch_start` | `if_true`. |
| `keep_call` | boolean | Default `true` for `throw` (keeps `CallRecord`), `false` for other markers (macro emits marker only). |

Rules in `[[effect_rules]]` are applied **before** built-in effect defaults
(first match wins), so config can override the default table.

---

## Environment Variable Overrides

| Variable | Effect |
|----------|--------|
| `CODEGRAPH_INSTALL_DIR` | Install script target directory (default: `~/.local/bin`) |
| `CODEGRAPH_PROFILE` / `CODEGRAPH_PROFILE_FILE` | Enable internal profiling: `off`/`0`/`false` disables, `stderr` or a file path selects the sink |
| `CODEGRAPH_BENCH_REPOS_LIST` | File listing repos for criterion benches (codegraph-bench only) |

---

## Related Docs

- [Storage Backends](storage-backends.md) — Deep dive on each backend
- [Semantic Search](semantic-search.md) — Embedding setup, models, sqlite-vss, CoreML
- [Architecture](architecture.md) — How config maps to pipeline
- [README](../README.md) — Quick start
