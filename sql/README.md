# SQL schema design — storage shared (PostgreSQL / MySQL)

Thiết kế schema cho **storage RDBMS mới** của codegraph, đặt **cạnh** các backend
local hiện có (sqlite / lmdb / redis / memory). Mục đích: 1 cơ sở dữ liệu dùng
**chung cho nhiều repository và nhiều server instance** — mỗi repo là một
partition độc lập, các instance (CLI / watcher / MCP stdio / MCP HTTP) cùng đọc
cùng ghi một DB.

DuckDB / S3 lakehouse sẽ được thêm sau (`sql/duckdb/…`) trên **cùng model**
partition + sharding này.

## Cấu trúc thư mục

```
sql/
  README.md            ← file này
  postgres/            ← DDL + migration PostgreSQL
    001-initial-schema.sql
  mysql/               ← DDL + migration MySQL (cùng design, khác dialect)
    001-initial-schema.sql
```

## Quy ước đặt tên & quản lý version (migration)

- Mỗi file schema đặt tên `NNN-<tên thay đổi>.sql`, với `NNN` là số **3 chữ số
  tăng dần** (`001-`, `002-`, ...). Tên mô tả ngắn gọn thay đổi (kebab-case),
  VD `002-add-repo-statistics.sql`.
- **Thứ tự áp dụng = thứ tự số** (lexicographic). Migration chạy đúng thứ tự đó.
- **Không sửa / xoá file đã apply** — một thay đổi mới luôn là một file kế tiếp.
  Nếu migration 002 cần sửa, viết 003 (ALTER/backfill), không sửa 002.
- Bảng `schema_migrations (version, applied_at)` (global, không có `repo_id`)
  ghi lại version đã chạy — nền cho migration runner ở phase code
  (sea-orm migrate / sqlx migrate đều theo convention này).
- **Migration là GLOBAL (schema-level)** — thay đổi cấu trúc bảng ảnh hưởng mọi
  repo. Dữ liệu (`repo_id`) là runtime, không nằm trong file migration.

## Mô hình dữ liệu

### 1. Partition theo repository

- Mọi bảng dữ liệu dẫn đầu bằng cột `repo_id BIGINT NOT NULL` — là **số u64**
  sinh ngẫu nhiên lúc `codegraph init`, lưu trong `.codegraph/config.toml`
  (`[storage] repo_id = <số nguyên>`). Một project root (`.codegraph/`) = một
  repository.
- PK composite `(repo_id, …)` trên mọi bảng → các repo cô lập hoàn toàn;
  re-index / xoá một repo chỉ là `DELETE … WHERE repo_id = ?`.
- `repo_id` nằm trong **handle của backend** (thuộc `Storage` impl), không đụng
  trait `Storage`/`Tx` — mỗi `GraphIndex`/`SharedGraphIndex` instance = một repo.

### 2. Sharding giữ nguyên

- Radix trie (chain engine) vẫn dùng `CHAIN_SHARDING = 64`,
  `shard_of(elem) = elem % 64` — toàn bộ key nằm trong đúng một shard (không
  fan-out khi search).
- `rt_roots (repo_id, shard, root)` ánh xạ shard → root node; `rt_shortcuts`
  (substring index) cũng theo `shard` như cũ. Sharding chỉ là partition nội bộ
  của trie — không đổi hành vi query so với sqlite/lmdb hiện tại.

### 3. Hai nhóm bảng

**Entity store (`sg_*`)** — dữ liệu cấu trúc, dùng **cột thật** (lợi ích của
relational: query SQL trực tiếp, join, index; đồng thời sẵn sàng cho lakehouse /
parquet ở phase DuckDB):

| Bảng | PK | Nội dung |
|---|---|---|
| `sg_symbols` | `(repo_id, id)` | `Symbol` — cột thật; `annotations` là cột `TEXT` (app lưu JSON string qua `serde_json`, đọc bằng `from_str` — không dùng JSON/JSONB để sqlx decode `String` được) |
| `sg_files` | `(repo_id, path)` | `FileInfo` |
| `sg_call_records` | `(repo_id, func)` | call records của từng function (JSON bytes) |
| `sg_call_names` | `(repo_id, name)` | inverted index call name → call sites (JSON bytes) |
| `sg_meta` | `(repo_id)` | `version` của repo — dò freshness |
| `sg_next_id` | `(repo_id)` | registry counter (symbol id), seed `100` (`SYMBOL_BASE`) |

**Radix trie (`rt_*`)** — dữ liệu nhị phân của trie (không có lợi ích relational,
giữ cột bytea/blob; vẫn partition theo `repo_id`):

