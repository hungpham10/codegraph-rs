# Kế hoạch tối ưu storage SQL và SQLite

## 1. Mục tiêu

Tối ưu lớp storage SQL hiện tại, trước hết cho SQLite, nhưng vẫn giữ khả năng mở rộng sang PostgreSQL, MySQL và các database khác.

Mục tiêu chính:

- giảm số lần round-trip giữa application và database;
- giảm số transaction nhỏ và chi phí WAL/fsync;
- giữ tính atomic của ingest và transaction radix;
- cải thiện batch read/write;
- giảm kích thước index và write amplification;
- giữ abstraction ở mức semantic operation, không khóa code vào SQLite;
- có benchmark và contract test để xác minh từng tối ưu.

Không thay đổi API bên ngoài của `GraphIndex` nếu chưa cần thiết.

## 2. Kiến trúc hiện tại

Các lớp chính:

- `CategoryStorage`: lưu radix nodes, children, roots và transaction.
- `EntityStorage`: lưu symbols, chains, call records, call-name index, files, version, stats và embeddings.
- `Storage`: trait gộp các storage capability.
- `SqliteStorage`: backend SQLite dùng `sqlx::SqlitePool`, WAL và BLOB.
- `CachedStorage`: cache LRU nằm phía trên backend.
- `PostgresStorage` và `MySqlStorage`: backend SQL khác có cùng semantic interface nhưng SQL dialect riêng.

Các file cần tham khảo:

- [`crates/codegraph-graph/src/storage.rs`](../crates/codegraph-graph/src/storage.rs)
- [`crates/codegraph-graph/src/storage/sqlite.rs`](../crates/codegraph-graph/src/storage/sqlite.rs)
- [`crates/codegraph-graph/src/storage/postgres.rs`](../crates/codegraph-graph/src/storage/postgres.rs)
- [`crates/codegraph-graph/src/storage/mysql.rs`](../crates/codegraph-graph/src/storage/mysql.rs)
- [`crates/codegraph-graph/src/storage/cached.rs`](../crates/codegraph-graph/src/storage/cached.rs)

Index Codegraph hiện có khoảng 2.934 symbols, 5.921 edges và 152 files; đủ để dùng làm baseline kiểm thử kiến trúc nhưng chưa đủ lớn để kết luận hiệu năng production. Cần benchmark với corpus lớn hơn.

## 3. Nhận định quan trọng từ implementation hiện tại

### 3.1. SQLite schema đang trộn hai loại dữ liệu

`SqliteStorage` lưu cả:

- radix/search data trong các bảng `rt_*`;
- entity/index data trong các bảng `sg_*`.

Phần lớn entity payload được serialize thành BLOB JSON:

- `sg_symbols.data`;
- `sg_call_records.records`;
- `sg_call_names.sites`.

Điều này giúp backend và format đơn giản, nhưng database không thể index hoặc filter trực tiếp trên các field nằm trong BLOB.

### 3.2. Nhiều primitive write tự tạo transaction nhỏ

Các method như `save_symbol`, `set_chain`, `set_call_records`, `set_call_name_index`, `upsert_file`, `set_version` và `set_stats` tự acquire connection rồi thực thi statement riêng.

Nếu pipeline ingest gọi các method này từng item, SQLite phải xử lý rất nhiều implicit transaction. Với WAL, vấn đề chính không phải chỉ là SQL execution mà còn là lock, journal và fsync.

### 3.3. `SqliteTx` đã buffer operation nhưng commit vẫn từng statement

Transaction radix hiện buffer node và `TxOp`, sau đó commit atomic. Tuy nhiên mỗi node/operation vẫn phát sinh một SQL execution riêng.

Đây là nền tảng tốt về correctness nhưng còn dư địa lớn để batch hóa.

### 3.4. Batch read mới chỉ được khai báo ở trait

`get_nodes` và `get_childrens` có default implementation lặp lại các single-read method. SQLite chưa có implementation batch riêng.

Radix DFS vì vậy có thể phát sinh rất nhiều query/connection acquire thay vì một read transaction hoặc một số ít batch query.

### 3.5. SQLite đã có một số thiết lập đúng

Hiện backend dùng:

- WAL;
- busy timeout 5 giây;
- `INTEGER PRIMARY KEY` cho nhiều integer ID;
- `sg_stats` để đọc counts O(1);
- fallback vector search nếu `sqlite-vss` không khả dụng.

Các thiết lập này cần giữ, nhưng nên đưa phần tuning vào configuration/capability thay vì hard-code toàn bộ.

## 4. Giai đoạn 0 — đo baseline

Trước khi sửa behavior, bổ sung benchmark và instrumentation.

### 4.1. Các workload cần đo

- mở database mới;
- reopen database đã có dữ liệu;
- ingest nhỏ, vừa và lớn;
- full rebuild;
- incremental update nếu pipeline hỗ trợ;
- single node read;
- batch node read;
- single children read;
- batch children read;
- search DFS;
- concurrent readers;
- concurrent writers;
- KNN bằng brute-force và native ANN;
- kích thước database, WAL và thời gian checkpoint.

