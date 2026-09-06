# Binary Analysis (radare2)

CodeGraph có thể xây dựng semantic graph **trực tiếp từ file binary** (ELF, PE, Mach-O) bằng cách tích hợp [radare2](https://rada.re/n/). Binary được phân tích thành symbols, call chains (kèm control-flow markers) và nạp vào graph như một "nguồn code" bình thường — tức là agent có thể query call flow của một executable/*nội dung trong binary* bằng cùng bộ MCP tools như với source code.

## Tổng quan

```
files → tree-sitter (source) ─┐
                              ├→ GraphIndex::ingest → semgraph → MCP server
binaries → radare2 (r2pipe) ──┘
```

Sau khi tree-sitter parse các file source, orchestrator gọi `codegraph_binary::collect_binaries` để scan và phân tích binary, rồi append kết quả `ParseResult` (với `language = "binary"`) vào cùng danh sách ingest.

Các bước chính trong `crates/codegraph-binary`:

1. **Scan** (`scan.rs`) — `find_binaries(root)` duyệt workspace (tôn trọng `.gitignore`, `.codegraphignore`) và nhận diện binary theo **magic bytes**: ELF (`\x7fELF`), PE (`MZ`), Mach-O (little/big-endian) và fat Mach-O.
2. **Cache** (`cache.rs`) — nếu binary chưa đổi (key là sha256 của `path | mtime | size`), kết quả được load từ `.codegraph/binary-cache/` thay vì phân tích lại.
3. **Extract** (`extract.rs`) — mở session radare2 qua `r2pipe` (spawn `r2 -q0 -N -e scr.color=0 -e scr.utf8=0`, giao tiếp JSON) và trích xuất:
   - **Functions** (`aflj`) → `SymbolKind::Function`, bỏ qua PLT thunks `sym.imp.*`; signature gồm địa chỉ, size, calling convention và signature r2.
   - **Imports** (`iij`) → function symbols với annotation `import`; địa chỉ PLT được map để các call resolve về đúng import.
   - **Strings** (`izj`) → `SymbolKind::Constant` đặt tên `str:<vaddr>`.
   - **Call chains** — nếu `cfg_markers` bật: disassemble từng function bằng `pdfj @ <addr>` và map op types thành markers:
     | r2 op type | Marker |
     |------------|--------|
     | `call` | CallRecord |
     | `cjmp` | `IF_TRUE` |
     | `jmp` (backward) | `LOOP_BACK` |
     | `ret` | `RETURN` |
     | `swi` / `syscall` | `THROW` |

     Nếu tắt `cfg_markers`: chỉ lấy call edges nhẹ từ `agCj` (không có markers).
4. **Ingest** — `ParseResult` được nạp vào `GraphIndex` như mọi nguồn khác; từ đó `codegraph_search_symbol`, `codegraph_flow`, `codegraph_callers`, `codegraph_impact`, `codegraph_context`… hoạt động trên binary y như source.

## Cấu hình

Section `[binary]` trong `.codegraph/config.toml`:

```toml
[binary]
enabled = true        # bật/tắt phân tích binary khi index (mặc định bật)
depth = "aaa"         # "aaa" (đầy đủ, chính xác nhất) | "fast" (af + aar + aac, nhanh hơn)
cfg_markers = true    # xây markers IF/LOOP/RETURN/THROW từ CFG từng function (pdfj)
cache = true          # cache kết quả theo (path, mtime, size) trong .codegraph/binary-cache/
```

Ghi chú:

- `depth = "fast"` phù hợp binary lớn — bỏ qua phân tích sâu của `aaa`.
- Tính năng này nằm sau cargo feature `binary` của crate `codegraph-extract`, **đã bật trong `default` features** nên bản build mặc định có sẵn.

## Yêu cầu (Prerequisites)

- `radare2` phải có trong `PATH`:

  ```bash
  # macOS
  brew install radare2

  # Debian / Ubuntu
  apt install radare2
  ```

- Kiểm tra bằng `codegraph doctor` (cũng check `r2` trên PATH).
- Nếu thiếu `r2`, quá trình index **không lỗi** — phân tích binary bị bỏ qua và in warning.

## Query kết quả

Binary analysis không thêm MCP tool mới — kết quả chảy vào graph thông thường:

```json
// Tìm function trong binary
codegraph_search_symbol { "query": "main", "kind": "function" }

// Xem call chain của một function trong binary (kèm markers IF/LOOP/RETURN/THROW)
codegraph_flow { "name": "..." }

// Xem ai gọi hàm
codegraph_callers { "name": "..." }
```

Các symbol từ binary có `language = "binary"`, giúp phân biệt với symbol từ source.

## Kiến trúc code

| File (trong `crates/codegraph-binary/`) | Vai trò |
|---|---|
| `src/lib.rs` | `collect_binaries` — orchestration scan → cache → extract |
| `src/r2.rs` | Wrapper `r2pipe`: spawn session, `cmd`/`cmdj`, `analyze(depth)`, `r2_available()`, `r2_version()` |
| `src/scan.rs` | Tìm binary theo magic bytes (ELF / PE / Mach-O / fat Mach-O) |
| `src/extract.rs` | Trích xuất functions/imports/strings/chains → `ParseResult` |
| `src/model.rs` | Structs serde cho output JSON của `aflj` / `iij` / `izj` / `pdfj` / `agCj` |
| `src/cache.rs` | Cache JSON theo sha256(path, mtime, size) |
| `src/config.rs` | `BinaryConfig`, `AnalysisDepth` |
| `tests/extract.rs` | Integration test (chạy với `--ignored`, cần r2 cài sẵn) |

## Giới hạn

- Chỉ hỗ trợ các format nhận diện được qua magic bytes: ELF, PE, Mach-O (kể cả fat binary); stripped/obfuscated binary vẫn phân tích được nhưng tên function có thể là địa chỉ.
- Call graph phụ thuộc độ chính xác của radare2 analysis — với binary lớn nên cân nhắc `depth = "fast"` đổi lấy tốc độ.
- String được đưa vào graph dưới dạng constant `str:<vaddr>`, không gắn với function nào.
