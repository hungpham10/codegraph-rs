# CodeSmell Coverage — test-coverage estimate & threshold

`codesmell coverage` ước lượng **bao nhiêu phần trăm business logic đang được
unit test gọi tới** và cho phép policy đặt **ngưỡng tối thiểu** để `codesmell
check` chặn build khi không đạt. Document này là hướng dẫn cấu hình đầy đủ.

> Cùng đọc: [docs/codesmell.md](codesmell.md) — tổng quan về CodeSmell và
> `testing.missing_test` (rule per-function).

## How the estimate works

CodeSmell **không chạy test**. Nó parse repo bằng CodeGraph (AST + call graph)
và dùng heuristic sau:

```
covered(function) = tồn tại direct caller C của function
                    sao cho đường dẫn của C khớp một glob trong `test_paths`
coverage% = (logic functions covered / tổng logic functions) × 100
```

- **Logic function** là function/method được chọn bởi `selectors` (xem dưới).
  Với `selectors` trống, mọi function/method đều được tính.
- **Covered** được xác định qua call graph: function có caller nằm trong file
  test (khớp `test_paths`) → covered. Đây chính là cơ chế mà rule
  `testing.missing_test` dùng cho từng function — coverage gom nó thành %.
- Repo không có logic function nào → 100% (không có gì để test).

### Đây là heuristic, không phải statement coverage

Vì đo ở mức function qua call graph, tool không biết nhánh nào bên trong
function được chạy. Nó trả lời "function này có test gọi tới chưa?", không phải
"mỗi dòng đã được thực thi chưa?". Nếu cần line coverage thật, dùng `cargo
llvm-cov` / `tarpaulin` song song — hai con số đo hai thứ khác nhau.

### Limitations

- **Macro**: call nằm bên trong body macro (`assert_eq!(calc_total(1, 2), 3)`)
  không được tree-sitter ghi nhận là call node. Gọi ra biến trước:
  `let got = calc_total(1, 2); assert_eq!(got, 3);`
- **Direct caller only**: function A gọi B, test gọi A → B vẫn uncovered
  (depth = 1). Wrapper/indirection làm coverage trông thấp hơn thực tế.
- Test được nhận diện theo **vị trí file** (`test_paths`), không phải theo
  attribute `#[test]`.

## Configuration

Thêm bảng `[testing.coverage]` vào `.codesmell/policy.toml`:

```toml
[testing.coverage]
# Ngưỡng % tối thiểu. `codesmell check` sinh violation `testing.coverage`
# khi estimate dưới ngưỡng này. Bỏ field = chỉ đo, không enforce.
line = 80

# Glob (repo-relative) nhận diện file test. Caller khớp một trong các glob
# này → function được coi là covered.
test_paths = ["tests/**", "**/*_test.rs", "**/*_test.go", "**/test_*.py"]

# Định nghĩa các "tầng" của repo theo path glob — nguyên liệu cho selectors.
layers = [
  { name = "service", paths = ["src/services/**"] },
  { name = "repository", paths = ["src/repositories/**"] },
]

# Function nào được tính vào phép chia. Một function là logic khi thoả BẤT KỲ
# selector nào:
#   - layers  : function nằm trong tầng được liệt kê
#   - min_lines: body dài >= N dòng (bất kể tầng)
# Trống = tính mọi function/method.
selectors = [
  { layers = ["service"] },
  { min_lines = 20 },
]
```

### Chọn gì đưa vào phép tính

Coverage % chỉ có ý nghĩa khi mẫu đo đúng — getter/trivial code làm con số
phình to. Ví dụ thực dụng:

| Mục tiêu | `selectors` |
|---|---|
| Chỉ tầng service | `[{ layers = ["service"] }]` |
| Function đủ dài ở bất kỳ đâu | `[{ min_lines = 20 }]` |
| Service + mọi function lớn | `[{ layers = ["service"] }, { min_lines = 20 }]` |

`layers`/`selectors`/`test_paths` dùng **cùng ngữ nghĩa** với params của rule
`testing.missing_test` — giữ hai chỗ đồng bộ để rule per-function và ngưỡng
tổng thể không mâu thuẫn nhau.

### Severity

Violation `testing.coverage` mặc định là `required`. Đổi qua:

```toml
[severity]
"testing.coverage" = "blocking"   # hoặc info / warning / required
```

## CLI

```bash
# Báo cáo toàn repo (human)
codesmell coverage
# Coverage: 62% (31 of 50 logic functions tested) — threshold 80%: NOT MET
#
# Uncovered:
#   --> src/services/order_service.rs:12  place_order
#   --> src/services/order_service.rs:48  refund_order

# Machine-readable cho CI dashboard / PR bot
codesmell coverage --format json
# { "percent": 62, "total": 50, "covered": 31, "threshold": 80,
#   "met": false, "uncovered": [ { "file": "...", "line": 12, ... } ] }

# Chỉ đo code vừa thay đổi (diff ở stdin) — dùng cho CI gate per-PR
git diff main | codesmell coverage --diff -

# Scope theo thư mục
codesmell coverage src/services/
```

Exit code: `1` khi dưới ngưỡng (giống `check`, có thể đổi mốc qua
`--fail-on`). Không có ngưỡng trong policy → exit 0 trừ khi lỗi.

## Enforcement trong `codesmell check`

Khi policy đặt `line`, `check` tự chạy phép đo và sinh **một violation duy
nhất** nếu dưới ngưỡng (danh sách chi tiết nằm ở `codesmell coverage`):

```
error[testing.coverage]: test coverage 62% is below threshold 80% (19 of 50 logic functions untested)
  --> src/services/order_service.rs:12
  hint: add unit tests calling the uncovered logic, starting with the largest: ...
```

Violation tham gia cơ chế `--fail-on` như mọi rule khác, và kết quả đo
(`percent`/`uncovered[]`) cũng có trong JSON của `check`
(`report.coverage`).

## CI recipe (GitHub Actions)

```yaml
- name: CodeSmell conventions
  run: codesmell check --fail-on required

- name: CodeSmell coverage gate (changed code only)
  run: git diff origin/main | codesmell coverage --diff - --format json
```

Gate per-PR với `--diff` đo coverage của **code vừa đổi** — nợ coverage cũ
không chặn PR mới, nhưng code mới phải được test.

## Relation to `testing.missing_test`

| | `testing.missing_test` | `[testing.coverage]` |
|---|---|---|
| Granularity | từng function | cả scope |
| Đầu ra | violation per function | % + danh sách uncovered |
| Dùng khi | LLM/dev vừa viết hàm mới | CI gate tổng thể |
| Config | `params` của `[[rhai.rule]]` | `[testing.coverage]` table |

Hai cơ chế chia sẻ cách chọn logic function — nếu `missing_test` sạch mà
coverage vẫn thấp, đó là nợ test từ trước, đúng thứ CI cần nhìn thấy.
