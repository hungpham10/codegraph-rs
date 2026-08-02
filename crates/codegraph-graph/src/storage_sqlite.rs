//! SQLite-backed Storage implementation (Radix + Automaton).
//!
//! Implement `Storage` trait trên SQLite để `SearchIndex`/`RadixTree` chạy được
//! trên cùng engine SQLite với phần còn lại của codegraph — không cần Redis.
//!
//! ## Bảng dữ liệu
//!
//! | Bảng                 | Mục đích                                     |
//! |----------------------|----------------------------------------------|
//! | `rt_nodes`           | (id, prefix BLOB, record) — node radix       |
//! | `rt_children`        | (parent, child) — danh sách children         |
//! | `rt_roots`           | (shard, root_id) — root mỗi shard            |
//! | `rt_entries`         | (idx, entry_id, name, meta) — record payload |
//! | `rt_blobs`           | (k, v) — generic binary blobs                |
//! | `rt_counter`         | atomic record counter                        |
//! | automaton tables     | rt_states / rt_transitions / rt_failure / rt_output / rt_root_inputs |
//!
//! Lưu ý: `save_shard`/`load_shard` giữ default (no-op) — `SearchIndex::reload`
//! sẽ fallback qua DFS collect (không cần shard blob cho PoC).
//!
//! Node id 0 là sentinel (giống `InMemoryStorage`/`RedisStorage`), node thật bắt
//! đầu từ 1.

use std::sync::Mutex;

use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};

use crate::storage::{Result, Storage, StorageError};

/// Node sentinel (giống `storage::EMPTY`).
const EMPTY: usize = 0;

pub struct SqliteStorage {
    conn: Mutex<Connection>,
}

