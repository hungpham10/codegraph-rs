# Architecture Overview

This document provides a high‑level overview of CodeGraph's internal structure, its crates, and the data‑flow pipeline from source files to the MCP server.

## Crates

```
crates/
  codegraph-core/       Error + semgraph model (Symbol, SymbolKind, Chain, CallRecord, EffectType, ScopeLevel, markers)
  codegraph-source/     Source abstraction (DiskSource) — file content fed to extractors
  codegraph-extract/    tree-sitter native: 15 LangSpec declarative parsers (16 languages, TSX reuses lang-typescript)
                        + 2 embedded host parsers (Svelte, Astro re-parse their JS/TS blocks) + effect/flow classifiers
  codegraph-graph/      GraphIndex (semgraph): registry + 2 engines (chain Search<u64> + name Search<u8>) + pluggable storage (SQLite / LMDB / Redis / Postgres / MySQL) + optional embedding vector index
  codegraph-binary/     Binary graph: radare2 (r2pipe) extraction of functions/imports/strings + CFG-derived IF/LOOP/SWITCH markers, cached by (path, mtime, size)
  codegraph-context/    Markdown/JSON context formatter (symbol + callers + callees + source)
  codegraph-api/        GraphApi wrapper on SharedGraphIndex (async query surface)
  codegraph-docs/       Document graph: DocParser trait, HCL/YAML/JSON/TOML/Nginx/Markdown parsers, DocumentGraph with DocToken tries + pattern mining
  codegraph-sboxes/     Behavior sandbox: Cranelift JIT compile of function groups + Rhai mock runtime
  codegraph-mcp/        MCP server on the rmcp SDK (stdio + Streamable HTTP) + 42-tool dispatch, session-driven
  codegraph-graphql/    GraphQL HTTP API server (queries + mutations for code + documents + binary graph)
  codegraph-installer/  Self-install/uninstall logic for the CLI (`codegraph install`/`uninstall`)
  codegraph-bench/      Benchmarks (criterion search benches, storage benches, codspeed)
  codegraph/            CLI lifecycle (init/deinit/clean/doctor/embed/serve --mcp) + watcher (notify + debounced full re-index)
```

## Pipeline

```
files → ignore::WalkBuilder → rayon parse pool (tree-sitter, 17 langs)
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

## 📄 Document Formats (codegraph-docs)

CodeGraph-docs supports parsing structured configuration and document files into a unified document graph:

| Format | Extensions | Parser | Status |
|--------|------------|--------|--------|
| YAML | `.yaml`, `.yml` | YamlParser | ✅ Implemented |
| JSON | `.json` | JsonParser | ✅ Implemented |
| TOML | `.toml` | TomlParser | ✅ Implemented |
| **HCL** | `.hcl`, `.tf` | HclParser | ✅ Implemented |
| **Nginx** | `.conf`, `.nginx` | NginxParser | ✅ Implemented |
| **Markdown** | `.md`, `.markdown` | MarkdownParser | ✅ Implemented (heading-nested IR) |

### Document Graph

Parsed documents are stored in a `DocumentGraph` backed by `codegraph-graph`'s `InMemoryStorage` (persistent tries) with `Search<DocToken>` indices for path, type, value, struct, and pattern queries.

**Access points:**
- **MCP**: `codegraph_graphdoc_ingest` / `codegraph_graphdoc_ingest_dir` / `codegraph_graphdoc_remove`, `codegraph_graphdoc_search` / `codegraph_graphdoc_search_value` / `codegraph_graphdoc_search_struct`, `codegraph_graphdoc_hydrate`, `codegraph_graphdoc_list`, `codegraph_graphdoc_stats`, `codegraph_graphdoc_mine_patterns` / `codegraph_graphdoc_list_patterns`
- **GraphQL**: `graphdocList`, `graphdocSearch`, `graphdocSearchValue`, `graphdocHydrate`, `graphdocStats`, `graphdocListPatterns` queries and `graphdocIngest`, `graphdocIngestDir`, `graphdocRemove`, `graphdocMinePatterns` mutations

**Format auto-detection**: `.tf`/`.hcl` → hcl, `.yaml`/`.yml` → yaml, `.json` → json, `.toml` → toml, `.conf`/`.nginx` → nginx, `.md`/`.markdown` → markdown.

### Crate Structure

```
codegraph-docs/
  ├── lib.rs            Re-exports (DocConfig, DocumentGraph, DocToken, DocParser, ...)
  ├── config.rs         DocConfig (id bases, bloom cap)
  ├── graph.rs          DocumentGraph, NodePayload, DocStats
  ├── ir.rs             Document, Node, Kind, Scalar, ByteSpan
  ├── tokenize.rs       DocToken (8-byte structural token)
  └── parsers/
      └── mod.rs        DocParser trait, build_document(), all parsers
                        (HclParser, YamlParser, JsonParser, TomlParser,
                        NginxParser, MarkdownParser), detect_format()
