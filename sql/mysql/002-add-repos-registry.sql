-- =============================================================================
-- codegraph-rs · storage migration 002 — repos registry (global mapping) (MySQL)
-- =============================================================================
-- Cùng design với `sql/postgres/002-add-repos-registry.sql` — chỉ khác dialect.
--
-- Bảng mapping repo_id → shard — phần "quản lý mapping" của thiết kế sharding.
-- GLOBAL: KHÔNG partition theo repo_id, và được NHÂN BẢN trên MỌI shard server
-- (mỗi shard giữ bản sao đầy đủ) — bất kỳ instance nào cũng tra được repo thuộc
-- shard nào mà không cần biết trước điểm tra.
--
-- Vai trò:
--   * `shard` = chỉ mục vào `dsns` của shard server ĐƯỢC GÁN. Gán ĐÚNG MỘT LẦN
--     lúc đăng ký (lần chạm DB đầu tiên), mọi open sau ĐỌC từ bảng này — KHÔNG
--     recompute `repo_id % N`. Đổi số lượng DSN không làm repo dịch server
--     (dữ liệu không mất; chỉ repo MỚI tính theo N mới).
--   * `root` = root path chuẩn → lookup ngược: cùng root path (clone/máy khác)
--     nhận CÙNG repo_id → dùng chung partition trong DB.
--
-- LƯU Ý DIALECT:
--   * MySQL không hỗ trợ `CREATE INDEX IF NOT EXISTS` — an toàn vì migration
--     runner chạy MỘT LẦN per server (track theo `schema_migrations`).
--   * root dùng VARCHAR(700) (giới hạn index key utf8mb4 3072 bytes — xem 001).
--
-- Quy trình (thực thi ở tầng storage/repo resolver — KHÔNG nằm trong migration):
--   [Lookup by repo_id] — đã biết repo_id (config): đọc ở shard `repo_id % N`:
--     SELECT shard FROM repos WHERE repo_id = ?;
--   [Adopt by root] — config thiếu repo_id: đọc ở bất kỳ shard (chuẩn: shard 0):
--     SELECT repo_id, shard FROM repos WHERE root = ?;
--   [Register] — repo mới: gán shard = repo_id % N, ghi vào MỌI shard (idempotent):
--     INSERT INTO repos (repo_id, shard, root) VALUES (?, ?, ?)
--       ON DUPLICATE KEY UPDATE repo_id = repo_id;   -- no-op nếu đã tồn tại
--     -- lặp lại cho từng shard server
-- =============================================================================

CREATE TABLE IF NOT EXISTS repos (
    repo_id    BIGINT       NOT NULL PRIMARY KEY,     -- repo_id (số u64, random lúc init)
    shard      INT          NOT NULL,                 -- shard server được gán (index vào dsns)
    root       VARCHAR(700) NOT NULL,                 -- root path chuẩn để lookup
    created_at TIMESTAMP(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin;
-- Lookup theo root (adopt cùng repo_id cho clone/máy khác).
CREATE INDEX idx_repos_root ON repos (root);