use async_trait::async_trait;
use sqlx::{postgres::PgPoolOptions, PgPool};
use super::{Result, Storage, StorageError, Tx, EMPTY};

/// PostgreSQL implementation of the `Storage` trait.
/// The schema mirrors the SQLite version, adjusted for PostgreSQL syntax.
pub struct PostgresStorage {
    pool: PgPool,
}

impl PostgresStorage {
    /// Open a PostgreSQL connection pool. `dsn` must be a valid Postgres URL
    /// (e.g. `postgres://user:pass@host:5432/db`). No automatic initialization –
    /// the schema should be applied manually (e.g. via the migration files).
    pub async fn open(dsn: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(dsn)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(Self { pool })
    }


    async fn init(&mut self) -> Result<()> {
        // Same tables as SQLite, using PostgreSQL types.
        for stmt in [
            "CREATE TABLE IF NOT EXISTS rt_nodes (\n                id BIGSERIAL PRIMARY KEY,\n                prefix BYTEA NOT NULL,\n                record BIGINT NOT NULL\n            )",
            "CREATE TABLE IF NOT EXISTS rt_children (\n                parent BIGINT NOT NULL,\n                child BIGINT NOT NULL,\n                PRIMARY KEY (parent, child)\n            )",
            "CREATE INDEX IF NOT EXISTS idx_rt_children_parent ON rt_children(parent)",
            "CREATE TABLE IF NOT EXISTS rt_roots (\n                shard BIGINT PRIMARY KEY,\n                root BIGINT NOT NULL\n            )",
            "CREATE TABLE IF NOT EXISTS rt_meta (\n                record BIGINT PRIMARY KEY,\n                meta BYTEA NOT NULL\n            )",
            "CREATE TABLE IF NOT EXISTS rt_keylen (\n                record BIGINT PRIMARY KEY,\n                len BIGINT NOT NULL\n            )",
            "CREATE TABLE IF NOT EXISTS rt_shortcuts (\n                shard BIGINT NOT NULL,\n                elem BYTEA NOT NULL,\n                node_id BIGINT NOT NULL,\n                PRIMARY KEY (shard, elem, node_id)\n            )",
            "CREATE INDEX IF NOT EXISTS idx_rt_shortcuts_lookup ON rt_shortcuts(shard, elem)",
            "CREATE TABLE IF NOT EXISTS rt_edges (\n                id BIGINT PRIMARY KEY,\n                data BYTEA NOT NULL\n            )",
            "CREATE TABLE IF NOT EXISTS rt_node_meta (\n                elem BIGINT PRIMARY KEY,\n                meta BYTEA NOT NULL\n            )",
            "CREATE TABLE IF NOT EXISTS rt_chains (\n                record BIGINT PRIMARY KEY,\n                chain BYTEA NOT NULL\n            )",
            "CREATE TABLE IF NOT EXISTS rt_counter (\n                id BIGINT PRIMARY KEY CHECK (id = 1),\n                next BIGINT NOT NULL\n            )",
            // Entity tables needed for the rest of the graph.
            "CREATE TABLE IF NOT EXISTS sg_symbols (\n                id BIGINT PRIMARY KEY,\n                data BYTEA NOT NULL\n            )",
            "CREATE TABLE IF NOT EXISTS sg_next_id (\n                id BIGINT PRIMARY KEY CHECK (id = 1),\n                next BIGINT NOT NULL\n            )",
            "CREATE TABLE IF NOT EXISTS sg_call_records (\n                func BIGINT PRIMARY KEY,\n                records BYTEA NOT NULL\n            )",
            "CREATE TABLE IF NOT EXISTS sg_call_names (\n                name TEXT PRIMARY KEY,\n                sites BYTEA NOT NULL\n            )",
            "CREATE TABLE IF NOT EXISTS sg_files (\n                path TEXT PRIMARY KEY,\n                language TEXT NOT NULL,\n                bytes BIGINT NOT NULL,\n                lines BIGINT NOT NULL\n            )",
            "CREATE TABLE IF NOT EXISTS sg_meta (\n                id BIGINT PRIMARY KEY CHECK (id = 1),\n                version BIGINT NOT NULL\n            )",
            // Initialise counters if they do not exist.
            "INSERT INTO rt_counter (id, next) VALUES (1, 1) ON CONFLICT (id) DO NOTHING",
            "INSERT INTO sg_next_id (id, next) VALUES (1, 100) ON CONFLICT (id) DO NOTHING",
            "INSERT INTO sg_meta (id, version) VALUES (1, 0) ON CONFLICT (id) DO NOTHING",
        ].iter() {
            sqlx::query(stmt)
                .execute(&self.pool)
                .await
                .map_err(|e| StorageError::Internal(e.to_string()))?;
        }
        Ok(())
    }
}

