-- =============================================================================
-- codegraph-rs · storage migration 001 — initial schema (MySQL 8.0+)
-- =============================================================================
-- Cùng design với `sql/postgres/001-initial-schema.sql` — chỉ khác dialect.
--
-- LƯU Ý MySQL:
--   * DDL KHÔNG transactional — mỗi CREATE TABLE tự commit. Không bọc
--     BEGIN/COMMIT; chạy tuần tự theo thứ tự file.
--   * Không cho cột TEXT làm PRIMARY KEY / index toàn vẹn → mọi cột thuộc
--     khóa hoặc được index dùng `VARCHAR(700)` (đủ ngắn để nằm dưới giới hạn
--     index key 3072 bytes với utf8mb4, kể cả PK ghép với repo_id). Trường hợp
--     key dài hơn 700 ký tự → dùng hash key (md5/sha256) ở phase sau.
--   * `COLLATE utf8mb4_0900_as_cs` giữ so sánh case-sensitive (name/path là key
--     phân biệt hoa/thường) NHƯNG vẫn là charset utf8mb4 (không phải binary) —
--     sqlx decode VARCHAR/TEXT thành String được. Collations *_bin bị MySQL báo
--     về client dưới dạng VARBINARY nên sqlx từ chối decode thành String.
--   * id dùng BIGINT signed như sqlite hiện tại (u64 → i64, không đổi hành vi).
-- =============================================================================

