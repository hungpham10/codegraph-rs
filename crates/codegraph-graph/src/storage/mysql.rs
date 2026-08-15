use async_trait::async_trait;
use sqlx::{mysql::MySqlPoolOptions, MySqlPool};
use super::{Result, Storage, StorageError, Tx, EMPTY};

/// MySQL implementation of the `Storage` trait.
/// The schema mirrors the SQLite version, adjusted for MySQL syntax.
pub struct MySqlStorage {
    pool: MySqlPool,
}

impl MySqlStorage {
    /// Open a MySQL connection pool. `dsn` must be a valid MySQL URL
    /// (e.g. `mysql://user:pass@host:3306/db`). No automatic initialization –
    /// the schema should be applied manually (e.g. via the migration files).
    pub async fn open(dsn: &str) -> Result<Self> {
        let pool = MySqlPoolOptions::new()
            .max_connections(5)
            .connect(dsn)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(Self { pool })
    }

    // MySQL returns a `u64` for `LAST_INSERT_ID`, but our node ids live in a
    // central `rt_counter` table shared by all shards. Mirror the sequence used
    // by the other backends: read the current `next`, then bump it.
    async fn reserve_node_id(&self) -> Result<usize> {
        let row: (i64,) = sqlx::query_as("SELECT next FROM rt_counter WHERE id = 1")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        let id = row.0 as usize;
        sqlx::query("UPDATE rt_counter SET next = next + 1 WHERE id = 1")
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(id)
    }
}

#[async_trait]
impl Storage for MySqlStorage {
    async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize> {
        let id = self.reserve_node_id().await?;
        sqlx::query("INSERT INTO rt_nodes (id, prefix, record) VALUES (?, ?, ?)")
            .bind(id as i64)
            .bind(prefix)
            .bind(record as i64)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(id)
    }

    async fn update_node(
        &mut self,
        id: usize,
        prefix: Option<Vec<u8>>,
        record: Option<usize>,
    ) -> Result<()> {
        if let Some(p) = prefix {
            sqlx::query("UPDATE rt_nodes SET prefix = ? WHERE id = ?")
                .bind(p)
                .bind(id as i64)
                .execute(&self.pool)
                .await
                .map_err(|e| StorageError::Internal(e.to_string()))?;
        }
        if let Some(r) = record {
            sqlx::query("UPDATE rt_nodes SET record = ? WHERE id = ?")
                .bind(r as i64)
                .bind(id as i64)
                .execute(&self.pool)
                .await
                .map_err(|e| StorageError::Internal(e.to_string()))?;
        }
        Ok(())
    }

