//! SQLite-backed radix-node storage (sqlx) — persistent backend cho `Search`.
//!
//! Khác `codegraph-db` (lưu node/file/FTS của graph), đây là storage cho
//! **radix tree**: prefix + record + children + root của từng shard + metadata
//! + key length + shortcuts. Mỗi `SqliteStorage` = 1 file `.sqlite` riêng:
//!
//! - `Search::sqlite` mở nó.
//! - Forward/reverse index của `CallIndex` dùng 2 file khác nhau để không đụng
//!   id counter.
//!
//! Schema:
//! | Table          | Mục đích                               |
//! |----------------|----------------------------------------|
//! | `rt_nodes`     | id → (prefix, record); id 0 = sentinel |
//! | `rt_children`  | parent → children (PK (parent, child)) |
//! | `rt_roots`     | shard → root node id                   |
//! | `rt_meta`      | record → metadata (opaque bytes)       |
//! | `rt_keylen`    | record → key length (filter `depth`)   |
//! | `rt_shortcuts` | (shard, elem) → node ids chứa elem     |
//! | `rt_edges`     | edge id → edge data (CallEdgeMeta)     |
//! | `rt_node_meta` | element id → node metadata (Node JSON) |
//! | `rt_chains`    | record → chain bytes (u64 LE/element)  |
//! | `rt_counter`   | bộ cấp id (`next`)                     |
//!
//! Mỗi method tự acquire connection từ pool; `SqliteTx` buffer ops và áp dụng
//! atomic trong một SQLite transaction tại `commit` (giống InMemory/Redis).
//! Mọi query là runtime SQL (không dùng macro `query!` — tránh phụ thuộc
//! `DATABASE_URL` lúc build).
//!
//! Nếu extension sqlite-vss (`vector0`/`vss0`) có mặt (config
//! `[embedding].vss_extension`), kết nối sẽ load extension và tạo thêm virtual
//! table `sg_vss USING vss0(vec(384))` để KNN semantic chạy HNSW ANN ngay trong
//! SQLite. Thiếu extension → `sg_vss` không được tạo, KNN fallback brute-force
//! in-memory (như mọi backend khác).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use codegraph_core::{FileInfo, Symbol};
use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};

use super::{
    CategoryStorage, ChainStorage, EMPTY, EdgeDataStorage, EntityStorage, IndexCounts,
    NodeMetaStorage, Result, ShortcutsStorage, StorageError, Tx, TxOp, decode_vector,
    encode_vector,
};

#[cfg(feature = "bloom-search")]
use super::BloomStorage;
use crate::embeddings::resolve_vss_extensions;

fn db_err(e: sqlx::Error) -> StorageError {
    StorageError::Internal(e.to_string())
}

// ==================== SqliteStorage ====================

pub struct SqliteStorage {
    pool: SqlitePool,
    /// `true` nếu extension sqlite-vss (`vss0`) đã load thành công và bảng
    /// `sg_vss` sẵn sàng — KNN semantic chạy qua `vss0` (HNSW ANN trong SQLite).
    /// `false` → KNN fallback sang `VectorIndex` in-memory (brute-force).
    vss_available: AtomicBool,
}