-- ── Migration tracking (global) ──────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS schema_migrations (
    version    VARCHAR(64)  NOT NULL PRIMARY KEY,
    applied_at TIMESTAMP(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_as_cs;

-- ═══════════════════════════════════════════════════════════════════════════
-- Entity store (sg_*) — partition theo repo_id
-- ═══════════════════════════════════════════════════════════════════════════

-- Symbol — tương ứng `codegraph_core::Symbol` (annotations = Vec<Annotation>).
CREATE TABLE IF NOT EXISTS sg_symbols (
    repo_id     BIGINT       NOT NULL,             -- repository partition key (số u64)
    id          BIGINT       NOT NULL,             -- symbol id (registry global, ≥ 100)
    name        VARCHAR(700) NOT NULL DEFAULT '',
    kind        VARCHAR(32)  NOT NULL DEFAULT '',  -- SymbolKind: Function/Method/Class/...
    scope       VARCHAR(32)  NOT NULL DEFAULT '',  -- ScopeLevel: Global/ObjectField/Local/Parameter
    scope_id    BIGINT       NOT NULL DEFAULT 0,   -- id scope bao (0 = global)
    type_ref    BIGINT       NOT NULL DEFAULT 0,   -- id kiểu đã khai báo (0 = none)
    type_name   TEXT,                              -- raw type string, VD 'orderservice.OrderService'
    file        VARCHAR(700) NOT NULL DEFAULT '',
    line        INT          NOT NULL DEFAULT 0,
    end_line    INT          NOT NULL DEFAULT 0,
    signature   TEXT,
    doc         TEXT,
    annotations TEXT        NOT NULL,                 -- lưu JSON string (app luôn ghi giá trị nên không cần DEFAULT)
    language    VARCHAR(64)  NOT NULL DEFAULT '',
    PRIMARY KEY (repo_id, id),
    KEY idx_sg_symbols_repo_file (repo_id, file),
    KEY idx_sg_symbols_repo_name (repo_id, name)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_as_cs;

-- FileInfo — metadata file đã index.
-- `lines` được quote vì LINES là reserved word trong MySQL (LOAD DATA ... LINES).
CREATE TABLE IF NOT EXISTS sg_files (
    repo_id  BIGINT       NOT NULL,
    path     VARCHAR(700) NOT NULL,
    language VARCHAR(64)  NOT NULL DEFAULT '',
    bytes    BIGINT       NOT NULL DEFAULT 0,
    `lines`  INT          NOT NULL DEFAULT 0,
    PRIMARY KEY (repo_id, path)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_as_cs;

-- Call records của từng function — JSON bytes của `Vec<CallRecord>`.
CREATE TABLE IF NOT EXISTS sg_call_records (
    repo_id BIGINT NOT NULL,
    func    BIGINT      NOT NULL,                  -- caller symbol id
    records LONGBLOB    NOT NULL,                  -- serde_json bytes
    PRIMARY KEY (repo_id, func)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_as_cs;

-- Inverted index call name → call sites — JSON bytes của `Vec<CallSite>`.
CREATE TABLE IF NOT EXISTS sg_call_names (
    repo_id BIGINT  NOT NULL,
    name    VARCHAR(700) NOT NULL,                 -- tên call (lowercase)
    sites   LONGBLOB     NOT NULL,                 -- serde_json bytes
    PRIMARY KEY (repo_id, name)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_as_cs;

-- Version index của repo — `SharedGraphIndex::ensure_fresh` probe ở đây.
CREATE TABLE IF NOT EXISTS sg_meta (
    repo_id BIGINT NOT NULL,
    version BIGINT      NOT NULL DEFAULT 0,
    PRIMARY KEY (repo_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_as_cs;

-- Registry counter — symbol id tiếp theo (SYMBOL_BASE = 100).
CREATE TABLE IF NOT EXISTS sg_next_id (
    repo_id BIGINT NOT NULL,
    next    BIGINT      NOT NULL DEFAULT 100,      -- SYMBOL_BASE
    PRIMARY KEY (repo_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_as_cs;

-- ═══════════════════════════════════════════════════════════════════════════
-- Radix trie (rt_*) — partition theo repo_id, giữ nguyên sharding element % 64
-- ═══════════════════════════════════════════════════════════════════════════

-- Node của trie: prefix (bytes) + record (index key). id 0 = sentinel (EMPTY).
CREATE TABLE IF NOT EXISTS rt_nodes (
    repo_id BIGINT NOT NULL,
    id      BIGINT      NOT NULL,
    prefix  LONGBLOB    NOT NULL,
    record  BIGINT      NOT NULL DEFAULT 0,
    PRIMARY KEY (repo_id, id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_as_cs;

-- Cạnh cha-con của trie.
CREATE TABLE IF NOT EXISTS rt_children (
    repo_id BIGINT NOT NULL,
    parent  BIGINT      NOT NULL,
    child   BIGINT      NOT NULL,
    PRIMARY KEY (repo_id, parent, child),
    KEY idx_rt_children_repo_parent (repo_id, parent)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_as_cs;

-- Gốc mỗi shard: shard ∈ [0, 64). root = 0 nghĩa EMPTY — row tạo LAZY lần đầu
-- dùng shard (giống sqlite: get_root trả EMPTY khi thiếu row).
CREATE TABLE IF NOT EXISTS rt_roots (
    repo_id BIGINT NOT NULL,
    shard   INT         NOT NULL,
    root    BIGINT      NOT NULL DEFAULT 0,        -- EMPTY = 0
    PRIMARY KEY (repo_id, shard)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_as_cs;

-- Metadata opaque theo record (call-site info v.v.).
CREATE TABLE IF NOT EXISTS rt_meta (
    repo_id BIGINT NOT NULL,
    record  BIGINT      NOT NULL,
    meta    LONGBLOB,
    PRIMARY KEY (repo_id, record)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_as_cs;

-- Độ dài key (số element) theo record — filter depth trong search.
CREATE TABLE IF NOT EXISTS rt_keylen (
    repo_id BIGINT NOT NULL,
    record  BIGINT      NOT NULL,
    len     INT         NOT NULL,
    PRIMARY KEY (repo_id, record)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_as_cs;

-- Shortcut index (substring search): node có prefix chứa elem → ứng viên KMP.
CREATE TABLE IF NOT EXISTS rt_shortcuts (
    repo_id BIGINT NOT NULL,
    shard   INT         NOT NULL,
    elem    LONGBLOB    NOT NULL,
    node_id BIGINT      NOT NULL,
    PRIMARY KEY (repo_id, shard, elem(255), node_id),
    KEY idx_rt_shortcuts_repo_shard_elem (repo_id, shard, elem(255))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_as_cs;

-- Chain của function (record → bytes u64 LE mỗi element). Nguồn chân lý để
-- rebuild engine khi reopen (`GraphIndex::rebuild` → `all_chains()`).
CREATE TABLE IF NOT EXISTS rt_chains (
    repo_id BIGINT NOT NULL,
    record  BIGINT      NOT NULL,                  -- func id
    chain   LONGBLOB    NOT NULL,
    PRIMARY KEY (repo_id, record)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_as_cs;

-- Edge data stream (legacy — Storage trait còn giữ, GraphIndex chưa dùng).
CREATE TABLE IF NOT EXISTS rt_edges (
    repo_id BIGINT NOT NULL,
    id      BIGINT      NOT NULL,
    data    LONGBLOB,
    PRIMARY KEY (repo_id, id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_as_cs;

-- Node metadata stream (Node JSON theo element — legacy, chưa dùng).
CREATE TABLE IF NOT EXISTS rt_node_meta (
    repo_id BIGINT NOT NULL,
    elem    BIGINT      NOT NULL,
    meta    LONGBLOB,
    PRIMARY KEY (repo_id, elem)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_as_cs;

-- Bloom filter per node (feature `bloom-search`).
CREATE TABLE IF NOT EXISTS rt_node_blooms (
    repo_id BIGINT NOT NULL,
    id      BIGINT      NOT NULL,
    bloom   LONGBLOB,
    PRIMARY KEY (repo_id, id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_as_cs;

-- Node-id allocator (per repo — các shard dùng chung một dãy id như sqlite).
CREATE TABLE IF NOT EXISTS rt_counter (
    repo_id BIGINT NOT NULL,
    next    BIGINT      NOT NULL DEFAULT 1,
    PRIMARY KEY (repo_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_as_cs;

-- ═══════════════════════════════════════════════════════════════════════════
-- Pattern dùng chung (thực thi ở tầng storage — KHÔNG nằm trong migration,
-- vì repo_id là dữ liệu runtime từ config)
-- ═══════════════════════════════════════════════════════════════════════════
--
-- [Seed per repo] — lần đầu chạm repo, upsert idempotent (repo_id = số u64):
--   INSERT IGNORE INTO rt_nodes (repo_id, id, prefix, record) VALUES (?, 0, '', 0);
--   INSERT IGNORE INTO rt_counter (repo_id, next)  VALUES (?, 1);
--   INSERT IGNORE INTO sg_next_id (repo_id, next)  VALUES (?, 100);
--   INSERT IGNORE INTO sg_meta    (repo_id, version) VALUES (?, 0);
--
-- [Node id alloc] — atomic, per repo. `LAST_INSERT_ID(expr)` là connection-scoped;
-- idiom dưới giữ semantics GIỐNG PG/sqlite: id cấp = next cũ, rồi next += 1.
-- (KHÔNG dùng `LAST_INSERT_ID(next + 1)` — nó trả next MỚI, sai id vừa cấp.)
--   UPDATE rt_counter SET next = LAST_INSERT_ID(next) + 1 WHERE repo_id = ?;
--   SELECT LAST_INSERT_ID();                       -- = next cũ (id vừa cấp)
--
-- [Symbol registry id alloc]:
--   UPDATE sg_next_id SET next = LAST_INSERT_ID(next) + 1 WHERE repo_id = ?;
--   SELECT LAST_INSERT_ID();
--
-- [Upsert entity] — ví dụ sg_symbols:
--   INSERT INTO sg_symbols (repo_id, id, name, kind, scope, scope_id, type_ref,
--                           type_name, file, line, end_line, signature, doc,
--                           annotations, language)
--   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
--   ON DUPLICATE KEY UPDATE
--       name = VALUES(name), kind = VALUES(kind), ...,
--       annotations = VALUES(annotations), language = VALUES(language);
--
-- [Probe version] — `SharedGraphIndex::current_version`:
--   SELECT version FROM sg_meta WHERE repo_id = ?;
--
-- [Full re-index (clear)] — xoá toàn bộ data repo rồi ingest lại:
--   DELETE FROM sg_symbols      WHERE repo_id = ?;
--   DELETE FROM sg_files        WHERE repo_id = ?;
--   DELETE FROM sg_call_records WHERE repo_id = ?;
--   DELETE FROM sg_call_names   WHERE repo_id = ?;
--   DELETE FROM rt_nodes        WHERE repo_id = ?;
--   DELETE FROM rt_children     WHERE repo_id = ?;
--   DELETE FROM rt_roots        WHERE repo_id = ?;
--   DELETE FROM rt_meta         WHERE repo_id = ?;
--   DELETE FROM rt_keylen       WHERE repo_id = ?;
--   DELETE FROM rt_shortcuts    WHERE repo_id = ?;
--   DELETE FROM rt_chains       WHERE repo_id = ?;
--   DELETE FROM rt_edges        WHERE repo_id = ?;
--   DELETE FROM rt_node_meta    WHERE repo_id = ?;
--   DELETE FROM rt_node_blooms  WHERE repo_id = ?;
--   UPDATE rt_counter  SET next = 1   WHERE repo_id = ?;
--   UPDATE sg_next_id  SET next = 100 WHERE repo_id = ?;
--   UPDATE sg_meta     SET version = 0   WHERE repo_id = ?;
-- ═══════════════════════════════════════════════════════════════════════════