impl SqliteStorage {
    /// Mở (hoặc tạo) SQLite file.
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path).map_err(Self::sql_err)?;
        Self::init(conn)
    }

    /// Storage trong bộ nhớ (`:memory:`) — dùng cho test/benchmark.
    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory().map_err(Self::sql_err)?)
    }

    /// Xoá toàn bộ dữ liệu (giữ schema). Dùng khi rebuild index.
    pub fn clear(&mut self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            r#"
            DELETE FROM rt_nodes;
            DELETE FROM rt_children;
            DELETE FROM rt_roots;
            DELETE FROM rt_entries;
            DELETE FROM rt_blobs;
            DELETE FROM rt_counter;
            DELETE FROM rt_states;
            DELETE FROM rt_transitions;
            DELETE FROM rt_failure;
            DELETE FROM rt_output;
            DELETE FROM rt_root_inputs;
            INSERT INTO rt_nodes (id, prefix, record) VALUES (0, x'', 0);
            "#,
        )
        .map_err(Self::sql_err)?;
        Ok(())
    }

    fn init(conn: Connection) -> Result<Self> {
        // WAL: không hỗ trợ trên :memory:, ignore lỗi. synchronous=NORMAL để
        // transaction insert rẻ (WAL checkpoint) nhưng vẫn an toàn crash.
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "synchronous", "NORMAL").ok();
        conn.pragma_update(None, "busy_timeout", 5000).ok();
        conn.pragma_update(None, "foreign_keys", "OFF").ok();

        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS rt_nodes (
                id     INTEGER PRIMARY KEY,
                prefix BLOB NOT NULL,
                record INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS rt_children (
                parent INTEGER NOT NULL,
                child  INTEGER NOT NULL,
                PRIMARY KEY (parent, child)
            );
            CREATE INDEX IF NOT EXISTS idx_rt_children_child ON rt_children (child);
            CREATE TABLE IF NOT EXISTS rt_roots (
                shard   INTEGER PRIMARY KEY,
                root_id INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS rt_entries (
                idx      INTEGER PRIMARY KEY,
                entry_id INTEGER NOT NULL,
                name     TEXT NOT NULL,
                meta     BLOB
            );
            CREATE TABLE IF NOT EXISTS rt_blobs (
                k TEXT PRIMARY KEY,
                v BLOB NOT NULL
            );
            CREATE TABLE IF NOT EXISTS rt_counter (
                id  INTEGER PRIMARY KEY CHECK (id = 1),
                val INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS rt_states (
                id    INTEGER PRIMARY KEY,
                label TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS rt_transitions (
                state INTEGER NOT NULL,
                label TEXT NOT NULL,
                "to"  INTEGER NOT NULL,
                PRIMARY KEY (state, label)
            );
            CREATE TABLE IF NOT EXISTS rt_failure (
                state INTEGER PRIMARY KEY,
                fail  INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS rt_output (
                state   INTEGER PRIMARY KEY,
                pattern INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS rt_root_inputs (
                state INTEGER PRIMARY KEY
            );
            INSERT OR IGNORE INTO rt_nodes (id, prefix, record) VALUES (0, x'', 0);
            "#,
        )
        .map_err(Self::sql_err)?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn sql_err(e: rusqlite::Error) -> StorageError {
        StorageError::Internal(format!("sqlite: {e}"))
    }
}

#[async_trait]
impl Storage for SqliteStorage {
    // ==================== Radix Methods ====================

    async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO rt_nodes (prefix, record) VALUES (?1, ?2)",
            params![prefix, record as i64],
        )
        .map_err(Self::sql_err)?;
        Ok(conn.last_insert_rowid() as usize)
    }

    async fn update_node(
        &mut self,
        id: usize,
        prefix: Option<Vec<u8>>,
        record: Option<usize>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        match (prefix, record) {
            (Some(p), Some(r)) => {
                conn.execute(
                    "UPDATE rt_nodes SET prefix = ?1, record = ?2 WHERE id = ?3",
                    params![p, r as i64, id as i64],
                )
            }
            (Some(p), None) => conn.execute(
                "UPDATE rt_nodes SET prefix = ?1 WHERE id = ?2",
                params![p, id as i64],
            ),
            (None, Some(r)) => conn.execute(
                "UPDATE rt_nodes SET record = ?1 WHERE id = ?2",
                params![r as i64, id as i64],
            ),
            (None, None) => return Ok(()),
        }
        .map_err(Self::sql_err)?;
        Ok(())
    }

    async fn add_child(&mut self, parent_id: usize, child_id: usize) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO rt_children (parent, child) VALUES (?1, ?2)",
            params![parent_id as i64, child_id as i64],
        )
        .map_err(Self::sql_err)?;
        Ok(())
    }

    async fn clear_children(&mut self, parent_id: usize) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM rt_children WHERE parent = ?1",
            params![parent_id as i64],
        )
        .map_err(Self::sql_err)?;
        Ok(())
    }

    async fn remove_child(&mut self, parent_id: usize, child_id: usize) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM rt_children WHERE parent = ?1 AND child = ?2",
            params![parent_id as i64, child_id as i64],
        )
        .map_err(Self::sql_err)?;
        Ok(())
    }

    /// Atomic split commit: xoá children cũ + update prefix/record trong cùng
    /// SAVEPOINT, đảm bảo crash không để lại tree không navigate được.
    /// SAVEPOINT (không BEGIN) để hoạt động cả khi đang trong `begin_bulk`.
    async fn commit_split(
        &mut self,
        parent: usize,
        root_prefix: Vec<u8>,
        new_record: usize,
        children_to_remove: &[usize],
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.savepoint().map_err(Self::sql_err)?;
        for &child in children_to_remove {
            tx.execute(
                "DELETE FROM rt_children WHERE parent = ?1 AND child = ?2",
                params![parent as i64, child as i64],
            )
            .map_err(Self::sql_err)?;
        }
        tx.execute(
            "UPDATE rt_nodes SET prefix = ?1, record = ?2 WHERE id = ?3",
            params![root_prefix, new_record as i64, parent as i64],
        )
        .map_err(Self::sql_err)?;
        tx.commit().map_err(Self::sql_err)?;
        Ok(())
    }

    async fn get_node(&self, id: usize) -> Result<(Vec<u8>, usize)> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT prefix, record FROM rt_nodes WHERE id = ?1")
            .map_err(Self::sql_err)?;
        stmt.query_row(params![id as i64], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)? as usize))
        })
        .optional()
        .map_err(Self::sql_err)?
        .ok_or(StorageError::BranchOutOfRange(id))
    }

    async fn get_children(&self, id: usize) -> Result<Vec<usize>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT child FROM rt_children WHERE parent = ?1 ORDER BY child")
            .map_err(Self::sql_err)?;
        let rows = stmt
            .query_map(params![id as i64], |row| row.get::<_, i64>(0))
            .map_err(Self::sql_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(Self::sql_err)? as usize);
        }
        Ok(out)
    }

    /// Batch: children + prefix + record trong 1 JOIN — dùng cho walk-down của
    /// prefix search (tránh O(fanout) `get_node` riêng lẻ mỗi level).
    async fn get_children_with_prefixes(&self, id: usize) -> Result<Vec<(usize, Vec<u8>, usize)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached(
                "SELECT c.child, n.prefix, n.record
                 FROM rt_children c
                 JOIN rt_nodes n ON n.id = c.child
                 WHERE c.parent = ?1
                 ORDER BY c.child",
            )
            .map_err(Self::sql_err)?;
        let rows = stmt
            .query_map(params![id as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)? as usize,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)? as usize,
                ))
            })
            .map_err(Self::sql_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(Self::sql_err)?);
        }
        Ok(out)
    }

    /// Scan toàn bộ subtree trong MỘT recursive CTE — thay cho DFS từng node.
    /// Root (node_id) có parent = NULL. Thứ tự row không đảm bảo — caller tái
    /// dựng cây trong bộ nhớ (sort children theo id) trước khi dựng key.
    async fn scan_subtree(
        &self,
        node_id: usize,
    ) -> Result<Vec<(Option<usize>, usize, Vec<u8>, usize)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached(
                "WITH RECURSIVE sub(parent, child, prefix, record) AS (
                    SELECT NULL, id, prefix, record FROM rt_nodes WHERE id = ?1
                    UNION ALL
                    SELECT c.parent, n.id, n.prefix, n.record
                    FROM rt_children c
                    JOIN rt_nodes n ON n.id = c.child
                    JOIN sub s ON s.child = c.parent
                 )
                 SELECT parent, child, prefix, record FROM sub",
            )
            .map_err(Self::sql_err)?;
        let rows = stmt
            .query_map(params![node_id as i64], |row| {
                let parent: Option<i64> = row.get(0)?;
                Ok((
                    parent.map(|p| p as usize),
                    row.get::<_, i64>(1)? as usize,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, i64>(3)? as usize,
                ))
            })
            .map_err(Self::sql_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(Self::sql_err)?);
        }
        Ok(out)
    }

    async fn set_root(&mut self, shard: usize, root_id: usize) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO rt_roots (shard, root_id) VALUES (?1, ?2)",
            params![shard as i64, root_id as i64],
        )
        .map_err(Self::sql_err)?;
        Ok(())
    }

    async fn get_root(&self, shard: usize) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT root_id FROM rt_roots WHERE shard = ?1")
            .map_err(Self::sql_err)?;
        let root: Option<i64> = stmt
            .query_row(params![shard as i64], |row| row.get(0))
            .optional()
            .map_err(Self::sql_err)?;
        Ok(root.unwrap_or(EMPTY as i64) as usize)
    }

    // ── Persistence for reload ──

    async fn save_entries(&mut self, entries: &[(i32, String)]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        // SAVEPOINT thay vì transaction: an toàn khi đang nằm trong `begin_bulk`
        // (SQLite không cho BEGIN lồng nhau, nhưng SAVEPOINT luôn hợp lệ).
        let tx = conn.savepoint().map_err(Self::sql_err)?;
        tx.execute("DELETE FROM rt_entries", []).map_err(Self::sql_err)?;
        for (i, (eid, name)) in entries.iter().enumerate() {
            tx.execute(
                "INSERT INTO rt_entries (idx, entry_id, name, meta) VALUES (?1, ?2, ?3, NULL)",
                params![(i + 1) as i64, eid, name],
            )
            .map_err(Self::sql_err)?;
        }
        tx.commit().map_err(Self::sql_err)?;
        Ok(())
    }

    async fn load_entries(&self) -> Result<Vec<(i32, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT entry_id, name FROM rt_entries ORDER BY idx")
            .map_err(Self::sql_err)?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, i32>(0)?, row.get::<_, String>(1)?)))
            .map_err(Self::sql_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(Self::sql_err)?);
        }
        Ok(out)
    }

    async fn load_entry(&self, idx: usize) -> Result<(i32, String)> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT entry_id, name FROM rt_entries WHERE idx = ?1")
            .map_err(Self::sql_err)?;
        stmt.query_row(params![idx as i64], |row| {
            Ok((row.get::<_, i32>(0)?, row.get::<_, String>(1)?))
        })
        .optional()
        .map_err(Self::sql_err)?
        .ok_or_else(|| StorageError::Internal(format!("entry at index {idx} not found")))
    }

    async fn save_entry(&mut self, idx: usize, entry_id: i32, name: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        // ON CONFLICT chỉ update entry_id/name — meta được giữ nguyên.
        conn.execute(
            "INSERT INTO rt_entries (idx, entry_id, name, meta) VALUES (?1, ?2, ?3, NULL)
             ON CONFLICT(idx) DO UPDATE SET entry_id = excluded.entry_id, name = excluded.name",
            params![idx as i64, entry_id, name],
        )
        .map_err(Self::sql_err)?;
        Ok(())
    }

    async fn save_entry_meta(&mut self, idx: usize, meta: &[u8]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO rt_entries (idx, entry_id, name, meta) VALUES (?1, 0, '', ?2)
             ON CONFLICT(idx) DO UPDATE SET meta = excluded.meta",
            params![idx as i64, meta],
        )
        .map_err(Self::sql_err)?;
        Ok(())
    }

    async fn load_entry_meta(&self, idx: usize) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT meta FROM rt_entries WHERE idx = ?1")
            .map_err(Self::sql_err)?;
        let res: Option<Option<Vec<u8>>> = stmt
            .query_row(params![idx as i64], |row| row.get(0))
            .optional()
            .map_err(Self::sql_err)?;
        Ok(res.flatten())
    }

    async fn count_entries(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT COUNT(*) FROM rt_entries")
            .map_err(Self::sql_err)?;
        let n: i64 = stmt.query_row([], |r| r.get(0)).map_err(Self::sql_err)?;
        Ok(n as usize)
    }

    /// Atomic record ID allocation — transaction + UPSERT (1,2,3,...).
    /// Dùng SAVEPOINT (không phải BEGIN) để hoạt động cả trong `begin_bulk`.
    async fn allocate_record_id(&mut self) -> Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.savepoint().map_err(Self::sql_err)?;
        tx.execute(
            "INSERT INTO rt_counter (id, val) VALUES (1, 1)
             ON CONFLICT(id) DO UPDATE SET val = val + 1",
            [],
        )
        .map_err(Self::sql_err)?;
        let val: i64 = tx
            .query_row("SELECT val FROM rt_counter WHERE id = 1", [], |r| r.get(0))
            .map_err(Self::sql_err)?;
        tx.commit().map_err(Self::sql_err)?;
        Ok(val as usize)
    }

    /// Khởi tạo counter — chỉ set nếu chưa tồn tại (giống Redis `SET NX`).
    async fn init_record_counter(&mut self, count: usize) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO rt_counter (id, val) VALUES (1, ?1)",
            params![count as i64],
        )
        .map_err(Self::sql_err)?;
        Ok(())
    }

    // ── Generic blob storage ──

    async fn save_blob(&mut self, key: &str, data: &[u8]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO rt_blobs (k, v) VALUES (?1, ?2)",
            params![key, data],
        )
        .map_err(Self::sql_err)?;
        Ok(())
    }

    async fn load_blob(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT v FROM rt_blobs WHERE k = ?1")
            .map_err(Self::sql_err)?;
        stmt.query_row(params![key], |row| row.get(0))
            .optional()
            .map_err(Self::sql_err)
    }

    // ── Bulk write mode ──

    /// Mở transaction bao phủ nhiều insert — cắt chi phí autocommit per-write
    /// khi rebuild. Mọi `commit_split`/`savepoint` bên trong vẫn hoạt động
    /// (SAVEPOINT lồng nhau), toàn bộ được COMMIT ở `end_bulk`.
    async fn begin_bulk(&mut self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch("BEGIN").map_err(Self::sql_err)?;
        Ok(())
    }

    async fn end_bulk(&mut self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch("COMMIT").map_err(Self::sql_err)?;
        Ok(())
    }

    // ==================== Automaton Methods ====================

    async fn add_state(&mut self, label: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        conn.execute("INSERT INTO rt_states (label) VALUES (?1)", params![label])
            .map_err(Self::sql_err)?;
        Ok(conn.last_insert_rowid() as usize)
    }

    async fn set_transition(&mut self, from: usize, label: &str, to: usize) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO rt_transitions (state, label, \"to\") VALUES (?1, ?2, ?3)",
            params![from as i64, label, to as i64],
        )
        .map_err(Self::sql_err)?;
        Ok(())
    }

    async fn get_transitions(&self, from: usize) -> Result<Vec<(String, usize)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT label, \"to\" FROM rt_transitions WHERE state = ?1")
            .map_err(Self::sql_err)?;
        let rows = stmt
            .query_map(params![from as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize))
            })
            .map_err(Self::sql_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(Self::sql_err)?);
        }
        Ok(out)
    }

    async fn set_failure(&mut self, state: usize, fail: usize) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO rt_failure (state, fail) VALUES (?1, ?2)",
            params![state as i64, fail as i64],
        )
        .map_err(Self::sql_err)?;
        Ok(())
    }

    async fn get_failure(&self, state: usize) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT fail FROM rt_failure WHERE state = ?1")
            .map_err(Self::sql_err)?;
        let v: Option<i64> = stmt
            .query_row(params![state as i64], |r| r.get(0))
            .optional()
            .map_err(Self::sql_err)?;
        Ok(v.unwrap_or(0) as usize)
    }

    async fn set_output(&mut self, state: usize, pattern_idx: usize) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO rt_output (state, pattern) VALUES (?1, ?2)",
            params![state as i64, pattern_idx as i64],
        )
        .map_err(Self::sql_err)?;
        Ok(())
    }

    async fn get_output(&self, state: usize) -> Result<Option<usize>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT pattern FROM rt_output WHERE state = ?1")
            .map_err(Self::sql_err)?;
        let v: Option<i64> = stmt
            .query_row(params![state as i64], |r| r.get(0))
            .optional()
            .map_err(Self::sql_err)?;
        Ok(v.map(|x| x as usize))
    }

    async fn add_root_input(&mut self, state: usize) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO rt_root_inputs (state) VALUES (?1)",
            params![state as i64],
        )
        .map_err(Self::sql_err)?;
        Ok(())
    }

    async fn get_root_inputs(&self) -> Result<Vec<usize>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT state FROM rt_root_inputs ORDER BY state")
            .map_err(Self::sql_err)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, i64>(0))
            .map_err(Self::sql_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(Self::sql_err)? as usize);
        }
        Ok(out)
    }

    async fn get_label(&self, state: usize) -> Result<String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT label FROM rt_states WHERE id = ?1")
            .map_err(Self::sql_err)?;
        stmt.query_row(params![state as i64], |r| r.get(0))
            .optional()
            .map_err(Self::sql_err)?
            .ok_or(StorageError::BranchOutOfRange(state))
    }

    async fn num_states(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare_cached("SELECT COUNT(*) FROM rt_states")
            .map_err(Self::sql_err)?;
        let n: i64 = stmt.query_row([], |r| r.get(0)).map_err(Self::sql_err)?;
        Ok(n as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::radixtree::RadixTree;

    /// Helper: async test với `SqliteStorage::in_memory()`.
    #[tokio::test]
    async fn node_crud_and_children() {
        let mut st = SqliteStorage::in_memory().unwrap();

        let n1 = st.new_node(vec![1, 2, 3], 7).await.unwrap();
        let n2 = st.new_node(vec![9], 0).await.unwrap();
        assert_eq!(n1, 1); // sentinel tại id 0
        assert_eq!(n2, 2);

        st.add_child(n1, n2).await.unwrap();
        assert_eq!(st.get_children(n1).await.unwrap(), vec![2]);

        let (prefix, record) = st.get_node(n1).await.unwrap();
        assert_eq!(prefix, vec![1, 2, 3]);
        assert_eq!(record, 7);

        st.update_node(n1, Some(vec![1, 2]), Some(99)).await.unwrap();
        assert_eq!(st.get_node(n1).await.unwrap(), (vec![1, 2], 99));

        st.remove_child(n1, n2).await.unwrap();
        assert!(st.get_children(n1).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn roots_and_split_commit() {
        let mut st = SqliteStorage::in_memory().unwrap();

        st.set_root(0, 5).await.unwrap();
        st.set_root(1, 6).await.unwrap();
        assert_eq!(st.get_root(0).await.unwrap(), 5);
        assert_eq!(st.get_root(1).await.unwrap(), 6);
        assert_eq!(st.get_root(9).await.unwrap(), EMPTY);

        // commit_split: update prefix/record + xoá children cũ
        let n1 = st.new_node(vec![1, 2, 3], 7).await.unwrap(); // id = 1
        let n2 = st.new_node(vec![9], 0).await.unwrap(); // id = 2
        let n3 = st.new_node(vec![8], 0).await.unwrap(); // id = 3
        st.add_child(n1, n2).await.unwrap();
        st.add_child(n1, n3).await.unwrap();
        st.commit_split(n1, vec![0, 0], 42, &[n2, n3]).await.unwrap();
        assert_eq!(st.get_node(n1).await.unwrap(), (vec![0, 0], 42));
        assert!(st.get_children(n1).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn entries_and_meta() {
        let mut st = SqliteStorage::in_memory().unwrap();

        st.save_entry(1, 100, "func_a").await.unwrap();
        st.save_entry(2, 200, "func_b").await.unwrap();
        st.save_entry_meta(1, b"meta-a").await.unwrap();

        assert_eq!(st.load_entry(1).await.unwrap(), (100, "func_a".into()));
        assert_eq!(st.load_entry(2).await.unwrap(), (200, "func_b".into()));
        assert_eq!(
            st.load_entry_meta(1).await.unwrap(),
            Some(b"meta-a".to_vec())
        );
        assert_eq!(st.load_entry_meta(2).await.unwrap(), None);
        assert_eq!(st.count_entries().await.unwrap(), 2);

        // save_entry không được ghi đè meta
        st.save_entry(1, 101, "func_a2").await.unwrap();
        assert_eq!(st.load_entry(1).await.unwrap(), (101, "func_a2".into()));
        assert_eq!(
            st.load_entry_meta(1).await.unwrap(),
            Some(b"meta-a".to_vec())
        );

        // roundtrip entries
        st.save_entries(&[(1, "x".into()), (2, "y".into())]).await.unwrap();
        assert_eq!(st.load_entries().await.unwrap(), vec![(1, "x".into()), (2, "y".into())]);
    }

    #[tokio::test]
    async fn record_counter_allocation() {
        let mut st = SqliteStorage::in_memory().unwrap();
        assert_eq!(st.allocate_record_id().await.unwrap(), 1);
        assert_eq!(st.allocate_record_id().await.unwrap(), 2);
        assert_eq!(st.allocate_record_id().await.unwrap(), 3);

        // init_record_counter chỉ set khi chưa tồn tại (giống SET NX)
        let mut st2 = SqliteStorage::in_memory().unwrap();
        st2.init_record_counter(10).await.unwrap();
        assert_eq!(st2.allocate_record_id().await.unwrap(), 11);
        st2.init_record_counter(5).await.unwrap();
        assert_eq!(st2.allocate_record_id().await.unwrap(), 12);
    }

    #[tokio::test]
    async fn blobs() {
        let mut st = SqliteStorage::in_memory().unwrap();
        st.save_blob("key1", b"data1").await.unwrap();
        assert_eq!(st.load_blob("key1").await.unwrap(), Some(b"data1".to_vec()));
        assert_eq!(st.load_blob("missing").await.unwrap(), None);
    }

    #[tokio::test]
    async fn radix_tree_end_to_end() {
        // Chạy RadixTree trên SqliteStorage — tương tự test của InMemoryStorage.
        let mut tree = RadixTree::<u64>::new(4, SqliteStorage::in_memory().unwrap());

        let (id1, _) = tree.insert(&[10, 20], 1).await.unwrap();
        assert_ne!(id1, crate::radixtree::EMPTY);
        let (id2, _) = tree.insert(&[10, 30], 2).await.unwrap();
        assert_ne!(id2, crate::radixtree::EMPTY);
        let (id3, _) = tree.insert(&[11, 5], 3).await.unwrap();
        assert_ne!(id3, crate::radixtree::EMPTY);

        assert_eq!(tree.r#match(&[10, 20]).await.unwrap(), 1);
        assert_eq!(tree.r#match(&[10, 30]).await.unwrap(), 2);
        assert_eq!(tree.r#match(&[11, 5]).await.unwrap(), 3);
        assert!(tree.r#match(&[10, 40]).await.is_err());

        // insert duplicate key → EMPTY
        let (dup, _) = tree.insert(&[10, 20], 99).await.unwrap();
        assert_eq!(dup, crate::radixtree::EMPTY);

        // search_prefix trả về toàn bộ leaf dưới prefix
        let prefixed = tree.search_prefix(&[10]).await.unwrap();
        assert_eq!(prefixed.len(), 2);
    }

    #[tokio::test]
    async fn batch_methods_match_default_impl() {
        // Dựng cùng một tree trên Sqlite + InMemory, so sánh batch methods.
        let keys: Vec<Vec<u64>> = vec![
            vec![1, 2],
            vec![1, 3],
            vec![1, 4, 5],
            vec![2, 6],
            vec![2, 7, 8],
        ];
        let mut sql = RadixTree::<u64>::new(4, SqliteStorage::in_memory().unwrap());
        let mut mem = RadixTree::<u64>::new(4, crate::storage::InMemoryStorage::default());
        for (i, k) in keys.iter().enumerate() {
            sql.insert(k, i + 1).await.unwrap();
            mem.insert(k, i + 1).await.unwrap();
        }

        // get_children_with_prefixes khớp giữa 2 backend (với node id tương ứng).
        // Dùng scan_subtree từng root shard — tổng node/số record phải khớp.
        let mut sql_rows = Vec::new();
        let mut mem_rows = Vec::new();
        for si in 0..4 {
            let sr = sql.get_storage_root(si).await.unwrap();
            let mr = mem.get_storage_root(si).await.unwrap();
            if sr == crate::radixtree::EMPTY {
                assert_eq!(mr, crate::radixtree::EMPTY);
                continue;
            }
            sql_rows.extend(sql.scan_subtree(sr).await.unwrap());
            mem_rows.extend(mem.scan_subtree(mr).await.unwrap());
        }

        // So sánh theo (child, prefix, record) — parent/child id có thể lệch
        // giữa 2 backend (thứ tự allocate khác nhau), nên sort theo prefix.
        let mut norm_sql: Vec<(Vec<u8>, usize)> = sql_rows
            .iter()
            .map(|(_, _, p, r)| (p.clone(), *r))
            .collect();
        let mut norm_mem: Vec<(Vec<u8>, usize)> = mem_rows
            .iter()
            .map(|(_, _, p, r)| (p.clone(), *r))
            .collect();
        // Bỏ sentinel/root rỗng (prefix rỗng)
        norm_sql.retain(|(p, _)| !p.is_empty());
        norm_mem.retain(|(p, _)| !p.is_empty());
        norm_sql.sort();
        norm_mem.sort();
        assert_eq!(norm_sql, norm_mem, "scan_subtree nội dung khác nhau giữa backend");

        // get_children_with_prefixes: so qua prefix của root (shard có data).
        let sr = sql.get_storage_root(0).await.unwrap();
        let mr = mem.get_storage_root(0).await.unwrap();
        let mut sql_c: Vec<Vec<u8>> = sql
            .get_children_with_prefixes(sr)
            .await
            .unwrap()
            .iter()
            .map(|(_, p, _)| p.clone())
            .collect();
        let mut mem_c: Vec<Vec<u8>> = mem
            .get_children_with_prefixes(mr)
            .await
            .unwrap()
            .iter()
            .map(|(_, p, _)| p.clone())
            .collect();
        sql_c.sort();
        mem_c.sort();
        assert_eq!(sql_c, mem_c);
    }

    #[tokio::test]
    async fn bulk_insert_with_splits() {
        // begin_bulk → insert key chia sẻ prefix (trigger split → commit_split
        // lồng trong transaction) → end_bulk. Kết quả phải khớp không-bulk.
        let mut tree = RadixTree::<u64>::new(4, SqliteStorage::in_memory().unwrap());
        tree.begin_bulk().await.unwrap();
        for (i, k) in [
            vec![5, 1],
            vec![5, 2],
            vec![5, 3, 7],
            vec![5, 3, 8],
            vec![6, 9],
        ]
        .iter()
        .enumerate()
        {
            let (id, _) = tree.insert(k, i + 1).await.unwrap();
            assert_ne!(id, crate::radixtree::EMPTY, "insert {k:?} thất bại trong bulk");
        }
        tree.end_bulk().await.unwrap();

        assert_eq!(tree.r#match(&[5, 1]).await.unwrap(), 1);
        assert_eq!(tree.r#match(&[5, 3, 8]).await.unwrap(), 4);
        assert_eq!(tree.search_prefix(&[5]).await.unwrap().len(), 4);
        assert_eq!(tree.search_prefix(&[5, 3]).await.unwrap().len(), 2);
    }
}