impl SqliteStorage {
    /// Mở (hoặc tạo mới nếu chưa tồn tại) file sqlite tại `path`.
    ///
    /// Idempotent với file cũ — schema `CREATE TABLE IF NOT EXISTS` + sentinel
    /// `INSERT OR IGNORE` nên reopen giữ nguyên toàn bộ dữ liệu.
    pub async fn open(path: &str) -> Result<Self> {
        if let Some(parent) = std::path::Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| StorageError::Internal(e.to_string()))?;
        }
        // Nếu extension sqlite-vss (`vector0`/`vss0`) có mặt → load vào kết nối
        // để KNN chạy HNSW ANN ngay trong SQLite. Thiếu file → không load, KNN
        // fallback brute-force (open vẫn thành công).
        let vss = resolve_vss_extensions();
        let mut options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5));
        let vss_requested = if let Some((v0, vss_ext)) = &vss {
            options = options
                .extension(v0.to_string_lossy().into_owned())
                .extension(vss_ext.to_string_lossy().into_owned());
            true
        } else {
            false
        };
        let pool = SqlitePoolOptions::new()
            .connect_with(options)
            .await
            .map_err(db_err)?;
        let s = Self {
            pool,
            vss_available: AtomicBool::new(false),
        };
        s.init().await?;
        // Bật `sg_vss` (vss0 virtual table) khi extension đã được load. Nếu tạo
        // bảng lỗi → tắt vss, KNN fallback brute-force (vẫn hoạt động đúng).
        let available = if vss_requested {
            match sqlx::query("CREATE VIRTUAL TABLE IF NOT EXISTS sg_vss USING vss0(vec(384))")
                .execute(&mut *s.pool.acquire().await.map_err(db_err)?)
                .await
            {
                Ok(_) => true,
                Err(e) => {
                    eprintln!(
                        "codegraph: sqlite-vss loaded but vss0 table create failed; \
                         falling back to brute-force KNN: {e}"
                    );
                    false
                }
            }
        } else {
            false
        };
        s.vss_available.store(available, Ordering::SeqCst);
        Ok(s)
    }

    /// Đọc `index_version` từ file mà KHÔNG tạo file (nếu chưa có) — dùng bởi
    /// `SharedGraphIndex::ensure_fresh` để dò stale trước khi quyết định rebuild.
    pub async fn probe_version(path: &str) -> Result<u64> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .connect_with(options)
            .await
            .map_err(db_err)?;
        let mut conn = pool.acquire().await.map_err(db_err)?;
        let v: i64 = sqlx::query_scalar("SELECT version FROM sg_meta WHERE id = 1")
            .fetch_one(&mut *conn)
            .await
            .map_err(db_err)?;
        Ok(v as u64)
    }

    async fn init(&self) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        for stmt in [
            "CREATE TABLE IF NOT EXISTS rt_nodes (
                id INTEGER PRIMARY KEY,
                prefix BLOB NOT NULL,
                record INTEGER NOT NULL
            )",
            "CREATE TABLE IF NOT EXISTS rt_children (
                parent INTEGER NOT NULL,
                child INTEGER NOT NULL,
                PRIMARY KEY (parent, child)
            )",
            "CREATE INDEX IF NOT EXISTS idx_rt_children_parent ON rt_children(parent)",
            "CREATE TABLE IF NOT EXISTS rt_roots (
                shard INTEGER PRIMARY KEY,
                root INTEGER NOT NULL
            )",
            "CREATE TABLE IF NOT EXISTS rt_meta (
                record INTEGER PRIMARY KEY,
                meta BLOB NOT NULL
            )",
            "CREATE TABLE IF NOT EXISTS rt_keylen (
                record INTEGER PRIMARY KEY,
                len INTEGER NOT NULL
            )",
            "CREATE TABLE IF NOT EXISTS rt_shortcuts (
                shard INTEGER NOT NULL,
                elem BLOB NOT NULL,
                node_id INTEGER NOT NULL,
                PRIMARY KEY (shard, elem, node_id)
            )",
            "CREATE INDEX IF NOT EXISTS idx_rt_shortcuts_lookup ON rt_shortcuts(shard, elem)",
            "CREATE TABLE IF NOT EXISTS rt_edges (
                id INTEGER PRIMARY KEY,
                data BLOB NOT NULL
            )",
            "CREATE TABLE IF NOT EXISTS rt_node_meta (
                elem INTEGER PRIMARY KEY,
                meta BLOB NOT NULL
            )",
            "CREATE TABLE IF NOT EXISTS rt_chains (
                record INTEGER PRIMARY KEY,
                chain BLOB NOT NULL
            )",
            "CREATE TABLE IF NOT EXISTS rt_node_blooms (
                id INTEGER PRIMARY KEY,
                bloom BLOB NOT NULL
            )",
            "CREATE TABLE IF NOT EXISTS rt_counter (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                next INTEGER NOT NULL
            )",
            // ── Entity store (semgraph model — db/ cũ dời xuống đây) ──
            "CREATE TABLE IF NOT EXISTS sg_symbols (
                id INTEGER PRIMARY KEY,
                data BLOB NOT NULL
            )",
            "CREATE TABLE IF NOT EXISTS sg_next_id (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                next INTEGER NOT NULL
            )",
            "CREATE TABLE IF NOT EXISTS sg_call_records (
                func INTEGER PRIMARY KEY,
                records BLOB NOT NULL
            )",
            "CREATE TABLE IF NOT EXISTS sg_call_names (
                name TEXT PRIMARY KEY,
                sites BLOB NOT NULL
            )",
            "CREATE TABLE IF NOT EXISTS sg_files (
                path TEXT PRIMARY KEY,
                language TEXT NOT NULL,
                bytes INTEGER NOT NULL,
                lines INTEGER NOT NULL
            )",
            "CREATE TABLE IF NOT EXISTS sg_meta (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                version INTEGER NOT NULL
            )",
            // ── Stats (counts tổng hợp — codegraph_status đọc O(1)) ──
            "CREATE TABLE IF NOT EXISTS sg_stats (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                symbols INTEGER NOT NULL,
                chains INTEGER NOT NULL,
                edges INTEGER NOT NULL,
                files INTEGER NOT NULL,
                next_id INTEGER NOT NULL
            )",
            // ── Embeddings (vector per symbol id) ──
            "CREATE TABLE IF NOT EXISTS sg_embeddings (
                symbol_id INTEGER PRIMARY KEY,
                vector BLOB NOT NULL
            )",
            // Sentinel node id 0 + counter bắt đầu từ 1.
            "INSERT OR IGNORE INTO rt_nodes (id, prefix, record) VALUES (0, X'', 0)",
            "INSERT OR IGNORE INTO rt_counter (id, next) VALUES (1, 1)",
            // next_id bắt đầu từ SYMBOL_BASE (marker reserved 1..=99).
            "INSERT OR IGNORE INTO sg_next_id (id, next) VALUES (1, 100)",
            "INSERT OR IGNORE INTO sg_meta (id, version) VALUES (1, 0)",
            "INSERT OR IGNORE INTO sg_stats (id, symbols, chains, edges, files, next_id) VALUES (1, 0, 0, 0, 0, 0)",
        ] {
            sqlx::query(stmt)
                .execute(&mut *conn)
                .await
                .map_err(db_err)?;
        }
        Ok(())
    }
}

#[async_trait]
impl CategoryStorage for SqliteStorage {
    async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        // `UPDATE ... RETURNING next - 1` cấp id atomic — không cần SELECT rồi
        // UPDATE (2 bước có thể bị xen giữa bởi writer khác).
        let next: i64 = sqlx::query_scalar(
            "UPDATE rt_counter SET next = next + 1 WHERE id = 1 RETURNING next - 1",
        )
        .fetch_one(&mut *conn)
        .await
        .map_err(db_err)?;
        let id = next as usize;
        sqlx::query("INSERT INTO rt_nodes (id, prefix, record) VALUES (?1, ?2, ?3)")
            .bind(id as i64)
            .bind(prefix)
            .bind(record as i64)
            .execute(&mut *conn)
            .await
            .map_err(db_err)?;
        Ok(id)
    }

    async fn update_node(
        &mut self,
        id: usize,
        prefix: Option<Vec<u8>>,
        record: Option<usize>,
    ) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        if let Some(p) = prefix {
            let r = sqlx::query("UPDATE rt_nodes SET prefix = ?1 WHERE id = ?2")
                .bind(p)
                .bind(id as i64)
                .execute(&mut *conn)
                .await
                .map_err(db_err)?;
            if r.rows_affected() == 0 {
                return Err(StorageError::BranchOutOfRange(id));
            }
        }
        if let Some(rec) = record {
            let r = sqlx::query("UPDATE rt_nodes SET record = ?1 WHERE id = ?2")
                .bind(rec as i64)
                .bind(id as i64)
                .execute(&mut *conn)
                .await
                .map_err(db_err)?;
            if r.rows_affected() == 0 {
                return Err(StorageError::BranchOutOfRange(id));
            }
        }
        Ok(())
    }

    async fn get_node(&self, id: usize) -> Result<(Vec<u8>, usize)> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let row = sqlx::query("SELECT prefix, record FROM rt_nodes WHERE id = ?1")
            .bind(id as i64)
            .fetch_optional(&mut *conn)
            .await
            .map_err(db_err)?;
        let Some(row) = row else {
            return Err(StorageError::BranchOutOfRange(id));
        };
        let prefix: Vec<u8> = row.try_get(0).map_err(db_err)?;
        let record: i64 = row.try_get(1).map_err(db_err)?;
        Ok((prefix, record as usize))
    }

    async fn get_children(&self, id: usize) -> Result<Vec<usize>> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let rows = sqlx::query("SELECT child FROM rt_children WHERE parent = ?1 ORDER BY child")
            .bind(id as i64)
            .fetch_all(&mut *conn)
            .await
            .map_err(db_err)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let c: i64 = r.try_get(0).map_err(db_err)?;
            out.push(c as usize);
        }
        Ok(out)
    }

    async fn set_root(&mut self, shard: usize, root: usize) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        sqlx::query(
            "INSERT INTO rt_roots (shard, root) VALUES (?1, ?2)
             ON CONFLICT(shard) DO UPDATE SET root = excluded.root",
        )
        .bind(shard as i64)
        .bind(root as i64)
        .execute(&mut *conn)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn get_root(&self, shard: usize) -> Result<usize> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let root: Option<i64> = sqlx::query_scalar("SELECT root FROM rt_roots WHERE shard = ?1")
            .bind(shard as i64)
            .fetch_optional(&mut *conn)
            .await
            .map_err(db_err)?;
        Ok(root.unwrap_or(EMPTY as i64) as usize)
    }

    fn new_tx(&self) -> Box<dyn Tx> {
        Box::new(SqliteTx {
            pool: self.pool.clone(),
            nodes: Vec::new(),
            ops: Vec::new(),
        })
    }
}

