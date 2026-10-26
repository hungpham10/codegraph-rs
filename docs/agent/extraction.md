# Extraction & config

## Một bảng duy nhất: `[[effect_rules]]`

Mỗi rule khai báo `effect`, `marker`, hoặc cả hai — ít nhất một trong hai.
Không có bảng riêng cho control-flow; `[[flow_rules]]` là key cũ, bị bỏ qua (+ warn khi load).

```toml
[[effect_rules]]
call   = { exact = "PG_TRY" }        # prefix | contains | exact
marker = "try_start"

[[effect_rules]]
call   = { contains = "ereport" }
effect = "panic"                     # tác động bên ngoài
marker = "throw"                     # + vai trò control-flow
label  = "Error"                     # chỉ với throw | catch_start | if_true
keep_call = true                     # mặc định true cho throw
```

Thứ tự khai báo = thứ tự match; rule đầu tiên thắng. Rule config được xét **trước** bảng effect default, nên `effect = "none"` dùng để chặn một default (shadow).

## Marker whitelist + ID

| Marker | ID | Ý nghĩa |
|---|---:|---|
| `loop` | 1 | mở vòng lặp |
| `if_true` | 3 | nhánh đúng |
| `if_false` | 4 | nhánh sai / else |
| `branch_end` | 5 | đóng nhánh |
| `return` | 6 | return sớm |
| `loop_back` | 7 | đóng vòng lặp |
| `break` | 10 | break |
| `continue` | 11 | continue |
| `throw` | 12 | ném exception |
| `try_start` | 22 | mở try |
| `try_end` | 23 | đóng try |
| `catch_start` | 24 | mở handler |

`CALL_ENTER` (17) / `CALL_EXIT` (18) không nằm trong whitelist — chèn marker vào giữa wrapper sẽ khiến interpreter coi marker là callee.

## `keep_call`

- `throw` → mặc định `true`: giữ `CallRecord` (ereport/longjmp là call thật).
- Marker khác → mặc định `false`: chỉ emit marker, thay thế call.
- `false` vẫn walk children trước khi emit, nên tham số (`IF(has_perm(u))`) không mất call edge.

## Lifecycle try/catch

Macro PG-style cần đủ cặp để interpreter ghép đúng:

```
PG_TRY → try_start        TRY_START
  body...
PG_CATCH → catch_start     TRY_END + CATCH_START (+ label)
  handler...
PG_END_TRY → try_end       BRANCH_END
```

- `catch_start` ngoài scope try bị bỏ qua (tránh marker mồ côi).
- Try lồng nhau dùng stack phase `Body`/`Handler`; `branch_end` (ENDIF) không đụng state try.
- Handler phải nằm ngay sau `TRY_END` thì interpreter mới thấy.

## Workflow khai báo (LLM-authored)

1. Đọc source quanh macro để hiểu cấu trúc.
2. `codegraph_config_effects {op:"add", matcher:"exact", text:"PG_TRY", marker:"try_start"}`
3. `codegraph_index` — rule có hiệu lực từ lần index này.
4. Verify: `codegraph_search_flow {pattern:"THROW,CATCH"}` và `codegraph_flow {node}`.
5. `op:"list"` để xem index, `op:"remove"` theo index; `op:"add"` append cuối.

Ưu tiên `exact`/`prefix`; `contains:"longjmp"` cũng trúng `siglongjmp`/`setjmp`.

## Thêm ngôn ngữ

1. Tạo `crates/codegraph-extract/src/languages/<lang>.rs` với `LangSpec` + parser functions.
2. Đăng ký trong `crates/codegraph-extract/src/languages.rs`.
3. Feature `lang-<lang>` trong `Cargo.toml` của crate, thêm vào `all-langs`.
4. Fixture ở `tests/fixtures/`, test chain/flow ở `tests/`.
5. Cập nhật `docs/configuration.md` nếu thêm toggle.

## Giới hạn đã biết

- Rule theo call-name, không receiver/import-aware.
- Không có switch/finally macro; không suy ra label từ argument.
- Cross-function throw chỉ ghép theo path mà trace đã explore.
