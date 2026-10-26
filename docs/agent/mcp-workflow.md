# MCP workflow — CodeGraph-first

CodeGraph MCP trả lời câu hỏi **cấu trúc** (symbol, edge, call chain) nhanh hơn grep và ít token hơn.
Đây là bước đầu tiên, không phải bước duy nhất.

## Luồng chuẩn

```
codegraph_init {path}            # bind (không index)
codegraph_context {query}        # gói symbol + callers + callees
codegraph_symbol / callers / callees / impact
  ↓ edit code
codegraph_graphcode_diff {diff}  # draft impact, không mutate index
codegraph_index {}               # re-index khi cần dữ liệu mới
verify: codegraph_search_flow / codegraph_flow
```

## Khi nào fallback grep/read

| Tình huống | Dùng |
|---|---|
| Text literal (string, comment, tên config key) | `grep` |
| `docs/`, `README.md`, `config.toml` | `read` |
| File không được index (vd `.md`, script ngoài workspace) | `read` |
| Vừa sửa code, index chưa re-index | `read` để xác nhận |
| Đọc toàn bộ implementation của 1 file lớn | `read` sau khi `codegraph_symbol` khoanh vùng |

## Index freshness

- Index chỉ đổi khi chạy `codegraph_index` (hoặc watcher trong `serve --mcp`).
- Sau refactor schema/config, kết quả cũ có thể sai. Re-index trước khi tin.
- `.codegraph/` nằm trong `.gitignore` — không commit.

## Large index

Search tool nhận `timeout_ms` (mặc định 20000) và trả `resume: "<id>"` khi hết budget.
Retry **đúng call + đúng args** kèm `resume` để đi tiếp, không quét lại từ đầu.

## Bind lỗi

Nếu tool báo session chưa bind / không có root: gọi `codegraph_init` với path tuyệt đối.
Không tự scan filesystem để thay thế.
