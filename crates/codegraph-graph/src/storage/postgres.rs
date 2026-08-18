use std::collections::HashMap;

use super::{
    IndexCounts, Result, Storage, StorageError, Tx, decode_chain, decode_vector, encode_chain,
    encode_vector,
};
use async_trait::async_trait;
use codegraph_core::{Annotation, FileInfo, ScopeLevel, Symbol, SymbolKind};
use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::{PgPool, Row};

/// PostgreSQL implementation của `Storage` trait — multi-tenant theo thiết kế
/// `sql/README.md`: mọi bảng dẫn đầu bằng `repo_id`, 1 repository = 1 partition.
///
/// Instance này được **bind vào một `repo_id`** (không đổi signature `Storage`,
/// không động tới backend khác). Schema được apply **thủ công** (user chạy
/// `sql/postgres/001`+`002`); code chỉ seed các runtime row per-repo (counter,
/// sentinel node, version).
pub struct PostgresStorage {
    pool: PgPool,
    repo_id: u64,
}

fn db_err(e: sqlx::Error) -> StorageError {
    StorageError::Internal(e.to_string())
}

fn ser_err(e: impl std::fmt::Display) -> StorageError {
    StorageError::Internal(e.to_string())
}

impl PostgresStorage {
    /// Mở pool + seed per-repo runtime rows. `repo_id` do config/sharding quyết
    /// định (không lấy từ DSN). `dsn` phải là Postgres URL hợp lệ.
    pub async fn open(dsn: &str, repo_id: u64) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(dsn)
            .await
            .map_err(db_err)?;
        let s = Self { pool, repo_id };
        s.ensure_repo_seeded().await?;
        Ok(s)
    }

    /// Idempotent seed runtime row cho 1 repo (theo mẫu "[Seed per repo]" trong
    /// `sql/postgres/001`). KHÔNG tạo schema — schema là manual migration.
    async fn ensure_repo_seeded(&self) -> Result<()> {
        let rid = self.repo_id as i64;
        sqlx::query(
            "INSERT INTO rt_nodes (repo_id, id, prefix, record) VALUES ($1, 0, '', 0) \
             ON CONFLICT (repo_id, id) DO NOTHING",
        )
        .bind(rid)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        sqlx::query(
            "INSERT INTO rt_counter (repo_id, next) VALUES ($1, 1) ON CONFLICT (repo_id) DO NOTHING",
        )
        .bind(rid)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        sqlx::query(
            "INSERT INTO sg_next_id (repo_id, next) VALUES ($1, 100) ON CONFLICT (repo_id) DO NOTHING",
        )
        .bind(rid)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        sqlx::query(
            "INSERT INTO sg_meta (repo_id, version) VALUES ($1, 0) ON CONFLICT (repo_id) DO NOTHING",
        )
        .bind(rid)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        // Embeddings: (repo_id, symbol_id) → vector BLOB. Tạo bảng nếu chưa có
        // (idempotent) để không bắt buộc chạy migration thủ công cho tính năng này.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS sg_embeddings (
                repo_id BIGINT NOT NULL,
                symbol_id BIGINT NOT NULL,
                vector BYTEA NOT NULL,
                PRIMARY KEY (repo_id, symbol_id)
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        // Stats tổng hợp (codegraph_status đọc O(1) không rebuild) — idempotent.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS sg_stats (
                repo_id BIGINT NOT NULL,
                symbols BIGINT NOT NULL,
                chains BIGINT NOT NULL,
                edges BIGINT NOT NULL,
                files BIGINT NOT NULL,
                next_id BIGINT NOT NULL,
                PRIMARY KEY (repo_id)
            )",
        )
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    /// Ghi/ensure row `repos` (registry toàn cục, shard của repo) — idempotent.
    /// Bảng này nằm trong migration `002` (áp dụng thủ công).
    pub async fn ensure_registered(&self, shard: usize, root: Option<&str>) -> Result<()> {
        sqlx::query(
            "INSERT INTO repos (repo_id, shard, root) VALUES ($1, $2, $3) \
             ON CONFLICT (repo_id) DO NOTHING",
        )
        .bind(self.repo_id as i64)
        .bind(shard as i32)
        .bind(root)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    /// Cấp node id nguyên tử, per-repo.
    async fn reserve_node_id(&self) -> Result<usize> {
        let row: (i64,) = sqlx::query_as(
            "UPDATE rt_counter SET next = next + 1 WHERE repo_id = $1 RETURNING next - 1",
        )
        .bind(self.repo_id as i64)
        .fetch_one(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.0 as usize)
    }
}