```

## 🔀 Effect rules — call/macro → effect + control-flow marker

### Vấn đề

Nhiều codebase (đặc biệt C — PostgreSQL) dùng **macro/gọi hàm "magic"** để giả
lập control-flow của ngôn ngữ khác khi tree-sitter không phân tích thành node
try/throw riêng:

```
PG_TRY()          → khối try
ereport(ERROR,…)  → throw
PG_CATCH()        → handler catch
PG_END_TRY()      → đóng khối try
```

Các node này không rơi vào `spec.markers.try_kinds/except_kinds` của ngôn ngữ,
nên chain không có marker `TRY_START/CATCH_START/THROW` → interpreter của
`codegraph_trace_full_flow` dừng xử lý throw↔catch, các luồng unwind/catch
không được ghép.

**Giải pháp: `[[effect_rules]]`** — một bảng config thuần duy nhất từ khai báo
của user/LLM, mapping một call-name matcher vào (i) một effect bên ngoài
(sql_query, http_call, panic…) và/hoặc (ii) một control-flow marker có sẵn.
Rule có thể khai báo `effect`, `marker`, hoặc cả hai (≥1 trong hai). Không có
bất kỳ default rule nào được ship: classifier không có config → `classify_flow`
trả `None` cho mọi call → hành vi giống hệt trước khi tính năng tồn tại
(zero regression). Idiom của từng repo khác nhau + rule sai mặc định còn nguy
hơn thiếu, nên khai báo tường minh là lựa chọn cố ý.

### Schema `.codegraph/config.toml`

```toml
[[effect_rules]]
call   = { exact = "PG_TRY" }
marker = "try_start"          # mở khối try (interpreter push catch_stack)

[[effect_rules]]
call   = { exact = "PG_CATCH" }
marker = "catch_start"        # handler — không label → match MỌI exception

[[effect_rules]]
call   = { exact = "PG_END_TRY" }
marker = "try_end"            # pop catch_stack

[[effect_rules]]
call   = { contains = "ereport" }
effect = "panic"              # tác động bên ngoài
marker = "throw"              # throw → interpreter nhảy tới handler khớp

[[effect_rules]]
call   = { exact = "errcode_for_backend" }
marker = "throw"
label  = "Error"              # nhãn exception override (chỉ throw|catch_start|if_true)
```

| Field | Giá trị | Ghi chú |
|---|---|---|
| `call` | `{ prefix = "…" }`, `{ contains = "…" }`, `{ exact = "…" }` | Chia sẻ schema với `[[effect_rules]]` — first-match-wins theo thứ tự khai báo, chính xác từng call-name (không receiver-aware). |
| `effect` | `sql_query`, `sql_write`, `cache_read`, `cache_write`, `http_call`, `event_emit`, `file_read`, `file_write`, `log`, `panic` | Tác động bên ngoài. `effect = "none"` (nếu khai báo) shadow một bảng default. Không khai báo (mặc định) = rule chỉ marker. |
| `marker` | `try_start`, `try_end`, `catch_start`, `throw`, `if_true`, `if_false`, `branch_end`, `loop`, `loop_back`, `break`, `continue`, `return` | Whitelist marker điều khiển. Không cho phép `CALL_ENTER`/`CALL_EXIT` (sẽ phá graph call). |
| `label` | string, chỉ hợp lệ với `marker = throw | catch_start | if_true` | Nhãn exception / điều kiện hiển thị. Throw không label → fallback exception type hoặc throw vô danh (match mọi handler). |
| `keep_call` | mặc định: `throw → true`, còn lại `false` | `throw` (ereport, longjmp…) là call thật → vẫn push `CallRecord`. Macro (PG_TRY…) không phải call thật → chỉ emit marker, không CallRecord. |

`[[flow_rules]]` (key cũ) bị bỏ qua + warn khi load — đã gộp vào `[[effect_rules]]`.

### Kết quả runtime (không cần thêm code ở engine)

Marker được emit vào chain tại đúng vị trí macro — giống như chúng là ngôn ngữ
gốc — nên mọi query hưởng lợi chung từ marker infra mà không cần sửa thêm code:
- Chain của `codegraph_flow` / `codegraph_mermaid kind=flow` hiển thị đủ
  TRY/THROW/CATCH.
- `codegraph_search_flow` với pattern `THROW,CATCH` tìm ra mọi hàm dùng idiom
  này.
- `codegraph_trace_full_flow` — interpreter có sẵn runtime `catch_stack`: khi
  gặp `THROW` trong try body nó pop catch_stack, nhảy tới handler khớp
  exception type (handler không label → match mọi exception); frame không
  catch được thì unwind qua caller frames — chính xác semantics exception
  engine kiểu longjmp như PostgreSQL. `throw` trong handler (`PG_RE_THROW`) →
  `catch_stack` rỗng → unwind lên caller/caller's try khớp.

### Giới hạn

- Lexical-only: rule theo call-name, không aware receiver/imports. Nhảy
  `sigsetjmp(...) == 2` thủ công không đi qua macro chuẩn sẽ không model.
- Throw/swallow cross-function chỉ ghép chính xác theo các path mà trace đã
  có explore theo caller chain của GraphIndex (giới hạn chung của trace).

### Vòng lặp khai báo (LLM-authored)

1. Đọc source quanh macro để hiểu cấu trúc (PG_TRY / body / throw / PG_CATCH /
   dedupe bước mở-đóng trước khi chốt rule).
2. `codegraph_config_effects {op:"add", matcher:"exact", text:"PG_TRY", marker:"try_start"}`
   … tạo tương tự cho PG_CATCH/PG_END_TRY/throw-er. Matcher `contains` chỉ khi
   văn bản tên gọi đủ đặc trưng (đặc biệt tránh contains `longjmp` — sẽ false
   positive lên `siglongjmp`/`setjmp`; dùng `prefix`/`exact`).
3. `codegraph_index` — rule hiệu lực từ lần index này.
4. Verify: `codegraph_search_flow {pattern:"THROW,CATCH"}` +
   `codegraph_flow {node:<id>}`; nếu cần `codegraph_trace_full_flow` xem path
   có nhảy vào handler và/hoặc unwind đúng.
5. Tinh chỉnh matcher/label; `remove` theo index từ `list` (index order = thứ
   tự match, rule trước thắng).
