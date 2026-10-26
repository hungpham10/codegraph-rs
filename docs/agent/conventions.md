# Rust conventions

## Crates

```
codegraph-core        Error + semgraph model (Symbol, CallRecord, Marker, EffectType, EffectRule, FlowMarker)
codegraph-extract     tree-sitter parsers, LangSpec + 15 language modules, config.toml CRUD
codegraph-graph       GraphIndex: registry + 2 engines + pluggable storage + embeddings
codegraph-context     Markdown/JSON context formatter
codegraph-api         GraphApi (async query surface) trên SharedGraphIndex
codegraph-graphql     GraphQL HTTP API (queries + mutations)
codegraph-sboxes      Behavior sandbox: Cranelift JIT + Rhai mocks
codegraph-mcp         MCP server (rmcp) + tool registry/dispatch + session
codegraph-bench       Criterion benchmarks + codspeed
codegraph-installer   Multi-agent installer (claude/codex/cursor/opencode/...)
codegraph-binary      Binary graph qua radare2
codegraph-docs        Document graph (HCL/YAML/JSON/TOML)
codegraph-source      Source abstraction (DiskSource, GitSource, registry)
codegraph-web         Web UI assets
codegraph             CLI (init/deinit/embed/serve) + watcher
```

## Đặt code mới

| Loại | Nơi |
|---|---|
| Kiểu dữ liệu dùng chung nhiều crate | `codegraph-core` |
| Parse / classify một ngôn ngữ | `codegraph-extract/src/languages/<lang>.rs` + đăng ký `languages.rs` |
| Storage backend mới | `codegraph-graph/src/storage/<backend>.rs` + `Storage` trait |
| Query/aggregate mới cho agent | `codegraph-api`, rồi expose ở `codegraph-mcp` / `codegraph-graphql` |
| Config key mới | `codegraph-extract/src/config.rs` + `DEFAULT_CONFIG_TOML` |

## Quy ước

- Error: dùng `codegraph_core::Error` (thiserror); API public trả `Result<T, Error>`. Không `unwrap()` trong lib ngoài test.
- Feature gate: storage/embedding phải qua `#[cfg(feature = "...")]`; xem `docs/development.md` cho bảng feature.
- Dependency: ưu tiên std, rồi dependency đã có trong workspace. Không thêm crate mới cho vài dòng.
- Comment: tiếng Việt, theo style code xung quanh; chỉ giải thích **why**, không mô tả lại code.
- Serialization ổn định: `Marker` / `EffectType` / `FlowMarker` id và tên là contract — không đổi số/chuỗi cũ.
- Config ghi bằng `toml_edit` để giữ nguyên section khác của người dùng.
- Không commit `.codegraph/`, `target/`, `Cargo.lock` (đã gitignore).