#[cfg(feature = "bloom-search")]
#[async_trait]
impl BloomStorage for SqliteStorage {
    async fn set_node_bloom(&mut self, id: usize, bloom: &[u8]) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        sqlx::query(
            "INSERT INTO rt_node_blooms (id, bloom) VALUES (?1, ?2) \
                     ON CONFLICT(id) DO UPDATE SET bloom = excluded.bloom",
        )
        .bind(id as i64)
        .bind(bloom)
        .execute(&mut *conn)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn get_node_bloom(&self, id: usize) -> Result<Option<Vec<u8>>> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let row = sqlx::query("SELECT bloom FROM rt_node_blooms WHERE id = ?1")
            .bind(id as i64)
            .fetch_optional(&mut *conn)
            .await
            .map_err(db_err)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let bloom: Vec<u8> = row.try_get(0).map_err(db_err)?;
        Ok(Some(bloom))
    }
}

#[async_trait]
impl EdgeDataStorage for SqliteStorage {
    async fn set_edge_data(&mut self, edge: usize, data: &[u8]) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        sqlx::query(
            "INSERT INTO rt_edges (id, data) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET data = excluded.data",
        )
        .bind(edge as i64)
        .bind(data)
        .execute(&mut *conn)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn get_edge_data(&self, edge: usize) -> Result<Option<Vec<u8>>> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let data: Option<Vec<u8>> = sqlx::query_scalar("SELECT data FROM rt_edges WHERE id = ?1")
            .bind(edge as i64)
            .fetch_optional(&mut *conn)
            .await
            .map_err(db_err)?;
        Ok(data)
    }

    async fn clear_edges(&mut self) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        sqlx::query("DELETE FROM rt_edges")
            .execute(&mut *conn)
            .await
            .map_err(db_err)?;
        Ok(())
    }
}

#[async_trait]
impl NodeMetaStorage for SqliteStorage {
    async fn set_node_meta(&mut self, elem: usize, meta: &[u8]) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        sqlx::query(
            "INSERT INTO rt_node_meta (elem, meta) VALUES (?1, ?2)
             ON CONFLICT(elem) DO UPDATE SET meta = excluded.meta",
        )
        .bind(elem as i64)
        .bind(meta)
        .execute(&mut *conn)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn get_node_meta(&self, elem: usize) -> Result<Option<Vec<u8>>> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let meta: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT meta FROM rt_node_meta WHERE elem = ?1")
                .bind(elem as i64)
                .fetch_optional(&mut *conn)
                .await
                .map_err(db_err)?;
        Ok(meta)
    }

    async fn clear_node_meta(&mut self) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        sqlx::query("DELETE FROM rt_node_meta")
            .execute(&mut *conn)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn set_meta(&mut self, record: usize, meta: &[u8]) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        sqlx::query(
            "INSERT INTO rt_meta (record, meta) VALUES (?1, ?2)
             ON CONFLICT(record) DO UPDATE SET meta = excluded.meta",
        )
        .bind(record as i64)
        .bind(meta)
        .execute(&mut *conn)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn get_meta(&self, record: usize) -> Result<Option<Vec<u8>>> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let meta: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT meta FROM rt_meta WHERE record = ?1")
                .bind(record as i64)
                .fetch_optional(&mut *conn)
                .await
                .map_err(db_err)?;
        Ok(meta)
    }

    async fn set_key_len(&mut self, record: usize, len: usize) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        sqlx::query(
            "INSERT INTO rt_keylen (record, len) VALUES (?1, ?2)
             ON CONFLICT(record) DO UPDATE SET len = excluded.len",
        )
        .bind(record as i64)
        .bind(len as i64)
        .execute(&mut *conn)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn get_key_len(&self, record: usize) -> Result<Option<usize>> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let len: Option<i64> = sqlx::query_scalar("SELECT len FROM rt_keylen WHERE record = ?1")
            .bind(record as i64)
            .fetch_optional(&mut *conn)
            .await
            .map_err(db_err)?;
        Ok(len.map(|x| x as usize))
    }
}

#[async_trait]
impl ShortcutsStorage for SqliteStorage {
    async fn add_shortcut_node(&mut self, shard: usize, elem: &[u8], node_id: usize) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        sqlx::query(
            "INSERT INTO rt_shortcuts (shard, elem, node_id) VALUES (?1, ?2, ?3)
             ON CONFLICT DO NOTHING",
        )
        .bind(shard as i64)
        .bind(elem)
        .bind(node_id as i64)
        .execute(&mut *conn)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn get_shortcut_nodes(&self, shard: usize, elem: &[u8]) -> Result<Vec<usize>> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let rows = sqlx::query(
            "SELECT node_id FROM rt_shortcuts WHERE shard = ?1 AND elem = ?2 ORDER BY node_id",
        )
        .bind(shard as i64)
        .bind(elem)
        .fetch_all(&mut *conn)
        .await
        .map_err(db_err)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let c: i64 = r.try_get(0).map_err(db_err)?;
            out.push(c as usize);
        }
        Ok(out)
    }

    async fn clear_shortcuts(&mut self) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        sqlx::query("DELETE FROM rt_shortcuts")
            .execute(&mut *conn)
            .await
            .map_err(db_err)?;
        Ok(())
    }
}