    async fn get_node(&self, id: usize) -> Result<(Vec<u8>, usize)> {
        let row = sqlx::query_as::<_, (Vec<u8>, i64)>(
            "SELECT prefix, record FROM rt_nodes WHERE id = ?",
        )
        .bind(id as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        let Some((prefix, record)) = row else {
            return Err(StorageError::BranchOutOfRange(id));
        };
        Ok((prefix, record as usize))
    }

    async fn get_children(&self, id: usize) -> Result<Vec<usize>> {
        let rows = sqlx::query_as::<_, (i64,)>(
            "SELECT child FROM rt_children WHERE parent = ? ORDER BY child",
        )
        .bind(id as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(rows.into_iter().map(|(c,)| c as usize).collect())
    }

    async fn set_root(&mut self, shard: usize, root: usize) -> Result<()> {
        sqlx::query(
            "INSERT INTO rt_roots (shard, root) VALUES (?, ?) \
             ON DUPLICATE KEY UPDATE root = VALUES(root)",
        )
        .bind(shard as i64)
        .bind(root as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_root(&self, shard: usize) -> Result<usize> {
        let row = sqlx::query_as::<_, (i64,)>("SELECT root FROM rt_roots WHERE shard = ?")
            .bind(shard as i64)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        let Some((root,)) = row else {
            return Err(StorageError::BranchOutOfRange(shard));
        };
        Ok(root as usize)
    }

    async fn set_meta(&mut self, record: usize, meta: &[u8]) -> Result<()> {
        sqlx::query(
            "INSERT INTO rt_meta (record, meta) VALUES (?, ?) \
             ON DUPLICATE KEY UPDATE meta = VALUES(meta)",
        )
        .bind(record as i64)
        .bind(meta)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_meta(&self, record: usize) -> Result<Option<Vec<u8>>> {
        let row = sqlx::query_as::<_, (Vec<u8>,)>("SELECT meta FROM rt_meta WHERE record = ?")
            .bind(record as i64)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(row.map(|(m,)| m))
    }

    async fn set_key_len(&mut self, record: usize, len: usize) -> Result<()> {
        sqlx::query(
            "INSERT INTO rt_keylen (record, len) VALUES (?, ?) \
             ON DUPLICATE KEY UPDATE len = VALUES(len)",
        )
        .bind(record as i64)
        .bind(len as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_key_len(&self, record: usize) -> Result<Option<usize>> {
        let row = sqlx::query_as::<_, (i64,)>("SELECT len FROM rt_keylen WHERE record = ?")
            .bind(record as i64)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(row.map(|(len,)| len as usize))
    }

    async fn add_shortcut_node(&mut self, shard: usize, elem: &[u8], node_id: usize) -> Result<()> {
        sqlx::query(
            "INSERT IGNORE INTO rt_shortcuts (shard, elem, node_id) VALUES (?, ?, ?)",
        )
        .bind(shard as i64)
        .bind(elem)
        .bind(node_id as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_shortcut_nodes(&self, shard: usize, elem: &[u8]) -> Result<Vec<usize>> {
        let rows = sqlx::query_as::<_, (i64,)>(
            "SELECT node_id FROM rt_shortcuts WHERE shard = ? AND elem = ?",
        )
        .bind(shard as i64)
        .bind(elem)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(rows.into_iter().map(|(id,)| id as usize).collect())
    }

    async fn clear_shortcuts(&mut self) -> Result<()> {
        sqlx::query("DELETE FROM rt_shortcuts")
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn set_edge_data(&mut self, edge: usize, data: &[u8]) -> Result<()> {
        sqlx::query(
            "INSERT INTO rt_edges (id, data) VALUES (?, ?) \
             ON DUPLICATE KEY UPDATE data = VALUES(data)",
        )
        .bind(edge as i64)
        .bind(data)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_edge_data(&self, edge: usize) -> Result<Option<Vec<u8>>> {
        let row = sqlx::query_as::<_, (Vec<u8>,)>("SELECT data FROM rt_edges WHERE id = ?")
            .bind(edge as i64)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(row.map(|(d,)| d))
    }

    async fn clear_edges(&mut self) -> Result<()> {
        sqlx::query("DELETE FROM rt_edges")
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    // The remaining methods are either no‑ops or can be forwarded to other
    // storage implementations if needed. For now we keep the minimal set.
    async fn set_node_meta(&mut self, _elem: usize, _meta: &[u8]) -> Result<()> { Ok(()) }
    async fn get_node_meta(&self, _elem: usize) -> Result<Option<Vec<u8>>> { Ok(None) }
    async fn clear_node_meta(&mut self) -> Result<()> { Ok(()) }
    async fn set_chain(&mut self, _record: usize, _chain: &[u64]) -> Result<()> { Ok(()) }
    async fn get_chain(&self, _record: usize) -> Result<Option<Vec<u64>>> { Ok(None) }
    async fn clear_chains(&mut self) -> Result<()> { Ok(()) }
    async fn save_symbol(&mut self, _sym: &codegraph_core::Symbol) -> Result<()> { Ok(()) }
    async fn load_symbol(&self, _id: u64) -> Result<Option<codegraph_core::Symbol>> { Ok(None) }
}
