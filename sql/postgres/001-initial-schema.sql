-- =============================================================================
-- codegraph-rs · storage migration 001 — initial schema (PostgreSQL)
-- =============================================================================
-- Quy ước migration: thư mục `sql/postgres/`, mỗi file đặt tên
-- `NNN-<tên thay đổi>.sql` (001-, 002-, ...). Áp dụng theo thứ tự số; KHÔNG sửa
-- file đã apply — thay đổi mới phải là file kế tiếp. Bảng `schema_migrations`
-- ghi lại version đã chạy (nền cho migration runner ở phase code).
--
-- Thiết kế (chi tiết xem sql/README.md):
--   * Mọi bảng dữ liệu dẫn đầu bằng `repo_id` (SỐ u64, sinh ngẫu nhiên lúc
--     `codegraph init`, lưu `.codegraph/config.toml`) — 1 repository = 1 partition.
--     PK composite `(repo_id, ...)`. Re-index / xoá repo = `DELETE WHERE repo_id = ?`.
--     Shard server = `repo_id % số_lượng_dsn` (xem mục sharding trong README).
--   * `sg_*` = entity store (cột thật — Symbol/FileInfo/CallRecord/CallSite,
--     phục vụ query SQL trực tiếp + sẵn sàng cho lakehouse/parquet).
--   * `rt_*` = radix trie (dữ liệu nhị phân — prefix/chain/shortcut/meta),
--     giữ nguyên cơ chế sharding hiện tại (CHAIN_SHARDING = 64,
--     `shard_of(elem) = elem % 64`). Không có lợi ích relational nên giữ cột
--     bytea; vẫn partition theo repo_id như mọi bảng khác.
--   * Migration (schema) là GLOBAL — không partition theo repo.
-- =============================================================================

BEGIN;