#[async_trait]
impl ChainStorage for SqliteStorage {
    async fn set_chain(&mut self, record: usize, chain: &[u64]) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        sqlx::query(
            "INSERT INTO rt_chains (record, chain) VALUES (?1, ?2)
             ON CONFLICT(record) DO UPDATE SET chain = excluded.chain",
        )
        .bind(record as i64)
        .bind(super::encode_chain(chain))
        .execute(&mut *conn)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn get_chain(&self, record: usize) -> Result<Option<Vec<u64>>> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let bytes: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT chain FROM rt_chains WHERE record = ?1")
                .bind(record as i64)
                .fetch_optional(&mut *conn)
                .await
                .map_err(db_err)?;
        Ok(bytes.map(|b| super::decode_chain(&b)))
    }

    async fn clear_chains(&mut self) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        sqlx::query("DELETE FROM rt_chains")
            .execute(&mut *conn)
            .await
            .map_err(db_err)?;
        Ok(())
    }
}

#[async_trait]
impl EntityStorage for SqliteStorage {
    async fn save_symbol(&mut self, sym: &Symbol) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let data = serde_json::to_vec(sym).map_err(|e| StorageError::Internal(e.to_string()))?;
        sqlx::query(
            "INSERT INTO sg_symbols (id, data) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET data = excluded.data",
        )
        .bind(sym.id as i64)
        .bind(data)
        .execute(&mut *conn)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn load_symbol(&self, id: u64) -> Result<Option<Symbol>> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let data: Option<Vec<u8>> = sqlx::query_scalar("SELECT data FROM sg_symbols WHERE id = ?1")
            .bind(id as i64)
            .fetch_optional(&mut *conn)
            .await
            .map_err(db_err)?;
        data.map(|d| serde_json::from_slice(&d).map_err(|e| StorageError::Internal(e.to_string())))
            .transpose()
    }

    async fn load_all_symbols(&self) -> Result<Vec<Symbol>> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let rows: Vec<Vec<u8>> = sqlx::query_scalar("SELECT data FROM sg_symbols ORDER BY id")
            .fetch_all(&mut *conn)
            .await
            .map_err(db_err)?;
        rows.into_iter()
            .map(|d| serde_json::from_slice(&d).map_err(|e| StorageError::Internal(e.to_string())))
            .collect()
    }

    async fn save_next_id(&mut self, next: u64) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        sqlx::query("UPDATE sg_next_id SET next = ?1 WHERE id = 1")
            .bind(next as i64)
            .execute(&mut *conn)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn load_next_id(&self) -> Result<u64> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let next: i64 = sqlx::query_scalar("SELECT next FROM sg_next_id WHERE id = 1")
            .fetch_one(&mut *conn)
            .await
            .map_err(db_err)?;
        Ok(next as u64)
    }

    async fn all_chains(&self) -> Result<Vec<(u64, Vec<u8>)>> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let rows: Vec<(i64, Vec<u8>)> =
            sqlx::query_as("SELECT record, chain FROM rt_chains ORDER BY record")
                .fetch_all(&mut *conn)
                .await
                .map_err(db_err)?;
        Ok(rows.into_iter().map(|(r, b)| (r as u64, b)).collect())
    }

    async fn set_call_records(&mut self, func: u64, records: &[u8]) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        sqlx::query(
            "INSERT INTO sg_call_records (func, records) VALUES (?1, ?2)
             ON CONFLICT(func) DO UPDATE SET records = excluded.records",
        )
        .bind(func as i64)
        .bind(records)
        .execute(&mut *conn)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn get_call_records(&self, func: u64) -> Result<Option<Vec<u8>>> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let records: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT records FROM sg_call_records WHERE func = ?1")
                .bind(func as i64)
                .fetch_optional(&mut *conn)
                .await
                .map_err(db_err)?;
        Ok(records)
    }

    async fn all_call_records(&self) -> Result<Vec<(u64, Vec<u8>)>> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let rows: Vec<(i64, Vec<u8>)> =
            sqlx::query_as("SELECT func, records FROM sg_call_records ORDER BY func")
                .fetch_all(&mut *conn)
                .await
                .map_err(db_err)?;
        Ok(rows.into_iter().map(|(f, b)| (f as u64, b)).collect())
    }

    async fn set_call_name_index(&mut self, name: &str, sites: &[u8]) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        sqlx::query(
            "INSERT INTO sg_call_names (name, sites) VALUES (?1, ?2)
             ON CONFLICT(name) DO UPDATE SET sites = excluded.sites",
        )
        .bind(name)
        .bind(sites)
        .execute(&mut *conn)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn load_call_name_index(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let sites: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT sites FROM sg_call_names WHERE name = ?1")
                .bind(name)
                .fetch_optional(&mut *conn)
                .await
                .map_err(db_err)?;
        Ok(sites)
    }

    async fn all_call_name_indexes(&self) -> Result<Vec<(String, Vec<u8>)>> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let rows: Vec<(String, Vec<u8>)> =
            sqlx::query_as("SELECT name, sites FROM sg_call_names ORDER BY name")
                .fetch_all(&mut *conn)
                .await
                .map_err(db_err)?;
        Ok(rows)
    }

    async fn upsert_file(&mut self, f: &FileInfo) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        sqlx::query(
            "INSERT INTO sg_files (path, language, bytes, lines) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET language = excluded.language,
                 bytes = excluded.bytes, lines = excluded.lines",
        )
        .bind(&f.path)
        .bind(&f.language)
        .bind(f.bytes as i64)
        .bind(f.lines as i64)
        .execute(&mut *conn)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn load_all_files(&self) -> Result<Vec<FileInfo>> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let rows: Vec<(String, String, i64, i64)> =
            sqlx::query_as("SELECT path, language, bytes, lines FROM sg_files ORDER BY path")
                .fetch_all(&mut *conn)
                .await
                .map_err(db_err)?;
        Ok(rows
            .into_iter()
            .map(|(path, language, bytes, lines)| FileInfo {
                path,
                language,
                bytes: bytes as u64,
                lines: lines as u32,
            })
            .collect())
    }

    async fn version(&self) -> Result<u64> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let v: i64 = sqlx::query_scalar("SELECT version FROM sg_meta WHERE id = 1")
            .fetch_one(&mut *conn)
            .await
            .map_err(db_err)?;
        Ok(v as u64)
    }

    async fn set_version(&mut self, v: u64) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        sqlx::query("UPDATE sg_meta SET version = ?1 WHERE id = 1")
            .bind(v as i64)
            .execute(&mut *conn)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn set_stats(&mut self, s: IndexCounts) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        sqlx::query(
            "INSERT INTO sg_stats (id, symbols, chains, edges, files, next_id) \
             VALUES (1, ?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(id) DO UPDATE SET \
             symbols = excluded.symbols, chains = excluded.chains, \
             edges = excluded.edges, files = excluded.files, next_id = excluded.next_id",
        )
        .bind(s.symbols as i64)
        .bind(s.chains as i64)
        .bind(s.edges as i64)
        .bind(s.files as i64)
        .bind(s.next_id as i64)
        .execute(&mut *conn)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn stats(&self) -> Result<IndexCounts> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let row: Option<(i64, i64, i64, i64, i64)> = sqlx::query_as(
            "SELECT symbols, chains, edges, files, next_id FROM sg_stats WHERE id = 1",
        )
        .fetch_optional(&mut *conn)
        .await
        .map_err(db_err)?;
        match row {
            Some((symbols, chains, edges, files, next_id)) => Ok(IndexCounts {
                symbols: symbols as u64,
                chains: chains as u64,
                edges: edges as u64,
                files: files as u64,
                next_id: next_id as u64,
            }),
            // Bảng thiếu (index cũ) → trả 0 để caller fallback rebuild.
            None => Ok(IndexCounts::default()),
        }
    }

    async fn clear_entities(&mut self) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        for stmt in [
            "DELETE FROM sg_symbols",
            "DELETE FROM sg_call_records",
            "DELETE FROM sg_call_names",
            "DELETE FROM sg_files",
            "DELETE FROM sg_embeddings",
            "UPDATE sg_next_id SET next = 100 WHERE id = 1",
            "UPDATE sg_meta SET version = 0 WHERE id = 1",
        ] {
            sqlx::query(stmt)
                .execute(&mut *conn)
                .await
                .map_err(db_err)?;
        }
        Ok(())
    }

    async fn save_embedding(&mut self, symbol_id: u64, vector: &[f32]) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        sqlx::query(
            "INSERT INTO sg_embeddings (symbol_id, vector) VALUES (?1, ?2)
             ON CONFLICT(symbol_id) DO UPDATE SET vector = excluded.vector",
        )
        .bind(symbol_id as i64)
        .bind(encode_vector(vector))
        .execute(&mut *conn)
        .await
        .map_err(db_err)?;
        // Mirror vào `vss0` (HNSW ANN) nếu extension khả dụng.
        if self.vss_available.load(Ordering::SeqCst) {
            sqlx::query("INSERT OR REPLACE INTO sg_vss(rowid, vec) VALUES (?1, ?2)")
                .bind(symbol_id as i64)
                .bind(encode_vector(vector))
                .execute(&mut *conn)
                .await
                .map_err(db_err)?;
        }
        Ok(())
    }

    async fn load_embedding(&self, symbol_id: u64) -> Result<Option<Vec<f32>>> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let data: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT vector FROM sg_embeddings WHERE symbol_id = ?1")
                .bind(symbol_id as i64)
                .fetch_optional(&mut *conn)
                .await
                .map_err(db_err)?;
        Ok(data.and_then(|b| decode_vector(&b)))
    }

    async fn load_all_embeddings(&self) -> Result<HashMap<u64, Vec<f32>>> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        let rows: Vec<(i64, Vec<u8>)> =
            sqlx::query_as("SELECT symbol_id, vector FROM sg_embeddings ORDER BY symbol_id")
                .fetch_all(&mut *conn)
                .await
                .map_err(db_err)?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, b)| decode_vector(&b).map(|v| (id as u64, v)))
            .collect())
    }

    async fn clear_embeddings(&mut self) -> Result<()> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        sqlx::query("DELETE FROM sg_embeddings")
            .execute(&mut *conn)
            .await
            .map_err(db_err)?;
        if self.vss_available.load(Ordering::SeqCst) {
            sqlx::query("DELETE FROM sg_vss")
                .execute(&mut *conn)
                .await
                .map_err(db_err)?;
        }
        Ok(())
    }

    async fn knn(&self, query_vec: &[f32], k: usize) -> Result<Option<Vec<(u64, f32)>>> {
        if !self.vss_available.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        // `vss_search(vec, <query>)` trả các row gần nhất + `distance` (nhỏ = gần).
        // Đảo dấu distance → `sim` (lớn = gần) đồng nhất với `VectorIndex::knn`.
        let rows: Vec<(i64, f64)> = sqlx::query_as(
            "SELECT rowid, distance FROM sg_vss
             WHERE vss_search(vec, ?) ORDER BY distance LIMIT ?",
        )
        .bind(encode_vector(query_vec))
        .bind(k as i64)
        .fetch_all(&mut *conn)
        .await
        .map_err(db_err)?;
        Ok(Some(
            rows.into_iter()
                .map(|(id, dist)| (id as u64, -dist as f32))
                .collect(),
        ))
    }
}