#[async_trait]
impl Storage for PostgresStorage {
    async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize> {
        let id = self.reserve_node_id().await?;
        sqlx::query(
            "INSERT INTO rt_nodes (repo_id, id, prefix, record) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (repo_id, id) DO NOTHING",
        )
        .bind(self.repo_id as i64)
        .bind(id as i64)
        .bind(prefix)
        .bind(record as i64)
        .execute(&self.pool)
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
        let rid = self.repo_id as i64;
        if let Some(p) = prefix {
            sqlx::query("UPDATE rt_nodes SET prefix = $1 WHERE repo_id = $2 AND id = $3")
                .bind(p)
                .bind(rid)
                .bind(id as i64)
                .execute(&self.pool)
                .await
                .map_err(db_err)?;
        }
        if let Some(r) = record {
            sqlx::query("UPDATE rt_nodes SET record = $1 WHERE repo_id = $2 AND id = $3")
                .bind(r as i64)
                .bind(rid)
                .bind(id as i64)
                .execute(&self.pool)
                .await
                .map_err(db_err)?;
        }
        Ok(())
    }

    async fn get_node(&self, id: usize) -> Result<(Vec<u8>, usize)> {
        let row = sqlx::query_as::<_, (Vec<u8>, i64)>(
            "SELECT prefix, record FROM rt_nodes WHERE repo_id = $1 AND id = $2",
        )
        .bind(self.repo_id as i64)
        .bind(id as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        let Some((prefix, record)) = row else {
            return Err(StorageError::BranchOutOfRange(id));
        };
        Ok((prefix, record as usize))
    }

    async fn get_children(&self, id: usize) -> Result<Vec<usize>> {
        let rows = sqlx::query_as::<_, (i64,)>(
            "SELECT child FROM rt_children WHERE repo_id = $1 AND parent = $2 ORDER BY child",
        )
        .bind(self.repo_id as i64)
        .bind(id as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.into_iter().map(|(c,)| c as usize).collect())
    }

    async fn set_root(&mut self, shard: usize, root: usize) -> Result<()> {
        sqlx::query(
            "INSERT INTO rt_roots (repo_id, shard, root) VALUES ($1, $2, $3) \
             ON CONFLICT (repo_id, shard) DO UPDATE SET root = EXCLUDED.root",
        )
        .bind(self.repo_id as i64)
        .bind(shard as i32)
        .bind(root as i64)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn get_root(&self, shard: usize) -> Result<usize> {
        let row = sqlx::query_as::<_, (i64,)>(
            "SELECT root FROM rt_roots WHERE repo_id = $1 AND shard = $2",
        )
        .bind(self.repo_id as i64)
        .bind(shard as i32)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        let Some((root,)) = row else {
            return Err(StorageError::BranchOutOfRange(shard));
        };
        Ok(root as usize)
    }

    async fn set_meta(&mut self, record: usize, meta: &[u8]) -> Result<()> {
        sqlx::query(
            "INSERT INTO rt_meta (repo_id, record, meta) VALUES ($1, $2, $3) \
             ON CONFLICT (repo_id, record) DO UPDATE SET meta = EXCLUDED.meta",
        )
        .bind(self.repo_id as i64)
        .bind(record as i64)
        .bind(meta)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn get_meta(&self, record: usize) -> Result<Option<Vec<u8>>> {
        let row = sqlx::query_as::<_, (Vec<u8>,)>(
            "SELECT meta FROM rt_meta WHERE repo_id = $1 AND record = $2",
        )
        .bind(self.repo_id as i64)
        .bind(record as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|(m,)| m))
    }

    async fn set_key_len(&mut self, record: usize, len: usize) -> Result<()> {
        sqlx::query(
            "INSERT INTO rt_keylen (repo_id, record, len) VALUES ($1, $2, $3) \
             ON CONFLICT (repo_id, record) DO UPDATE SET len = EXCLUDED.len",
        )
        .bind(self.repo_id as i64)
        .bind(record as i64)
        .bind(len as i32)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn get_key_len(&self, record: usize) -> Result<Option<usize>> {
        let row = sqlx::query_as::<_, (i32,)>(
            "SELECT len FROM rt_keylen WHERE repo_id = $1 AND record = $2",
        )
        .bind(self.repo_id as i64)
        .bind(record as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|(len,)| len as usize))
    }

    async fn add_shortcut_node(&mut self, shard: usize, elem: &[u8], node_id: usize) -> Result<()> {
        sqlx::query(
            "INSERT INTO rt_shortcuts (repo_id, shard, elem, node_id) VALUES ($1, $2, $3, $4) \
             ON CONFLICT DO NOTHING",
        )
        .bind(self.repo_id as i64)
        .bind(shard as i32)
        .bind(elem)
        .bind(node_id as i64)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn get_shortcut_nodes(&self, shard: usize, elem: &[u8]) -> Result<Vec<usize>> {
        let rows = sqlx::query_as::<_, (i64,)>(
            "SELECT node_id FROM rt_shortcuts WHERE repo_id = $1 AND shard = $2 AND elem = $3",
        )
        .bind(self.repo_id as i64)
        .bind(shard as i32)
        .bind(elem)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(rows.into_iter().map(|(id,)| id as usize).collect())
    }

    async fn clear_shortcuts(&mut self) -> Result<()> {
        sqlx::query("DELETE FROM rt_shortcuts WHERE repo_id = $1")
            .bind(self.repo_id as i64)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn set_edge_data(&mut self, edge: usize, data: &[u8]) -> Result<()> {
        sqlx::query(
            "INSERT INTO rt_edges (repo_id, id, data) VALUES ($1, $2, $3) \
             ON CONFLICT (repo_id, id) DO UPDATE SET data = EXCLUDED.data",
        )
        .bind(self.repo_id as i64)
        .bind(edge as i64)
        .bind(data)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn get_edge_data(&self, edge: usize) -> Result<Option<Vec<u8>>> {
        let row = sqlx::query_as::<_, (Vec<u8>,)>(
            "SELECT data FROM rt_edges WHERE repo_id = $1 AND id = $2",
        )
        .bind(self.repo_id as i64)
        .bind(edge as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|(d,)| d))
    }

    async fn clear_edges(&mut self) -> Result<()> {
        sqlx::query("DELETE FROM rt_edges WHERE repo_id = $1")
            .bind(self.repo_id as i64)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn for_each_edge_data(
        &self,
        f: &mut (dyn for<'a> FnMut(usize, &'a [u8]) -> Result<()> + Send),
    ) -> Result<()> {
        let rows = sqlx::query("SELECT id, data FROM rt_edges WHERE repo_id = $1")
            .bind(self.repo_id as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(db_err)?;
        for r in &rows {
            let id: i64 = r.try_get("id").map_err(db_err)?;
            let data: Vec<u8> = r.try_get("data").map_err(db_err)?;
            f(id as usize, &data)?;
        }
        Ok(())
    }

    async fn set_node_meta(&mut self, elem: usize, meta: &[u8]) -> Result<()> {
        sqlx::query(
            "INSERT INTO rt_node_meta (repo_id, elem, meta) VALUES ($1, $2, $3) \
             ON CONFLICT (repo_id, elem) DO UPDATE SET meta = EXCLUDED.meta",
        )
        .bind(self.repo_id as i64)
        .bind(elem as i64)
        .bind(meta)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn get_node_meta(&self, elem: usize) -> Result<Option<Vec<u8>>> {
        let row = sqlx::query_as::<_, (Vec<u8>,)>(
            "SELECT meta FROM rt_node_meta WHERE repo_id = $1 AND elem = $2",
        )
        .bind(self.repo_id as i64)
        .bind(elem as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|(m,)| m))
    }

    async fn clear_node_meta(&mut self) -> Result<()> {
        sqlx::query("DELETE FROM rt_node_meta WHERE repo_id = $1")
            .bind(self.repo_id as i64)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn set_chain(&mut self, record: usize, chain: &[u64]) -> Result<()> {
        let bytes = encode_chain(chain);
        sqlx::query(
            "INSERT INTO rt_chains (repo_id, record, chain) VALUES ($1, $2, $3) \
             ON CONFLICT (repo_id, record) DO UPDATE SET chain = EXCLUDED.chain",
        )
        .bind(self.repo_id as i64)
        .bind(record as i64)
        .bind(bytes)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn get_chain(&self, record: usize) -> Result<Option<Vec<u64>>> {
        let row = sqlx::query_as::<_, (Vec<u8>,)>(
            "SELECT chain FROM rt_chains WHERE repo_id = $1 AND record = $2",
        )
        .bind(self.repo_id as i64)
        .bind(record as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|(b,)| decode_chain(&b)))
    }

    async fn clear_chains(&mut self) -> Result<()> {
        sqlx::query("DELETE FROM rt_chains WHERE repo_id = $1")
            .bind(self.repo_id as i64)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn save_symbol(&mut self, sym: &Symbol) -> Result<()> {
        let annotations = serde_json::to_string(&sym.annotations).map_err(ser_err)?;
        sqlx::query(
            "INSERT INTO sg_symbols \
             (repo_id, id, name, kind, scope, scope_id, type_ref, type_name, file, \
              line, end_line, signature, doc, annotations, language) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15) \
             ON CONFLICT (repo_id, id) DO UPDATE SET \
                name = EXCLUDED.name, kind = EXCLUDED.kind, scope = EXCLUDED.scope, \
                scope_id = EXCLUDED.scope_id, type_ref = EXCLUDED.type_ref, \
                type_name = EXCLUDED.type_name, file = EXCLUDED.file, line = EXCLUDED.line, \
                end_line = EXCLUDED.end_line, signature = EXCLUDED.signature, doc = EXCLUDED.doc, \
                annotations = EXCLUDED.annotations, language = EXCLUDED.language",
        )
        .bind(self.repo_id as i64)
        .bind(sym.id as i64)
        .bind(&sym.name)
        .bind(sym.kind.as_str())
        .bind(sym.scope.as_str())
        .bind(sym.scope_id as i64)
        .bind(sym.type_ref as i64)
        .bind(&sym.type_name)
        .bind(&sym.file)
        .bind(sym.line as i32)
        .bind(sym.end_line as i32)
        .bind(&sym.signature)
        .bind(&sym.doc)
        .bind(annotations)
        .bind(&sym.language)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn load_symbol(&self, id: u64) -> Result<Option<Symbol>> {
        let row = sqlx::query(
            "SELECT id, name, kind, scope, scope_id, type_ref, type_name, file, line, \
             end_line, signature, doc, annotations, language \
             FROM sg_symbols WHERE repo_id = $1 AND id = $2",
        )
        .bind(self.repo_id as i64)
        .bind(id as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.as_ref().map(row_to_symbol).transpose()?)
    }

    async fn load_all_symbols(&self) -> Result<Vec<Symbol>> {
        let rows = sqlx::query(
            "SELECT id, name, kind, scope, scope_id, type_ref, type_name, file, line, \
             end_line, signature, doc, annotations, language FROM sg_symbols WHERE repo_id = $1",
        )
        .bind(self.repo_id as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        rows.iter().map(row_to_symbol).collect()
    }

    async fn save_embedding(&mut self, symbol_id: u64, vector: &[f32]) -> Result<()> {
        sqlx::query(
            "INSERT INTO sg_embeddings (repo_id, symbol_id, vector) VALUES ($1,$2,$3) \
             ON CONFLICT (repo_id, symbol_id) DO UPDATE SET vector = EXCLUDED.vector",
        )
        .bind(self.repo_id as i64)
        .bind(symbol_id as i64)
        .bind(encode_vector(vector))
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn load_embedding(&self, symbol_id: u64) -> Result<Option<Vec<f32>>> {
        let row: Option<(Vec<u8>,)> = sqlx::query_as(
            "SELECT vector FROM sg_embeddings WHERE repo_id = $1 AND symbol_id = $2",
        )
        .bind(self.repo_id as i64)
        .bind(symbol_id as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.and_then(|(b,)| decode_vector(&b)))
    }

    async fn load_all_embeddings(&self) -> Result<HashMap<u64, Vec<f32>>> {
        let rows: Vec<(i64, Vec<u8>)> =
            sqlx::query_as("SELECT symbol_id, vector FROM sg_embeddings WHERE repo_id = $1")
                .bind(self.repo_id as i64)
                .fetch_all(&self.pool)
                .await
                .map_err(db_err)?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, b)| decode_vector(&b).map(|v| (id as u64, v)))
            .collect())
    }

    async fn clear_embeddings(&mut self) -> Result<()> {
        sqlx::query("DELETE FROM sg_embeddings WHERE repo_id = $1")
            .bind(self.repo_id as i64)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn save_next_id(&mut self, next: u64) -> Result<()> {
        sqlx::query(
            "INSERT INTO sg_next_id (repo_id, next) VALUES ($1, $2) \
             ON CONFLICT (repo_id) DO UPDATE SET next = EXCLUDED.next",
        )
        .bind(self.repo_id as i64)
        .bind(next as i64)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn load_next_id(&self) -> Result<u64> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT next FROM sg_next_id WHERE repo_id = $1")
            .bind(self.repo_id as i64)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(row.map(|(v,)| v as u64).unwrap_or(0))
    }

    async fn all_chains(&self) -> Result<Vec<(u64, Vec<u8>)>> {
        let rows = sqlx::query("SELECT record, chain FROM rt_chains WHERE repo_id = $1")
            .bind(self.repo_id as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(db_err)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let record: i64 = r.try_get("record").map_err(db_err)?;
            let chain: Vec<u8> = r.try_get("chain").map_err(db_err)?;
            out.push((record as u64, chain));
        }
        Ok(out)
    }

    async fn set_call_records(&mut self, func: u64, records: &[u8]) -> Result<()> {
        sqlx::query(
            "INSERT INTO sg_call_records (repo_id, func, records) VALUES ($1, $2, $3) \
             ON CONFLICT (repo_id, func) DO UPDATE SET records = EXCLUDED.records",
        )
        .bind(self.repo_id as i64)
        .bind(func as i64)
        .bind(records)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn get_call_records(&self, func: u64) -> Result<Option<Vec<u8>>> {
        let row = sqlx::query_as::<_, (Vec<u8>,)>(
            "SELECT records FROM sg_call_records WHERE repo_id = $1 AND func = $2",
        )
        .bind(self.repo_id as i64)
        .bind(func as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|(b,)| b))
    }

    async fn all_call_records(&self) -> Result<Vec<(u64, Vec<u8>)>> {
        let rows = sqlx::query("SELECT func, records FROM sg_call_records WHERE repo_id = $1")
            .bind(self.repo_id as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(db_err)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let func: i64 = r.try_get("func").map_err(db_err)?;
            let records: Vec<u8> = r.try_get("records").map_err(db_err)?;
            out.push((func as u64, records));
        }
        Ok(out)
    }

    async fn set_call_name_index(&mut self, name: &str, sites: &[u8]) -> Result<()> {
        sqlx::query(
            "INSERT INTO sg_call_names (repo_id, name, sites) VALUES ($1, $2, $3) \
             ON CONFLICT (repo_id, name) DO UPDATE SET sites = EXCLUDED.sites",
        )
        .bind(self.repo_id as i64)
        .bind(name)
        .bind(sites)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn load_call_name_index(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let row = sqlx::query_as::<_, (Vec<u8>,)>(
            "SELECT sites FROM sg_call_names WHERE repo_id = $1 AND name = $2",
        )
        .bind(self.repo_id as i64)
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|(b,)| b))
    }

    async fn all_call_name_indexes(&self) -> Result<Vec<(String, Vec<u8>)>> {
        let rows = sqlx::query("SELECT name, sites FROM sg_call_names WHERE repo_id = $1")
            .bind(self.repo_id as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(db_err)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let name: String = r.try_get("name").map_err(db_err)?;
            let sites: Vec<u8> = r.try_get("sites").map_err(db_err)?;
            out.push((name, sites));
        }
        Ok(out)
    }

    async fn upsert_file(&mut self, f: &FileInfo) -> Result<()> {
        sqlx::query(
            "INSERT INTO sg_files (repo_id, path, language, bytes, lines) VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (repo_id, path) DO UPDATE SET \
                language = EXCLUDED.language, bytes = EXCLUDED.bytes, lines = EXCLUDED.lines",
        )
        .bind(self.repo_id as i64)
        .bind(&f.path)
        .bind(&f.language)
        .bind(f.bytes as i64)
        .bind(f.lines as i32)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn load_all_files(&self) -> Result<Vec<FileInfo>> {
        let rows =
            sqlx::query("SELECT path, language, bytes, lines FROM sg_files WHERE repo_id = $1")
                .bind(self.repo_id as i64)
                .fetch_all(&self.pool)
                .await
                .map_err(db_err)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            out.push(FileInfo {
                path: r.try_get("path").map_err(db_err)?,
                language: r.try_get("language").map_err(db_err)?,
                bytes: r.try_get::<i64, _>("bytes").map_err(db_err)? as u64,
                lines: r.try_get::<i32, _>("lines").map_err(db_err)? as u32,
            });
        }
        Ok(out)
    }

    async fn version(&self) -> Result<u64> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT version FROM sg_meta WHERE repo_id = $1")
            .bind(self.repo_id as i64)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(row.map(|(v,)| v as u64).unwrap_or(0))
    }

    async fn set_version(&mut self, v: u64) -> Result<()> {
        sqlx::query(
            "INSERT INTO sg_meta (repo_id, version) VALUES ($1, $2) \
             ON CONFLICT (repo_id) DO UPDATE SET version = EXCLUDED.version",
        )
        .bind(self.repo_id as i64)
        .bind(v as i64)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn set_stats(&mut self, s: IndexCounts) -> Result<()> {
        sqlx::query(
            "INSERT INTO sg_stats (repo_id, symbols, chains, edges, files, next_id) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (repo_id) DO UPDATE SET \
             symbols = EXCLUDED.symbols, chains = EXCLUDED.chains, \
             edges = EXCLUDED.edges, files = EXCLUDED.files, next_id = EXCLUDED.next_id",
        )
        .bind(self.repo_id as i64)
        .bind(s.symbols as i64)
        .bind(s.chains as i64)
        .bind(s.edges as i64)
        .bind(s.files as i64)
        .bind(s.next_id as i64)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn stats(&self) -> Result<IndexCounts> {
        let row: Option<(i64, i64, i64, i64, i64)> =
            sqlx::query_as("SELECT symbols, chains, edges, files, next_id FROM sg_stats WHERE repo_id = $1")
                .bind(self.repo_id as i64)
                .fetch_optional(&self.pool)
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
            None => Ok(IndexCounts::default()),
        }
    }

    async fn clear_entities(&mut self) -> Result<()> {
        let rid = self.repo_id as i64;
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        for t in [
            "sg_symbols",
            "sg_embeddings",
            "sg_files",
            "sg_call_records",
            "sg_call_names",
            "rt_nodes",
            "rt_children",
            "rt_roots",
            "rt_meta",
            "rt_keylen",
            "rt_shortcuts",
            "rt_chains",
            "rt_edges",
            "rt_node_meta",
            "rt_node_blooms",
        ] {
            sqlx::query(&format!("DELETE FROM {t} WHERE repo_id = $1"))
                .bind(rid)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
        }
        sqlx::query("UPDATE rt_counter SET next = 1 WHERE repo_id = $1")
            .bind(rid)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query("UPDATE sg_next_id SET next = 100 WHERE repo_id = $1")
            .bind(rid)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query("UPDATE sg_meta SET version = 0 WHERE repo_id = $1")
            .bind(rid)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query(
            "INSERT INTO rt_nodes (repo_id, id, prefix, record) VALUES ($1, 0, '', 0) \
             ON CONFLICT (repo_id, id) DO NOTHING",
        )
        .bind(rid)
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(())
    }

    #[cfg(feature = "bloom-search")]
    async fn set_node_bloom(&mut self, id: usize, bloom: &[u8]) -> Result<()> {
        sqlx::query(
            "INSERT INTO rt_node_blooms (repo_id, id, bloom) VALUES ($1, $2, $3) \
             ON CONFLICT (repo_id, id) DO UPDATE SET bloom = EXCLUDED.bloom",
        )
        .bind(self.repo_id as i64)
        .bind(id as i64)
        .bind(bloom)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    #[cfg(feature = "bloom-search")]
    async fn get_node_bloom(&self, id: usize) -> Result<Option<Vec<u8>>> {
        let row = sqlx::query_as::<_, (Vec<u8>,)>(
            "SELECT bloom FROM rt_node_blooms WHERE repo_id = $1 AND id = $2",
        )
        .bind(self.repo_id as i64)
        .bind(id as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|(b,)| b))
    }

    fn new_tx(&self) -> Box<dyn Tx> {
        Box::new(PostgresTx {
            pool: self.pool.clone(),
            repo_id: self.repo_id,
            nodes: Vec::new(),
            ops: Vec::new(),
        })
    }
}

/// Probe version index trên đĩa (dùng cho `SharedGraphIndex::ensure_fresh`) —
/// không mở toàn bộ index. `None`/lỗi → coi như version 0.
#[cfg(feature = "postgres")]
impl PostgresStorage {
    pub async fn probe_version(dsn: &str, repo_id: u64) -> Result<u64> {
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(dsn)
            .await
            .map_err(db_err)?;
        let row: Option<(i64,)> = sqlx::query_as("SELECT version FROM sg_meta WHERE repo_id = $1")
            .bind(repo_id as i64)
            .fetch_optional(&pool)
            .await
            .map_err(db_err)?;
        Ok(row.map(|(v,)| v as u64).unwrap_or(0))
    }
}

// ==================== PostgresTx ====================

/// Transaction cho `PostgresStorage`: buffer mutation, áp dụng atomic trong 1
/// Postgres transaction tại `commit`. `new_node` cấp id nguyên tử per-repo ngay
/// lúc reservation (tránh trùng id khi nhiều writer).
pub struct PostgresTx {
    pool: PgPool,
    repo_id: u64,
    nodes: Vec<(usize, Vec<u8>, usize)>,
    ops: Vec<super::TxOp>,
}

#[async_trait]
impl Tx for PostgresTx {
    async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize> {
        let row: (i64,) = sqlx::query_as(
            "UPDATE rt_counter SET next = next + 1 WHERE repo_id = $1 RETURNING next - 1",
        )
        .bind(self.repo_id as i64)
        .fetch_one(&self.pool)
        .await
        .map_err(db_err)?;
        let id = row.0 as usize;
        self.nodes.push((id, prefix, record));
        Ok(id)
    }

    async fn update_node(
        &mut self,
        id: usize,
        prefix: Option<Vec<u8>>,
        record: Option<usize>,
    ) -> Result<()> {
        self.ops
            .push(super::TxOp::UpdateNode { id, prefix, record });
        Ok(())
    }

    async fn add_child(&mut self, parent: usize, child: usize) -> Result<()> {
        self.ops.push(super::TxOp::AddChild { parent, child });
        Ok(())
    }

    async fn move_child(&mut self, from: usize, to: usize, child: usize) -> Result<()> {
        self.ops.push(super::TxOp::MoveChild { from, to, child });
        Ok(())
    }

    async fn commit(self: Box<Self>) -> Result<()> {
        let PostgresTx {
            pool,
            repo_id,
            nodes,
            ops,
        } = *self;
        let rid = repo_id as i64;
        let mut tx = pool.begin().await.map_err(db_err)?;

        for (id, prefix, record) in &nodes {
            sqlx::query(
                "INSERT INTO rt_nodes (repo_id, id, prefix, record) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (repo_id, id) DO NOTHING",
            )
            .bind(rid)
            .bind(*id as i64)
            .bind(prefix)
            .bind(*record as i64)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }

        if let Some(max_id) = nodes.iter().map(|(id, _, _)| *id).max() {
            sqlx::query("UPDATE rt_counter SET next = GREATEST(next, $1) WHERE repo_id = $2")
                .bind((max_id + 1) as i64)
                .bind(rid)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
        }

        for op in ops {
            match op {
                super::TxOp::AddChild { parent, child } => {
                    sqlx::query(
                        "INSERT INTO rt_children (repo_id, parent, child) VALUES ($1, $2, $3) \
                         ON CONFLICT DO NOTHING",
                    )
                    .bind(rid)
                    .bind(parent as i64)
                    .bind(child as i64)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
                }
                super::TxOp::MoveChild { from, to, child } => {
                    sqlx::query(
                        "DELETE FROM rt_children WHERE repo_id = $1 AND parent = $2 AND child = $3",
                    )
                    .bind(rid)
                    .bind(from as i64)
                    .bind(child as i64)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
                    sqlx::query(
                        "INSERT INTO rt_children (repo_id, parent, child) VALUES ($1, $2, $3) \
                         ON CONFLICT DO NOTHING",
                    )
                    .bind(rid)
                    .bind(to as i64)
                    .bind(child as i64)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
                }
                super::TxOp::UpdateNode { id, prefix, record } => {
                    if let Some(p) = prefix {
                        sqlx::query(
                            "UPDATE rt_nodes SET prefix = $1 WHERE repo_id = $2 AND id = $3",
                        )
                        .bind(p)
                        .bind(rid)
                        .bind(id as i64)
                        .execute(&mut *tx)
                        .await
                        .map_err(db_err)?;
                    }
                    if let Some(r) = record {
                        sqlx::query(
                            "UPDATE rt_nodes SET record = $1 WHERE repo_id = $2 AND id = $3",
                        )
                        .bind(r as i64)
                        .bind(rid)
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

/// Map một row `sg_symbols` → `codegraph_core::Symbol`.
fn row_to_symbol(row: &PgRow) -> Result<Symbol> {
    let id: i64 = row.try_get("id").map_err(db_err)?;
    let name: String = row.try_get("name").map_err(db_err)?;
    let kind: String = row.try_get("kind").map_err(db_err)?;
    let scope: String = row.try_get("scope").map_err(db_err)?;
    let scope_id: i64 = row.try_get("scope_id").map_err(db_err)?;
    let type_ref: i64 = row.try_get("type_ref").map_err(db_err)?;
    let type_name: Option<String> = row.try_get("type_name").map_err(db_err)?;
    let file: String = row.try_get("file").map_err(db_err)?;
    let line: i32 = row.try_get("line").map_err(db_err)?;
    let end_line: i32 = row.try_get("end_line").map_err(db_err)?;
    let signature: Option<String> = row.try_get("signature").map_err(db_err)?;
    let doc: Option<String> = row.try_get("doc").map_err(db_err)?;
    let annotations: String = row.try_get("annotations").map_err(db_err)?;
    let language: String = row.try_get("language").map_err(db_err)?;
    let kind = SymbolKind::parse(&kind)
        .ok_or_else(|| StorageError::Internal(format!("bad symbol kind: {kind}")))?;
    let scope = ScopeLevel::parse(&scope)
        .ok_or_else(|| StorageError::Internal(format!("bad scope level: {scope}")))?;
    let annotations: Vec<Annotation> = serde_json::from_str(&annotations).map_err(ser_err)?;
    Ok(Symbol {
        id: id as u64,
        name,
        kind,
        scope,
        scope_id: scope_id as u64,
        type_ref: type_ref as u64,
        type_name,
        file,
        line: line as u32,
        end_line: end_line as u32,
        signature,
        doc,
        annotations,
        language,
    })
}