### 4.2. Metrics

- tổng thời gian;
- số SQL statement;
- số transaction;
- số lần acquire connection;
- thời gian chờ lock;
- thời gian commit;
- p50/p95/p99 cho read;
- file size và WAL size;
- peak memory.

### 4.3. Query plan

Với các query nóng, chạy `EXPLAIN QUERY PLAN` và ghi nhận:

- có dùng primary key/index hay không;
- có full table scan hay không;
- có sort ngoài ý muốn hay không;
- index có bị dùng sai do thứ tự cột hay không.

Không kết luận index tốt chỉ dựa trên schema; phải kiểm tra plan và benchmark.

## 5. Giai đoạn 1 — gom write thành unit of work

Đây là ưu tiên cao nhất.

### 5.1. API đề xuất

Không nên để `GraphIndex` biết SQL. Có thể thêm một abstraction semantic:

```rust
trait StorageWriteBatch {
    async fn save_symbol(&mut self, symbol: &Symbol) -> Result<()>;
    async fn set_chain(&mut self, record: usize, chain: &[u64]) -> Result<()>;
    async fn set_call_records(&mut self, func: u64, records: &[u8]) -> Result<()>;
    async fn set_call_name_index(&mut self, name: &str, sites: &[u8]) -> Result<()>;
    async fn upsert_file(&mut self, file: &FileInfo) -> Result<()>;
    async fn finalize(self) -> Result<()>;
}
```

Hoặc dùng một `WriteBatch` data object rồi để backend commit.

### 5.2. SQLite behavior

SQLite thực hiện toàn bộ batch trong một explicit transaction:

```sql
BEGIN IMMEDIATE;
...
COMMIT;
```

`BEGIN IMMEDIATE` chỉ nên dùng cho write batch ngắn và có thể cấu hình; không dùng cho các read dài.

### 5.3. Consistency requirement

Các bảng data, `sg_stats` và `sg_meta.version` phải được finalize trong cùng transaction. Reader chỉ được thấy snapshot cũ hoặc snapshot mới hoàn chỉnh.

Nên thêm semantic operation tương tự:

```rust
async fn finalize_index(
    &mut self,
    stats: IndexCounts,
    version: u64,
) -> Result<()>;
```

## 6. Giai đoạn 2 — batch hóa `SqliteTx`

### 6.1. ID allocation

Hiện mỗi `new_node` cập nhật `rt_counter` bằng `UPDATE ... RETURNING`.

Đề xuất thêm reserve block:

```rust
async fn reserve_ids(&mut self, count: usize) -> Result<IdRange>;
```

Một transaction reserve một dải ID, sau đó cấp ID local trong memory. Có thể xuất hiện gap khi rollback; gap được chấp nhận nếu ID chỉ cần unique và monotonic.

### 6.2. Materialize nodes theo chunk

Thay vì một `INSERT` cho từng node:

- gom node thành nhóm có kích thước giới hạn;
- dùng multi-row insert hoặc `QueryBuilder`;
- giới hạn chunk để không vượt parameter limit của backend;
- commit vẫn nằm trong một transaction.

### 6.3. Add child

Gom nhiều `AddChild` thành multi-row insert với conflict-ignore.

### 6.4. Move child

Giữ thứ tự delete/insert để bảo toàn semantics. Có thể tối ưu sau bằng temporary table hoặc set-based operation nếu benchmark chứng minh cần thiết.

### 6.5. Prepared statement

Với các statement lặp lại, dùng prepared statement hoặc cơ chế statement cache của driver. Cần đo để tránh tối ưu giả vì `sqlx` đã có cache tùy driver/configuration.

## 7. Giai đoạn 3 — batch read cho SQLite

Override các method mặc định của trait:

```rust
async fn get_nodes(&self, ids: &[usize]) -> Result<Vec<(Vec<u8>, usize)>>;
async fn get_childrens(&self, ids: &[usize]) -> Result<Vec<Vec<usize>>>;
```

Query dự kiến:

```sql
SELECT id, prefix, record
FROM rt_nodes
WHERE id IN (...);

SELECT parent, child
FROM rt_children
WHERE parent IN (...)
ORDER BY parent, child;
```

Backend phải:

- chunk danh sách ID nếu quá dài;
- preserve thứ tự input;
- báo lỗi nếu node bắt buộc bị thiếu;
- dùng read transaction khi cần snapshot nhất quán.

Đây là điểm có thể dùng chung cho PostgreSQL/MySQL, nhưng syntax placeholder và giới hạn parameter phải nằm trong backend adapter.

## 8. Giai đoạn 4 — rà soát schema/index

### 8.1. Index có khả năng dư thừa

Kiểm tra hai index:

- `idx_rt_children_parent` sau primary key `(parent, child)`;
- `idx_rt_shortcuts_lookup` sau primary key `(shard, elem, node_id)`.

Cả hai primary key đều đã có left-prefix phù hợp cho lookup. Chỉ xoá sau khi:

1. xác nhận query plan;
2. benchmark read;
3. benchmark write;
4. kiểm tra kích thước DB/WAL.