use super::Storage;

#[async_trait]
impl Storage for SqliteStorage {}

// ==================== SqliteTx ====================

/// Transaction cho `SqliteStorage`: buffer toàn bộ mutation, áp dụng atomic
/// trong một SQLite transaction tại `commit`.
///
/// `new_node` đọc counter mới mỗi lần gọi, `id = next + nodes.len()` — giống
/// `RedisTx`; `commit` bump counter lên `max(reserved) + 1` (dùng `MAX` để
/// không hạ counter nếu writer khác đã bump) nên id không bao giờ trùng.
pub struct SqliteTx {
    pool: SqlitePool,
    nodes: Vec<(usize, Vec<u8>, usize)>,
    ops: Vec<TxOp>,
}

#[async_trait]
impl Tx for SqliteTx {
    async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize> {
        let mut conn = self.pool.acquire().await.map_err(db_err)?;
        // Cấp id atomic ngay tại lúc reservation — không `SELECT next` rồi tự
        // tính (đọc-then-giữ nếu 2 tx/writer chạy song song trên cùng db sẽ cấp
        // trùng id → `UNIQUE constraint failed: rt_nodes.id` — bug E). Bản thân
        // các row vẫn được materialize ở commit, nhưng id đã unique toàn cục.
        let next: i64 = sqlx::query_scalar(
            "UPDATE rt_counter SET next = next + 1 WHERE id = 1 RETURNING next - 1",
        )
        .fetch_one(&mut *conn)
        .await
        .map_err(db_err)?;
        let id = next as usize;
        self.nodes.push((id, prefix, record));
        Ok(id)
    }

    async fn update_node(
        &mut self,
        id: usize,
        prefix: Option<Vec<u8>>,
        record: Option<usize>,
    ) -> Result<()> {
        self.ops.push(TxOp::UpdateNode { id, prefix, record });
        Ok(())
    }

    async fn add_child(&mut self, parent: usize, child: usize) -> Result<()> {
        self.ops.push(TxOp::AddChild { parent, child });
        Ok(())
    }