-- ── Migration tracking (global) ──────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS schema_migrations (
    version    VARCHAR(64) NOT NULL PRIMARY KEY,   -- tên file, VD '001-initial-schema'
    applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ═══════════════════════════════════════════════════════════════════════════
-- Entity store (sg_*) — partition theo repo_id
-- ═══════════════════════════════════════════════════════════════════════════

-- Symbol — tương ứng `codegraph_core::Symbol` (annotations = Vec<Annotation>).
CREATE TABLE IF NOT EXISTS sg_symbols (
    repo_id     BIGINT NOT NULL,                 -- repository partition key (số u64)
    id          BIGINT      NOT NULL,              -- symbol id (registry global, ≥ 100)
    name        TEXT        NOT NULL DEFAULT '',
    kind        VARCHAR(32) NOT NULL DEFAULT '',   -- SymbolKind: Function/Method/Class/...
    scope       VARCHAR(32) NOT NULL DEFAULT '',   -- ScopeLevel: Global/ObjectField/Local/Parameter
    scope_id    BIGINT      NOT NULL DEFAULT 0,    -- id scope bao (0 = global)
    type_ref    BIGINT      NOT NULL DEFAULT 0,    -- id kiểu đã khai báo (0 = none)
    type_name   TEXT,                              -- raw type string, VD 'orderservice.OrderService'
    file        TEXT        NOT NULL DEFAULT '',
    line        INTEGER     NOT NULL DEFAULT 0,
    end_line    INTEGER     NOT NULL DEFAULT 0,
    signature   TEXT,
    doc         TEXT,
    annotations JSONB       NOT NULL DEFAULT '[]'::jsonb,
    language    TEXT        NOT NULL DEFAULT '',
    PRIMARY KEY (repo_id, id)
);
-- File filter (tool list theo root/file) + name lookup convenience.
CREATE INDEX IF NOT EXISTS idx_sg_symbols_repo_file ON sg_symbols (repo_id, file);
CREATE INDEX IF NOT EXISTS idx_sg_symbols_repo_name ON sg_symbols (repo_id, name);

-- FileInfo — metadata file đã index.
CREATE TABLE IF NOT EXISTS sg_files (
    repo_id  BIGINT      NOT NULL,
    path     TEXT        NOT NULL,
    language TEXT        NOT NULL DEFAULT '',
    bytes    BIGINT      NOT NULL DEFAULT 0,
    lines    INTEGER     NOT NULL DEFAULT 0,
    PRIMARY KEY (repo_id, path)
);

-- Call records của từng function — JSON bytes của `Vec<CallRecord>`.
CREATE TABLE IF NOT EXISTS sg_call_records (
    repo_id BIGINT NOT NULL,
    func    BIGINT      NOT NULL,                  -- caller symbol id
    records BYTEA       NOT NULL,                  -- serde_json bytes
    PRIMARY KEY (repo_id, func)
);

-- Inverted index call name → call sites — JSON bytes của `Vec<CallSite>`.
CREATE TABLE IF NOT EXISTS sg_call_names (
    repo_id BIGINT NOT NULL,
    name    TEXT        NOT NULL,                  -- tên call (lowercase)
    sites   BYTEA       NOT NULL,                  -- serde_json bytes
    PRIMARY KEY (repo_id, name)
);

-- Version index của repo — `SharedGraphIndex::ensure_fresh` probe ở đây
-- (mỗi full re-index bump version → snapshot in-memory cũ thấy stale, rebuild).
CREATE TABLE IF NOT EXISTS sg_meta (
    repo_id BIGINT NOT NULL,
    version BIGINT      NOT NULL DEFAULT 0,
    PRIMARY KEY (repo_id)
);

-- Registry counter — symbol id tiếp theo (SYMBOL_BASE = 100).
CREATE TABLE IF NOT EXISTS sg_next_id (
    repo_id BIGINT NOT NULL,
    next    BIGINT      NOT NULL DEFAULT 100,      -- SYMBOL_BASE
    PRIMARY KEY (repo_id)
);

-- ═══════════════════════════════════════════════════════════════════════════
-- Radix trie (rt_*) — partition theo repo_id, giữ nguyên sharding element % 64
-- ═══════════════════════════════════════════════════════════════════════════

-- Node của trie: prefix (bytes) + record (index key). id 0 = sentinel (EMPTY).
CREATE TABLE IF NOT EXISTS rt_nodes (
    repo_id BIGINT NOT NULL,
    id      BIGINT      NOT NULL,
    prefix  BYTEA       NOT NULL,
    record  BIGINT      NOT NULL DEFAULT 0,
    PRIMARY KEY (repo_id, id)
);

-- Cạnh cha-con của trie.
CREATE TABLE IF NOT EXISTS rt_children (
    repo_id BIGINT NOT NULL,
    parent  BIGINT      NOT NULL,
    child   BIGINT      NOT NULL,
    PRIMARY KEY (repo_id, parent, child)
);
-- Truy vấn children của một node — theo (repo_id, parent).
CREATE INDEX IF NOT EXISTS idx_rt_children_repo_parent ON rt_children (repo_id, parent);

-- Gốc mỗi shard: shard ∈ [0, 64). root = 0 nghĩa EMPTY — row tạo LAZY lần đầu
-- dùng shard (giống sqlite: get_root trả EMPTY khi thiếu row).
CREATE TABLE IF NOT EXISTS rt_roots (
    repo_id BIGINT NOT NULL,
    shard   INTEGER     NOT NULL,
    root    BIGINT      NOT NULL DEFAULT 0,        -- EMPTY = 0
    PRIMARY KEY (repo_id, shard)
);

-- Metadata opaque theo record (call-site info v.v.).
CREATE TABLE IF NOT EXISTS rt_meta (
    repo_id BIGINT NOT NULL,
    record  BIGINT      NOT NULL,
    meta    BYTEA,
    PRIMARY KEY (repo_id, record)
);

-- Độ dài key (số element) theo record — filter depth trong search.
CREATE TABLE IF NOT EXISTS rt_keylen (
    repo_id BIGINT NOT NULL,
    record  BIGINT      NOT NULL,
    len     INTEGER     NOT NULL,
    PRIMARY KEY (repo_id, record)
);

-- Shortcut index (substring search): node có prefix chứa elem → ứng viên KMP.
CREATE TABLE IF NOT EXISTS rt_shortcuts (
    repo_id BIGINT NOT NULL,
    shard   INTEGER     NOT NULL,
    elem    BYTEA       NOT NULL,
    node_id BIGINT      NOT NULL,
    PRIMARY KEY (repo_id, shard, elem, node_id)
);
-- Lookup: tập node id chứa elem trong một shard.
CREATE INDEX IF NOT EXISTS idx_rt_shortcuts_repo_shard_elem
    ON rt_shortcuts (repo_id, shard, elem);

-- Chain của function (record → bytes u64 LE mỗi element). Nguồn chân lý để
-- rebuild engine khi reopen (`GraphIndex::rebuild` → `all_chains()`).
CREATE TABLE IF NOT EXISTS rt_chains (
    repo_id BIGINT NOT NULL,
    record  BIGINT      NOT NULL,                  -- func id
    chain   BYTEA       NOT NULL,
    PRIMARY KEY (repo_id, record)
);

-- Edge data stream (legacy — Storage trait còn giữ, GraphIndex chưa dùng).
CREATE TABLE IF NOT EXISTS rt_edges (
    repo_id BIGINT NOT NULL,
    id      BIGINT      NOT NULL,
    data    BYTEA,
    PRIMARY KEY (repo_id, id)
);

-- Node metadata stream (Node JSON theo element — legacy, chưa dùng).
CREATE TABLE IF NOT EXISTS rt_node_meta (
    repo_id BIGINT NOT NULL,
    elem    BIGINT      NOT NULL,
    meta    BYTEA,
    PRIMARY KEY (repo_id, elem)
);

-- Bloom filter per node (feature `bloom-search`).
CREATE TABLE IF NOT EXISTS rt_node_blooms (
    repo_id BIGINT NOT NULL,
    id      BIGINT      NOT NULL,
    bloom   BYTEA,
    PRIMARY KEY (repo_id, id)
);

-- Node-id allocator (per repo — các shard dùng chung một dãy id như sqlite).
CREATE TABLE IF NOT EXISTS rt_counter (
    repo_id BIGINT NOT NULL,
    next    BIGINT      NOT NULL DEFAULT 1,
    PRIMARY KEY (repo_id)
);

-- ═══════════════════════════════════════════════════════════════════════════
-- Pattern dùng chung (thực thi ở tầng storage — KHÔNG nằm trong migration,
-- vì repo_id là dữ liệu runtime từ config)
-- ═══════════════════════════════════════════════════════════════════════════
--
-- [Seed per repo] — lần đầu chạm repo, upsert idempotent (repo_id = số u64):
--   INSERT INTO rt_nodes (repo_id, id, prefix, record) VALUES ($1, 0, '', 0)
--       ON CONFLICT DO NOTHING;
--   INSERT INTO rt_counter (repo_id, next)  VALUES ($1, 1)   ON CONFLICT DO NOTHING;
--   INSERT INTO sg_next_id (repo_id, next)  VALUES ($1, 100) ON CONFLICT DO NOTHING;
--   INSERT INTO sg_meta    (repo_id, version) VALUES ($1, 0) ON CONFLICT DO NOTHING;
--
-- [Node id alloc] — atomic, per repo:
--   UPDATE rt_counter SET next = next + 1 WHERE repo_id = $1 RETURNING next - 1;
--
-- [Symbol registry id alloc]:
--   UPDATE sg_next_id SET next = next + 1 WHERE repo_id = $1 RETURNING next - 1;
--
-- [Upsert entity] — ví dụ sg_symbols:
--   INSERT INTO sg_symbols (repo_id, id, name, kind, scope, scope_id, type_ref,
--                           type_name, file, line, end_line, signature, doc,
--                           annotations, language)
--   VALUES ($1, ..., $15)
--   ON CONFLICT (repo_id, id) DO UPDATE SET
--       name = $3, kind = $4, ..., annotations = $14, language = $15;
--
-- [Probe version] — `SharedGraphIndex::current_version`:
--   SELECT version FROM sg_meta WHERE repo_id = $1;
--
-- [Full re-index (clear)] — xoá toàn bộ data repo rồi ingest lại:
--   DELETE FROM sg_symbols      WHERE repo_id = $1;
--   DELETE FROM sg_files        WHERE repo_id = $1;
--   DELETE FROM sg_call_records WHERE repo_id = $1;
--   DELETE FROM sg_call_names   WHERE repo_id = $1;
--   DELETE FROM rt_nodes        WHERE repo_id = $1;
--   DELETE FROM rt_children     WHERE repo_id = $1;
--   DELETE FROM rt_roots        WHERE repo_id = $1;
--   DELETE FROM rt_meta         WHERE repo_id = $1;
--   DELETE FROM rt_keylen       WHERE repo_id = $1;
--   DELETE FROM rt_shortcuts    WHERE repo_id = $1;
--   DELETE FROM rt_chains       WHERE repo_id = $1;
--   DELETE FROM rt_edges        WHERE repo_id = $1;
--   DELETE FROM rt_node_meta    WHERE repo_id = $1;
--   DELETE FROM rt_node_blooms  WHERE repo_id = $1;
--   UPDATE rt_counter  SET next = 1   WHERE repo_id = $1;
--   UPDATE sg_next_id  SET next = 100 WHERE repo_id = $1;
--   UPDATE sg_meta     SET version = 0   WHERE repo_id = $1;
-- ═══════════════════════════════════════════════════════════════════════════

COMMIT;