### 8.2. Dữ liệu nóng và dữ liệu lạnh

Giữ BLOB JSON cho dữ liệu ít filter. Đưa field thường search ra cột riêng, ví dụ:

```sql
sg_symbols (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    kind INTEGER NOT NULL,
    file TEXT NOT NULL,
    line INTEGER NOT NULL,
    scope_id INTEGER NOT NULL,
    payload BLOB NOT NULL
);
```

Index chỉ tạo theo query thật:

```sql
CREATE INDEX ... ON sg_symbols(name);
CREATE INDEX ... ON sg_symbols(kind, name);
CREATE INDEX ... ON sg_symbols(file, line);
```

Không normalize toàn bộ JSON nếu application chủ yếu load nguyên payload.

### 8.3. Call records và call-name index

Nếu thường xuyên cập nhật một phần `sites`, BLOB hiện tại gây write amplification vì phải rewrite toàn bộ payload.

Chỉ normalize thành bảng con khi profiling chứng minh đây là bottleneck. Nếu chủ yếu load toàn bộ theo name/function thì BLOB có thể vẫn là lựa chọn tốt hơn.

## 9. Giai đoạn 5 — SQLite tuning có kiểm soát

Tạo `SqliteOptions` riêng, không rải PRAGMA trong business logic.

Các nhóm option:

- `journal_mode`: WAL;
- `synchronous`: `NORMAL` cho throughput hoặc `FULL` cho durability cao;
- `busy_timeout`;
- `foreign_keys`;
- `cache_size`;
- checkpoint policy;
- tùy chọn mmap nếu benchmark chứng minh có lợi.

Không mặc định dùng `synchronous=OFF`.

Pool SQLite cần cấu hình rõ. SQLite chỉ có một writer thực tế; pool lớn không đồng nghĩa throughput write lớn. Có thể dùng pool nhỏ cho SQLite, trong khi PostgreSQL/MySQL dùng pool lớn hơn.

Sau full rebuild nên xem xét:

- `ANALYZE`;
- checkpoint WAL;
- `VACUUM` chỉ khi thật sự cần reclaim file, vì thao tác này có thể nặng.

## 10. Giai đoạn 6 — vector storage

`sg_embeddings` là persistence store; `sg_vss` là native ANN index khi extension khả dụng.

Kiểm tra các invariant:

- save embedding cập nhật cả hai nơi;
- clear embeddings xoá cả hai nơi;
- rebuild không để lại vector cũ;
- vector sai dimension bị reject;
- fallback brute-force trả semantics giống native ANN.

API chung chỉ nên mô tả capability:

```rust
enum VectorCapability {
    None,
    BruteForce,
    NativeAnn,
}
```

Không để interface chung phụ thuộc trực tiếp vào `sqlite-vss`.

## 11. Abstraction cho PostgreSQL/MySQL và database tương lai

`Storage` nên mô tả semantic operation, không mô tả SQL syntax.

Các khác biệt cần nằm trong backend:

- placeholder `?`/`$1`;
- upsert syntax;
- ID generation;
- batch insert;
- generated ID/`RETURNING`;
- transaction isolation;
- write concurrency;
- native vector search.

Có thể thêm capability object:

```rust
struct BackendCapabilities {
    batch_write: bool,
    batch_read: bool,
    native_upsert: bool,
    native_vector_search: bool,
    id_block_reservation: bool,
}
```

Các test về semantics dùng chung cho mọi backend; test về SQL plan, PRAGMA và extension chỉ nằm trong backend-specific tests.

## 12. Test plan

### Correctness

- ingest rồi reopen;
- rollback không để lại node/edge/stat;
- transaction commit atomic;
- concurrent ID allocation không trùng;
- batch read bằng single read;
- move child giữ đúng cấu trúc;
- stats/version khớp dữ liệu;
- vector round-trip và fallback.

### Performance

- statement count trước/sau batch write;
- số transaction trước/sau;
- ingest latency;
- read latency single/batch;
- lock wait;
- WAL size;
- database size;
- concurrent readers/writer.

### Compatibility

- SQLite contract tests;
- PostgreSQL contract tests;
- MySQL contract tests;
- schema migration/reopen với database cũ;
- feature build với từng backend.

## 13. Thứ tự thực hiện khuyến nghị

1. Thêm benchmark/instrumentation.
2. Bọc ingest entity writes trong một transaction semantic.
3. Thêm SQLite batch read.
4. Batch hóa `SqliteTx`.
5. Reserve ID theo block.
6. Kiểm tra và loại bỏ index dư thừa nếu có bằng chứng.
7. Tách hot columns khỏi BLOB JSON nếu query profile yêu cầu.
8. Đưa SQLite tuning vào configuration.
9. Chuẩn hóa capability và contract tests cho nhiều backend.
10. Tối ưu vector ANN sau khi persistence/write path ổn định.

Nguyên tắc xuyên suốt: mỗi thay đổi phải có benchmark trước/sau, query plan hoặc correctness test tương ứng; không tối ưu SQLite bằng cách làm rò rỉ SQLite-specific behavior vào abstraction chung.