    async fn move_child(&mut self, from: usize, to: usize, child: usize) -> Result<()> {
        self.ops.push(TxOp::MoveChild { from, to, child });
        Ok(())
    }

    async fn commit(self: Box<Self>) -> Result<()> {
        let SqliteTx { pool, nodes, ops } = *self;
        let mut tx = pool.begin().await.map_err(db_err)?;

        // 1. Materialize node mới trước — để ops add/move trỏ tới hợp lệ.
        for (id, prefix, record) in &nodes {
            sqlx::query("INSERT INTO rt_nodes (id, prefix, record) VALUES (?1, ?2, ?3)")
                .bind(*id as i64)
                .bind(prefix)
                .bind(*record as i64)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
        }

        // 2. Bump counter lên max(reserved) + 1 — id tx cấp vẫn unique.
        if let Some(max_id) = nodes.iter().map(|(id, _, _)| *id).max() {
            sqlx::query("UPDATE rt_counter SET next = MAX(next, ?1) WHERE id = 1")
                .bind((max_id + 1) as i64)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
        }

        // 3. Áp dụng toàn bộ ops — atomic, không lộ trạng thái trung gian.
        for op in ops {
            match op {
                TxOp::AddChild { parent, child } => {
                    sqlx::query(
                        "INSERT INTO rt_children (parent, child) VALUES (?1, ?2)
                         ON CONFLICT DO NOTHING",
                    )
                    .bind(parent as i64)
                    .bind(child as i64)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
                }
                TxOp::MoveChild { from, to, child } => {
                    sqlx::query("DELETE FROM rt_children WHERE parent = ?1 AND child = ?2")
                        .bind(from as i64)
                        .bind(child as i64)
                        .execute(&mut *tx)
                        .await
                        .map_err(db_err)?;
                    sqlx::query(
                        "INSERT INTO rt_children (parent, child) VALUES (?1, ?2)
                         ON CONFLICT DO NOTHING",
                    )
                    .bind(to as i64)
                    .bind(child as i64)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
                }
                TxOp::UpdateNode { id, prefix, record } => {
                    if let Some(p) = prefix {
                        sqlx::query("UPDATE rt_nodes SET prefix = ?1 WHERE id = ?2")
                            .bind(p)
                            .bind(id as i64)
                            .execute(&mut *tx)
                            .await
                            .map_err(db_err)?;
                    }
                    if let Some(r) = record {
                        sqlx::query("UPDATE rt_nodes SET record = ?1 WHERE id = ?2")
                            .bind(r as i64)
                            .bind(id as i64)
                            .execute(&mut *tx)
                            .await
                            .map_err(db_err)?;
                    }
                }
            }
        }

        tx.commit().await.map_err(db_err)?;
        Ok(())
    }
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqlite");
        let path = path.to_string_lossy().into_owned();
        (dir, path)
    }

    #[tokio::test]
    async fn test_new_node_and_get_node() {
        let (_d, path) = tmp_path();
        let mut s = SqliteStorage::open(&path).await.unwrap();
        let id = s.new_node(b"hello".to_vec(), 42).await.unwrap();
        assert_ne!(id, EMPTY);
        let (prefix, record) = s.get_node(id).await.unwrap();
        assert_eq!(prefix, b"hello");
        assert_eq!(record, 42);
    }

    #[tokio::test]
    async fn test_update_node() {
        let (_d, path) = tmp_path();
        let mut s = SqliteStorage::open(&path).await.unwrap();
        let id = s.new_node(b"init".to_vec(), 1).await.unwrap();
        s.update_node(id, Some(b"updated".to_vec()), Some(99))
            .await
            .unwrap();
        let (prefix, record) = s.get_node(id).await.unwrap();
        assert_eq!(prefix, b"updated");
        assert_eq!(record, 99);
    }

    #[tokio::test]
    async fn test_children_and_roots() {
        let (_d, path) = tmp_path();
        let mut s = SqliteStorage::open(&path).await.unwrap();
        let parent = s.new_node(b"p".to_vec(), 0).await.unwrap();
        let c1 = s.new_node(b"c1".to_vec(), 1).await.unwrap();
        let c2 = s.new_node(b"c2".to_vec(), 2).await.unwrap();
        // Mutate qua Tx — production chỉ đi qua Tx, không có Storage::add_child.
        let mut tx = s.new_tx();
        tx.add_child(parent, c1).await.unwrap();
        tx.add_child(parent, c2).await.unwrap();
        tx.commit().await.unwrap();
        let children = s.get_children(parent).await.unwrap();
        assert_eq!(children.len(), 2);
        assert!(children.contains(&c1));
        assert!(children.contains(&c2));

        assert_eq!(s.get_root(3).await.unwrap(), EMPTY);
        s.set_root(3, parent).await.unwrap();
        assert_eq!(s.get_root(3).await.unwrap(), parent);
    }

    #[tokio::test]
    async fn test_meta_roundtrip() {
        let (_d, path) = tmp_path();
        let mut s = SqliteStorage::open(&path).await.unwrap();
        assert_eq!(s.get_meta(7).await.unwrap(), None);
        assert_eq!(s.get_key_len(7).await.unwrap(), None);
        s.set_meta(7, b"call-site-info").await.unwrap();
        s.set_key_len(7, 5).await.unwrap();
        assert_eq!(
            s.get_meta(7).await.unwrap().as_deref(),
            Some(b"call-site-info".as_slice())
        );
        assert_eq!(s.get_key_len(7).await.unwrap(), Some(5));
        s.set_meta(7, b"updated").await.unwrap();
        s.set_key_len(7, 6).await.unwrap();
        assert_eq!(
            s.get_meta(7).await.unwrap().as_deref(),
            Some(b"updated".as_slice())
        );
        assert_eq!(s.get_key_len(7).await.unwrap(), Some(6));
        assert_eq!(s.get_meta(8).await.unwrap(), None);
        assert_eq!(s.get_key_len(8).await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_shortcuts_roundtrip() {
        let (_d, path) = tmp_path();
        let mut s = SqliteStorage::open(&path).await.unwrap();
        assert!(s.get_shortcut_nodes(1, b"l").await.unwrap().is_empty());
        s.add_shortcut_node(1, b"l", 10).await.unwrap();
        s.add_shortcut_node(1, b"l", 20).await.unwrap();
        s.add_shortcut_node(1, b"o", 10).await.unwrap();
        s.add_shortcut_node(2, b"l", 30).await.unwrap(); // shard khác
        let nodes = s.get_shortcut_nodes(1, b"l").await.unwrap();
        assert!(nodes.contains(&10) && nodes.contains(&20));
        assert_eq!(nodes.len(), 2);
        assert_eq!(s.get_shortcut_nodes(2, b"l").await.unwrap(), vec![30]);

        s.clear_shortcuts().await.unwrap();
        assert!(s.get_shortcut_nodes(1, b"l").await.unwrap().is_empty());
        assert!(s.get_shortcut_nodes(2, b"l").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_tx_commit_applies_atomically() {
        let (_d, path) = tmp_path();
        let mut s = SqliteStorage::open(&path).await.unwrap();
        let parent = s.new_node(b"hello".to_vec(), 1).await.unwrap();

        let mut tx = s.new_tx();
        let new_id = tx.new_node(b"p".to_vec(), 2).await.unwrap();
        let leg_id = tx.new_node(b"lo".to_vec(), 1).await.unwrap();
        tx.move_child(parent, leg_id, 0).await.unwrap(); // no-op: 0 chưa phải child
        tx.add_child(parent, leg_id).await.unwrap();
        tx.add_child(parent, new_id).await.unwrap();
        tx.update_node(parent, Some(b"hel".to_vec()), Some(0))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let (prefix, record) = s.get_node(parent).await.unwrap();
        assert_eq!(prefix, b"hel");
        assert_eq!(record, 0);
        let children = s.get_children(parent).await.unwrap();
        assert!(children.contains(&leg_id));
        assert!(children.contains(&new_id));
        assert_eq!(s.get_node(new_id).await.unwrap().1, 2);
        assert_eq!(s.get_node(leg_id).await.unwrap().1, 1);
    }

    #[tokio::test]
    async fn test_tx_nodes_invisible_before_commit() {
        let (_d, path) = tmp_path();
        let s = SqliteStorage::open(&path).await.unwrap();
        let mut tx = s.new_tx();
        let id = tx.new_node(b"pending".to_vec(), 9).await.unwrap();
        // Trước commit, node chưa materialize → get_node lỗi BranchOutOfRange.
        assert!(s.get_node(id).await.is_err());
        tx.commit().await.unwrap();
        assert_eq!(s.get_node(id).await.unwrap().1, 9);
    }

    /// Regression bug E: `UNIQUE constraint failed: rt_nodes.id` khi nhiều tx
    /// (2 writer / watcher + mcp chạy cùng db.sqlite) cấp id node song song.
    /// `new_node` phải cấp id atomic qua `UPDATE rt_counter ... RETURNING`,
    /// không đọc-then-tính (`SELECT next` + `next + nodes.len()`) dễ trùng.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_tx_new_node_ids_unique() {
        use std::collections::HashSet;
        use std::sync::Arc;
        let (_d, path) = tmp_path();
        let s = Arc::new(SqliteStorage::open(&path).await.unwrap());

        let mut handles = Vec::new();
        for w in 0..8 {
            let s = Arc::clone(&s);
            handles.push(tokio::spawn(async move {
                let mut tx = s.new_tx();
                let mut ids = Vec::new();
                for i in 0..8 {
                    let prefix = format!("w{w}-{i}").into_bytes();
                    ids.push(tx.new_node(prefix, 1).await.unwrap());
                }
                tx.commit().await.unwrap();
                ids
            }));
        }

        let mut all = Vec::new();
        for h in handles {
            all.extend(h.await.unwrap());
        }
        let unique: HashSet<usize> = all.iter().copied().collect();
        assert_eq!(
            unique.len(),
            all.len(),
            "duplicate rt node ids allocated across concurrent transactions: {all:?}"
        );
        // Toàn bộ node đã materialize hợp lệ (commit không UNIQUE-fail).
        for id in all {
            s.get_node(id).await.expect("committed node readable");
        }
    }

    #[tokio::test]
    async fn test_tx_move_child_migrates() {
        let (_d, path) = tmp_path();
        let mut s = SqliteStorage::open(&path).await.unwrap();
        let parent = s.new_node(b"aaaaaa".to_vec(), 0).await.unwrap();
        let child = s.new_node(b"0".to_vec(), 1).await.unwrap();
        let mut seed = s.new_tx();
        seed.add_child(parent, child).await.unwrap();
        seed.commit().await.unwrap();

        let mut tx = s.new_tx();
        let leg = tx.new_node(b"a".to_vec(), 0).await.unwrap();
        tx.move_child(parent, leg, child).await.unwrap();
        tx.add_child(parent, leg).await.unwrap();
        tx.commit().await.unwrap();

        assert!(!s.get_children(parent).await.unwrap().contains(&child));
        assert!(s.get_children(leg).await.unwrap().contains(&child));
    }

    #[tokio::test]
    async fn test_edge_data_roundtrip() {
        let (_d, path) = tmp_path();
        let mut s = SqliteStorage::open(&path).await.unwrap();
        assert_eq!(s.get_edge_data(7).await.unwrap(), None);
        s.set_edge_data(7, b"call-site").await.unwrap();
        assert_eq!(
            s.get_edge_data(7).await.unwrap().as_deref(),
            Some(b"call-site".as_slice())
        );
        // Overwrite.
        s.set_edge_data(7, b"call-site-2").await.unwrap();
        assert_eq!(
            s.get_edge_data(7).await.unwrap().as_deref(),
            Some(b"call-site-2".as_slice())
        );
        s.clear_edges().await.unwrap();
        assert_eq!(s.get_edge_data(7).await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_node_meta_roundtrip() {
        let (_d, path) = tmp_path();
        let mut s = SqliteStorage::open(&path).await.unwrap();
        assert_eq!(s.get_node_meta(3).await.unwrap(), None);
        s.set_node_meta(3, b"node-json").await.unwrap();
        assert_eq!(
            s.get_node_meta(3).await.unwrap().as_deref(),
            Some(b"node-json".as_slice())
        );
        s.set_node_meta(3, b"node-json-2").await.unwrap();
        assert_eq!(
            s.get_node_meta(3).await.unwrap().as_deref(),
            Some(b"node-json-2".as_slice())
        );
        assert_eq!(s.get_node_meta(4).await.unwrap(), None);
        s.clear_node_meta().await.unwrap();
        assert_eq!(s.get_node_meta(3).await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_chains_roundtrip() {
        let (_d, path) = tmp_path();
        let mut s = SqliteStorage::open(&path).await.unwrap();
        assert_eq!(s.get_chain(9).await.unwrap(), None);
        s.set_chain(9, &[1, 2, 3]).await.unwrap();
        assert_eq!(s.get_chain(9).await.unwrap(), Some(vec![1, 2, 3]));
        s.set_chain(9, &[4]).await.unwrap();
        assert_eq!(s.get_chain(9).await.unwrap(), Some(vec![4]));
        assert_eq!(s.get_chain(10).await.unwrap(), None);
        s.clear_chains().await.unwrap();
        assert_eq!(s.get_chain(9).await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_embeddings_roundtrip() {
        let (_d, path) = tmp_path();
        // Save embeddings, then reload — verify BLOB persistence (dùng lại cho
        // KNN/k-means mà không re-embed).
        let mut s = SqliteStorage::open(&path).await.unwrap();
        let v1 = vec![0.1f32, 0.2, 0.3, -0.4];
        let v2 = vec![1.0f32, -1.0, 0.0, 0.5];
        s.save_embedding(100, &v1).await.unwrap();
        s.save_embedding(101, &v2).await.unwrap();
        // upsert overwrite cho 100.
        s.save_embedding(100, &v2).await.unwrap();

        let all = s.load_all_embeddings().await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(
            all.get(&100).unwrap(),
            &v2,
            "id 100 phải bị overwrite thành v2"
        );
        assert_eq!(all.get(&101).unwrap(), &v2, "id 101 giữ v2");
        assert_eq!(s.load_embedding(100).await.unwrap().unwrap(), v2);
        assert_eq!(s.load_embedding(101).await.unwrap().unwrap(), v2);

        // clear → rỗng
        s.clear_embeddings().await.unwrap();
        assert!(s.load_all_embeddings().await.unwrap().is_empty());
        assert_eq!(s.load_embedding(101).await.unwrap(), None);
    }

    /// KNN qua sqlite-vss (`vss0`) — chỉ chạy khi extension thực sự có mặt
    /// (`vector0`/`vss0` trong `vss_extension` config hoặc `<cache_dir>/vss`).
    /// Thiếu extension → skip (KNN lúc đó fallback brute-force in-memory).
    #[tokio::test]
    async fn test_vss_knn_when_extension_present() {
        if crate::embeddings::resolve_vss_extensions().is_none() {
            return;
        }
        let (_d, path) = tmp_path();
        let mut s = SqliteStorage::open(&path).await.unwrap();
        // Hai vector 384-dim: `a` cùng chiều với query, `b` ngược chiều.
        let a: Vec<f32> = vec![1.0; 384];
        let b: Vec<f32> = vec![-1.0; 384];
        let q: Vec<f32> = vec![1.0; 384];
        s.save_embedding(1, &a).await.unwrap();
        s.save_embedding(2, &b).await.unwrap();
        let hits = s.knn(&q, 2).await.unwrap();
        let hits = hits.expect("vss phải khả dụng khi extension có mặt");
        assert_eq!(hits.len(), 2);
        // Gần nhất với query (1,1,...) phải là `a` (id 1), không phải `b`.
        assert_eq!(hits[0].0, 1, "vss KNN phải trả symbol gần nhất trước");
        assert!(hits[0].1 > hits[1].1, "similarity phải giảm dần");
    }

    #[tokio::test]
    async fn test_persists_across_reopen() {
        let (_d, path) = tmp_path();
        let parent;
        {
            let mut s = SqliteStorage::open(&path).await.unwrap();
            parent = s.new_node(b"hello".to_vec(), 42).await.unwrap();
            let child = s.new_node(b"world".to_vec(), 7).await.unwrap();
            let mut seed = s.new_tx();
            seed.add_child(parent, child).await.unwrap();
            seed.commit().await.unwrap();
            s.set_root(3, parent).await.unwrap();
            s.set_meta(42, b"meta-42").await.unwrap();
            s.set_key_len(42, 5).await.unwrap();
            s.add_shortcut_node(1, b"h", parent).await.unwrap();
            s.set_node_meta(100, b"node-json").await.unwrap();
            s.set_chain(42, &[100, 101]).await.unwrap();

            let mut tx = s.new_tx();
            let extra = tx.new_node(b"z".to_vec(), 99).await.unwrap();
            tx.add_child(parent, extra).await.unwrap();
            tx.commit().await.unwrap();
        } // drop storage → pool đóng

        // Reopen: dữ liệu phải còn nguyên.
        let mut s = SqliteStorage::open(&path).await.unwrap();
        let (prefix, record) = s.get_node(parent).await.unwrap();
        assert_eq!(prefix, b"hello");
        assert_eq!(record, 42);
        assert_eq!(s.get_root(3).await.unwrap(), parent);
        assert_eq!(
            s.get_meta(42).await.unwrap().as_deref(),
            Some(b"meta-42".as_slice())
        );
        assert_eq!(s.get_key_len(42).await.unwrap(), Some(5));
        assert_eq!(
            s.get_node_meta(100).await.unwrap().as_deref(),
            Some(b"node-json".as_slice())
        );
        assert_eq!(s.get_chain(42).await.unwrap(), Some(vec![100, 101]));
        assert!(
            s.get_shortcut_nodes(1, b"h")
                .await
                .unwrap()
                .contains(&parent)
        );
        // Children gồm cả node tạo bằng tx (persist qua commit).
        let children = s.get_children(parent).await.unwrap();
        assert_eq!(children.len(), 2);
        // Node id mới tiếp tục cấp trên counter đã persist.
        let n = s.new_node(b"new".to_vec(), 1).await.unwrap();
        assert!(n > parent);
    }

    #[tokio::test]
    async fn test_stats_roundtrip() {
        let (_d, path) = tmp_path();
        let mut s = SqliteStorage::open(&path).await.unwrap();
        s.init().await.unwrap();
        // Chưa ghi → trả zeros (caller fallback rebuild).
        assert_eq!(s.stats().await.unwrap(), IndexCounts::default());
        let counts = IndexCounts {
            symbols: 12,
            chains: 3,
            edges: 5,
            files: 2,
            next_id: 100,
        };
        s.set_stats(counts).await.unwrap();
        let got = s.stats().await.unwrap();
        assert_eq!(got.symbols, 12);
        assert_eq!(got.chains, 3);
        assert_eq!(got.edges, 5);
        assert_eq!(got.files, 2);
        assert_eq!(got.next_id, 100);
        // Ghi lại đè → UPSERT cập nhật (không duplicate row).
        s.set_stats(IndexCounts {
            symbols: 99,
            ..IndexCounts::default()
        })
        .await
        .unwrap();
        assert_eq!(s.stats().await.unwrap().symbols, 99);
    }
}