| Bảng | PK | Nội dung |
|---|---|---|
| `rt_nodes` | `(repo_id, id)` | node trie: `prefix` + `record`; id 0 = sentinel (EMPTY) |
| `rt_children` | `(repo_id, parent, child)` | cạnh cha-con |
| `rt_roots` | `(repo_id, shard)` | gốc từng shard (root 0 = EMPTY, tạo lazy) |
| `rt_meta` | `(repo_id, record)` | metadata opaque theo record |
| `rt_keylen` | `(repo_id, record)` | độ dài key (filter depth) |
| `rt_shortcuts` | `(repo_id, shard, elem, node_id)` | substring candidate index |
| `rt_chains` | `(repo_id, record)` | chain bytes (u64 LE/element) — nguồn rebuild |
| `rt_edges` / `rt_node_meta` | `(repo_id, …)` | legacy stream (trait còn giữ, GraphIndex chưa dùng) |
| `rt_node_blooms` | `(repo_id, id)` | bloom filter (feature `bloom-search`) |
| `rt_counter` | `(repo_id)` | node-id allocator, seed `1` |

### 4. Pattern vận hành (comment chi tiết trong từng file)

- **Seed per repo** (idempotent, `ON CONFLICT DO NOTHING` / `INSERT IGNORE`):
  sentinel node 0, `rt_counter.next = 1`, `sg_next_id.next = 100`, `sg_meta.version = 0`.
- **Counter atomic per repo**:
  - Postgres: `UPDATE rt_counter SET next = next + 1 WHERE repo_id = $1 RETURNING next - 1`
    (tương tự `sg_next_id`).
  - MySQL: `UPDATE rt_counter SET next = LAST_INSERT_ID(next) + 1 WHERE repo_id = ?`
    rồi `SELECT LAST_INSERT_ID()` (connection-scoped, trả **next cũ** = id vừa cấp,
    cùng semantics PG/sqlite — không dùng `LAST_INSERT_ID(next + 1)`, nó trả next mới).
- **Upsert**: Postgres `ON CONFLICT (repo_id, pk) DO UPDATE`; MySQL
  `ON DUPLICATE KEY UPDATE`.
- **Probe version** (`SharedGraphIndex::ensure_fresh`):
  `SELECT version FROM sg_meta WHERE repo_id = ?` — rẻ, độc lập với instance.
- **Full re-index** (clear): xoá toàn bộ `sg_*` + `rt_*` theo `repo_id`, reset
  counters + version về seed.

## Khác biệt giữa 2 dialect

| | PostgreSQL | MySQL |
|---|---|---|
| DDL transactional | có (bọc `BEGIN/COMMIT`) | **không** — chạy tuần tự, không bọc transaction |
| binary | `BYTEA` | `LONGBLOB` |
| JSON | `JSONB` | `JSON` |
| timestamp | `TIMESTAMPTZ DEFAULT now()` | `TIMESTAMP(6) DEFAULT CURRENT_TIMESTAMP(6)` |
| cột key văn bản | `TEXT` thoải mái (PK được) | không cho `TEXT` làm PK/index toàn vẹn → `VARCHAR(700)` |
| case-sensitivity | chính xác theo byte | `COLLATE utf8mb4_bin` để giữ case-sensitive cho name/path |
| composite key `rt_shortcuts` | PK `(repo_id, shard, elem, node_id)` | `elem LONGBLOB` không vào PK được → index `elem(255)` prefix + PK không gồm elem (xem ghi chú) |

> Ghi chú MySQL về giới hạn key: index key tối đa 3072 bytes (utf8mb4 → 4 byte/
> ký tự). `repo_id BIGINT` (8 bytes) + `name/file/path VARCHAR(700)` (700 × 4 =
> 2800 bytes) ≈ 2808 bytes — nằm dưới 3072, vừa đủ. Giá trị dài hơn 700 ký tự
> cần hash key (md5/sha256) ở phase sau; schema hiện tại chấp nhận giới hạn này
> (tên call / path thực tế hiếm khi vượt).
>
> `rt_shortcuts.elem` là bytes nhị phân (element id encode) có thể rất dài →
> MySQL dùng prefix index `elem(255)`; Postgres giữ PK đầy đủ. Vì lookup luôn
> đi qua `(repo_id, shard, elem)` với elem truyền đúng độ dài thật, prefix index
> 255 bytes là đủ (kiểm tra lại khi implement — nếu cần chính xác tuyệt đối,
> thêm cột `elem_hash CHAR(32)`).

## Liên hệ với code hiện tại & kế hoạch

- Schema này là nguồn chân lý cho phase code: backend sea-orm (`RdbmsStorage`
  implement `Storage` + `Tx`), routing DSN `postgres://`/`mysql://`, `repo_id`
  vào config, `SharedGraphIndex` probe version, `IndexRegistry` (session giữ ref
  tới index dùng chung).
- Mapping với `crates/codegraph-graph/src/storage/sqlite.rs` (schema hiện tại):
  cùng tập bảng `sg_*`/`rt_*`, thêm cột `repo_id` + bỏ `CHECK(id = 1)` (đã thay
  bằng PK `(repo_id)`), entity `sg_symbols` chuyển từ JSON BLOB sang cột thật.
- DuckDB / S3 lakehouse: `sql/duckdb/001-…sql` — cùng model, bảng thành file
  parquet / duckdb, partition theo `repo_id`.