#[async_trait]
impl Storage for PostgresStorage {
    async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize> {
        // Reserve an id via the counter table.
        let row: (i64,) = sqlx::query_as(
            "UPDATE rt_counter SET next = next + 1 WHERE id = 1 RETURNING next - 1",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        let id = row.0 as usize;
        sqlx::query("INSERT INTO rt_nodes (id, prefix, record) VALUES ($1, $2, $3)")
            .bind(id as i64)
            .bind(prefix)
            .bind(record as i64)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(id)
    }

    async fn update_node(&mut self, id: usize, prefix: Option<Vec<u8>>, record: Option<usize>) -> Result<()> {
        if let Some(p) = prefix {
            sqlx::query("UPDATE rt_nodes SET prefix = $1 WHERE id = $2")
                .bind(p)
                .bind(id as i64)
                .execute(&self.pool)
                .await
                .map_err(|e| StorageError::Internal(e.to_string()))?;
        }
        if let Some(r) = record {
            sqlx::query("UPDATE rt_nodes SET record = $1 WHERE id = $2")
                .bind(r as i64)
                .bind(id as i64)
                .execute(&self.pool)
                .await
                .map_err(|e| StorageError::Internal(e.to_string()))?;
        }
        Ok(())
    }

    async fn get_node(&self, id: usize) -> Result<(Vec<u8>, usize)> {
        let row = sqlx::query_as::<_, (Vec<u8>, i64)>("SELECT prefix, record FROM rt_nodes WHERE id = $1")
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
        let rows = sqlx::query_as::<_, (i64,)>("SELECT child FROM rt_children WHERE parent = $1 ORDER BY child")
            .bind(id as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(rows.into_iter().map(|(c,)| c as usize).collect())
    }

    async fn set_root(&mut self, shard: usize, root: usize) -> Result<()> {
        sqlx::query(
            "INSERT INTO rt_roots (shard, root) VALUES ($1, $2) ON CONFLICT (shard) DO UPDATE SET root = EXCLUDED.root",
        )
        .bind(shard as i64)
        .bind(root as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_root(&self, shard: usize) -> Result<usize> {
        let row = sqlx::query_as::<_, (i64,)>("SELECT root FROM rt_roots WHERE shard = $1")
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
            "INSERT INTO rt_meta (record, meta) VALUES ($1, $2) ON CONFLICT (record) DO UPDATE SET meta = EXCLUDED.meta",
        )
        .bind(record as i64)
        .bind(meta)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_meta(&self, record: usize) -> Result<Option<Vec<u8>>> {
        let row = sqlx::query_as::<_, (Vec<u8>,)>("SELECT meta FROM rt_meta WHERE record = $1")
            .bind(record as i64)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(row.map(|(m,)| m))
    }

    async fn set_key_len(&mut self, record: usize, len: usize) -> Result<()> {
        sqlx::query(
            "INSERT INTO rt_keylen (record, len) VALUES ($1, $2) ON CONFLICT (record) DO UPDATE SET len = EXCLUDED.len",
        )
        .bind(record as i64)
        .bind(len as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_key_len(&self, record: usize) -> Result<Option<usize>> {
        let row = sqlx::query_as::<_, (i64,)>("SELECT len FROM rt_keylen WHERE record = $1")
            .bind(record as i64)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(row.map(|(len,)| len as usize))
    }

    async fn add_shortcut_node(&mut self, shard: usize, elem: &[u8], node_id: usize) -> Result<()> {
        sqlx::query(
            "INSERT INTO rt_shortcuts (shard, elem, node_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
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
        let rows = sqlx::query_as::<_, (i64,)>("SELECT node_id FROM rt_shortcuts WHERE shard = $1 AND elem = $2")
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
            "INSERT INTO rt_edges (id, data) VALUES ($1, $2) ON CONFLICT (id) DO UPDATE SET data = EXCLUDED.data",
        )
        .bind(edge as i64)
        .bind(data)
        .execute(&self.pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_edge_data(&self, edge: usize) -> Result<Option<Vec<u8>>> {
        let row = sqlx::query_as::<_, (Vec<u8>,)>("SELECT data FROM rt_edges WHERE id = $1")
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
