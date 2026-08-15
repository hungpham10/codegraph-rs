use super::{Result, Storage, StorageError, Tx, decode_chain, encode_chain};
use async_trait::async_trait;
use codegraph_core::{Annotation, FileInfo, ScopeLevel, Symbol, SymbolKind};
use sqlx::mysql::{MySqlPoolOptions, MySqlRow};
use sqlx::{MySqlPool, Row};

/// MySQL implementation của `Storage` trait — multi-tenant (mọi bảng dẫn đầu bằng
/// `repo_id`), theo thiết kế `sql/README.md`. Instance được bind vào một `repo_id`.
/// Schema apply **thủ công** (user chạy `sql/mysql/001`+`002`); code chỉ seed
/// runtime row per-repo.
pub struct MySqlStorage {
    pool: MySqlPool,
    repo_id: u64,
}

fn db_err(e: sqlx::Error) -> StorageError {
    StorageError::Internal(e.to_string())
}

fn ser_err(e: impl std::fmt::Display) -> StorageError {
    StorageError::Internal(e.to_string())
}

impl MySqlStorage {
    /// Mở pool + seed per-repo runtime rows. `repo_id` do config/sharding quyết
    /// định (không lấy từ DSN).
    pub async fn open(dsn: &str, repo_id: u64) -> Result<Self> {
        let pool = MySqlPoolOptions::new()
            .max_connections(5)
            .connect(dsn)
            .await
            .map_err(db_err)?;
        let s = Self { pool, repo_id };
        s.ensure_repo_seeded().await?;
        Ok(s)
    }

    async fn ensure_repo_seeded(&self) -> Result<()> {
        let rid = self.repo_id as i64;
        sqlx::query(
            "INSERT IGNORE INTO rt_nodes (repo_id, id, prefix, record) VALUES (?, 0, '', 0)",
        )
        .bind(rid)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        sqlx::query("INSERT IGNORE INTO rt_counter (repo_id, next) VALUES (?, 1)")
            .bind(rid)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        sqlx::query("INSERT IGNORE INTO sg_next_id (repo_id, next) VALUES (?, 100)")
            .bind(rid)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        sqlx::query("INSERT IGNORE INTO sg_meta (repo_id, version) VALUES (?, 0)")
            .bind(rid)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    /// Ghi/ensure row `repos` (registry toàn cục, shard của repo) — idempotent.
    /// Bảng này nằm trong migration `002` (áp dụng thủ công).
    pub async fn ensure_registered(&self, shard: usize, root: Option<&str>) -> Result<()> {
        sqlx::query("INSERT IGNORE INTO repos (repo_id, shard, root) VALUES (?, ?, ?)")
            .bind(self.repo_id as i64)
            .bind(shard as i32)
            .bind(root)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    /// Cấp node id nguyên tử, per-repo — dùng idiom `LAST_INSERT_ID(next) + 1`
    /// (theo README) trong 1 transaction để tránh race.
    async fn reserve_node_id(&self) -> Result<usize> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        sqlx::query("UPDATE rt_counter SET next = LAST_INSERT_ID(next) + 1 WHERE repo_id = ?")
            .bind(self.repo_id as i64)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        let row: (u64,) = sqlx::query_as("SELECT LAST_INSERT_ID()")
            .fetch_one(&mut *tx)
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
        Ok(row.0 as usize)
    }
}

#[async_trait]
impl Storage for MySqlStorage {
    async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize> {
        let id = self.reserve_node_id().await?;
        sqlx::query(
            "INSERT INTO rt_nodes (repo_id, id, prefix, record) VALUES (?, ?, ?, ?) \
             ON DUPLICATE KEY UPDATE id = id",
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
            sqlx::query("UPDATE rt_nodes SET prefix = ? WHERE repo_id = ? AND id = ?")
                .bind(p)
                .bind(rid)
                .bind(id as i64)
                .execute(&self.pool)
                .await
                .map_err(db_err)?;
        }
        if let Some(r) = record {
            sqlx::query("UPDATE rt_nodes SET record = ? WHERE repo_id = ? AND id = ?")
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
            "SELECT prefix, record FROM rt_nodes WHERE repo_id = ? AND id = ?",
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
            "SELECT child FROM rt_children WHERE repo_id = ? AND parent = ? ORDER BY child",
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
            "INSERT INTO rt_roots (repo_id, shard, root) VALUES (?, ?, ?) \
             ON DUPLICATE KEY UPDATE root = VALUES(root)",
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
            "SELECT root FROM rt_roots WHERE repo_id = ? AND shard = ?",
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
            "INSERT INTO rt_meta (repo_id, record, meta) VALUES (?, ?, ?) \
             ON DUPLICATE KEY UPDATE meta = VALUES(meta)",
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
            "SELECT meta FROM rt_meta WHERE repo_id = ? AND record = ?",
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
            "INSERT INTO rt_keylen (repo_id, record, len) VALUES (?, ?, ?) \
             ON DUPLICATE KEY UPDATE len = VALUES(len)",
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
            "SELECT len FROM rt_keylen WHERE repo_id = ? AND record = ?",
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
            "INSERT IGNORE INTO rt_shortcuts (repo_id, shard, elem, node_id) VALUES (?, ?, ?, ?)",
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
            "SELECT node_id FROM rt_shortcuts WHERE repo_id = ? AND shard = ? AND elem = ?",
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
        sqlx::query("DELETE FROM rt_shortcuts WHERE repo_id = ?")
            .bind(self.repo_id as i64)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn set_edge_data(&mut self, edge: usize, data: &[u8]) -> Result<()> {
        sqlx::query(
            "INSERT INTO rt_edges (repo_id, id, data) VALUES (?, ?, ?) \
             ON DUPLICATE KEY UPDATE data = VALUES(data)",
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
            "SELECT data FROM rt_edges WHERE repo_id = ? AND id = ?",
        )
        .bind(self.repo_id as i64)
        .bind(edge as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|(d,)| d))
    }

    async fn clear_edges(&mut self) -> Result<()> {
        sqlx::query("DELETE FROM rt_edges WHERE repo_id = ?")
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
        let rows = sqlx::query("SELECT id, data FROM rt_edges WHERE repo_id = ?")
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
            "INSERT INTO rt_node_meta (repo_id, elem, meta) VALUES (?, ?, ?) \
             ON DUPLICATE KEY UPDATE meta = VALUES(meta)",
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
            "SELECT meta FROM rt_node_meta WHERE repo_id = ? AND elem = ?",
        )
        .bind(self.repo_id as i64)
        .bind(elem as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|(m,)| m))
    }

    async fn clear_node_meta(&mut self) -> Result<()> {
        sqlx::query("DELETE FROM rt_node_meta WHERE repo_id = ?")
            .bind(self.repo_id as i64)
            .execute(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn set_chain(&mut self, record: usize, chain: &[u64]) -> Result<()> {
        let bytes = encode_chain(chain);
        sqlx::query(
            "INSERT INTO rt_chains (repo_id, record, chain) VALUES (?, ?, ?) \
             ON DUPLICATE KEY UPDATE chain = VALUES(chain)",
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
            "SELECT chain FROM rt_chains WHERE repo_id = ? AND record = ?",
        )
        .bind(self.repo_id as i64)
        .bind(record as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|(b,)| decode_chain(&b)))
    }

    async fn clear_chains(&mut self) -> Result<()> {
        sqlx::query("DELETE FROM rt_chains WHERE repo_id = ?")
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
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON DUPLICATE KEY UPDATE \
                name = VALUES(name), kind = VALUES(kind), scope = VALUES(scope), \
                scope_id = VALUES(scope_id), type_ref = VALUES(type_ref), \
                type_name = VALUES(type_name), file = VALUES(file), line = VALUES(line), \
                end_line = VALUES(end_line), signature = VALUES(signature), doc = VALUES(doc), \
                annotations = VALUES(annotations), language = VALUES(language)",
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
             FROM sg_symbols WHERE repo_id = ? AND id = ?",
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
             end_line, signature, doc, annotations, language FROM sg_symbols WHERE repo_id = ?",
        )
        .bind(self.repo_id as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db_err)?;
        rows.iter().map(row_to_symbol).collect()
    }

    async fn save_next_id(&mut self, next: u64) -> Result<()> {
        sqlx::query(
            "INSERT INTO sg_next_id (repo_id, next) VALUES (?, ?) \
             ON DUPLICATE KEY UPDATE next = VALUES(next)",
        )
        .bind(self.repo_id as i64)
        .bind(next as i64)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn load_next_id(&self) -> Result<u64> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT next FROM sg_next_id WHERE repo_id = ?")
            .bind(self.repo_id as i64)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(row.map(|(v,)| v as u64).unwrap_or(0))
    }

    async fn all_chains(&self) -> Result<Vec<(u64, Vec<u8>)>> {
        let rows = sqlx::query("SELECT record, chain FROM rt_chains WHERE repo_id = ?")
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
            "INSERT INTO sg_call_records (repo_id, func, records) VALUES (?, ?, ?) \
             ON DUPLICATE KEY UPDATE records = VALUES(records)",
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
            "SELECT records FROM sg_call_records WHERE repo_id = ? AND func = ?",
        )
        .bind(self.repo_id as i64)
        .bind(func as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|(b,)| b))
    }

    async fn all_call_records(&self) -> Result<Vec<(u64, Vec<u8>)>> {
        let rows = sqlx::query("SELECT func, records FROM sg_call_records WHERE repo_id = ?")
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
            "INSERT INTO sg_call_names (repo_id, name, sites) VALUES (?, ?, ?) \
             ON DUPLICATE KEY UPDATE sites = VALUES(sites)",
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
            "SELECT sites FROM sg_call_names WHERE repo_id = ? AND name = ?",
        )
        .bind(self.repo_id as i64)
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|(b,)| b))
    }

    async fn all_call_name_indexes(&self) -> Result<Vec<(String, Vec<u8>)>> {
        let rows = sqlx::query("SELECT name, sites FROM sg_call_names WHERE repo_id = ?")
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
            "INSERT INTO sg_files (repo_id, path, language, bytes, `lines`) VALUES (?, ?, ?, ?, ?) \
             ON DUPLICATE KEY UPDATE \
                language = VALUES(language), bytes = VALUES(bytes), `lines` = VALUES(`lines`)",
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
            sqlx::query("SELECT path, language, bytes, `lines` FROM sg_files WHERE repo_id = ?")
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
        let row: Option<(i64,)> = sqlx::query_as("SELECT version FROM sg_meta WHERE repo_id = ?")
            .bind(self.repo_id as i64)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_err)?;
        Ok(row.map(|(v,)| v as u64).unwrap_or(0))
    }

    async fn set_version(&mut self, v: u64) -> Result<()> {
        sqlx::query(
            "INSERT INTO sg_meta (repo_id, version) VALUES (?, ?) \
             ON DUPLICATE KEY UPDATE version = VALUES(version)",
        )
        .bind(self.repo_id as i64)
        .bind(v as i64)
        .execute(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn clear_entities(&mut self) -> Result<()> {
        let rid = self.repo_id as i64;
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        for t in [
            "sg_symbols",
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
            sqlx::query(&format!("DELETE FROM {t} WHERE repo_id = ?"))
                .bind(rid)
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
        }
        sqlx::query("UPDATE rt_counter SET next = 1 WHERE repo_id = ?")
            .bind(rid)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query("UPDATE sg_next_id SET next = 100 WHERE repo_id = ?")
            .bind(rid)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query("UPDATE sg_meta SET version = 0 WHERE repo_id = ?")
            .bind(rid)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query(
            "INSERT IGNORE INTO rt_nodes (repo_id, id, prefix, record) VALUES (?, 0, '', 0)",
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
            "INSERT INTO rt_node_blooms (repo_id, id, bloom) VALUES (?, ?, ?) \
             ON DUPLICATE KEY UPDATE bloom = VALUES(bloom)",
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
            "SELECT bloom FROM rt_node_blooms WHERE repo_id = ? AND id = ?",
        )
        .bind(self.repo_id as i64)
        .bind(id as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_err)?;
        Ok(row.map(|(b,)| b))
    }

    fn new_tx(&self) -> Box<dyn Tx> {
        Box::new(MySqlTx {
            pool: self.pool.clone(),
            repo_id: self.repo_id,
            nodes: Vec::new(),
            ops: Vec::new(),
        })
    }
}

/// Probe version index trên đĩa (dùng cho `SharedGraphIndex::ensure_fresh`).
#[cfg(feature = "mysql")]
impl MySqlStorage {
    pub async fn probe_version(dsn: &str, repo_id: u64) -> Result<u64> {
        let pool = MySqlPoolOptions::new()
            .max_connections(2)
            .connect(dsn)
            .await
            .map_err(db_err)?;
        let row: Option<(i64,)> = sqlx::query_as("SELECT version FROM sg_meta WHERE repo_id = ?")
            .bind(repo_id as i64)
            .fetch_optional(&pool)
            .await
            .map_err(db_err)?;
        Ok(row.map(|(v,)| v as u64).unwrap_or(0))
    }
}

// ==================== MySqlTx ====================

/// Transaction cho `MySqlStorage`: buffer mutation, áp dụng atomic trong 1 MySQL
/// transaction tại `commit`. `new_node` cấp id nguyên tử per-repo (LAST_INSERT_ID).
pub struct MySqlTx {
    pool: MySqlPool,
    repo_id: u64,
    nodes: Vec<(usize, Vec<u8>, usize)>,
    ops: Vec<super::TxOp>,
}

#[async_trait]
impl Tx for MySqlTx {
    async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize> {
        let mut tx = self.pool.begin().await.map_err(db_err)?;
        sqlx::query("UPDATE rt_counter SET next = LAST_INSERT_ID(next) + 1 WHERE repo_id = ?")
            .bind(self.repo_id as i64)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        let row: (u64,) = sqlx::query_as("SELECT LAST_INSERT_ID()")
            .fetch_one(&mut *tx)
            .await
            .map_err(db_err)?;
        tx.commit().await.map_err(db_err)?;
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
        let MySqlTx {
            pool,
            repo_id,
            nodes,
            ops,
        } = *self;
        let rid = repo_id as i64;
        let mut tx = pool.begin().await.map_err(db_err)?;

        for (id, prefix, record) in &nodes {
            sqlx::query(
                "INSERT INTO rt_nodes (repo_id, id, prefix, record) VALUES (?, ?, ?, ?) \
                 ON DUPLICATE KEY UPDATE id = id",
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
            sqlx::query("UPDATE rt_counter SET next = GREATEST(next, ?) WHERE repo_id = ?")
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
                        "INSERT IGNORE INTO rt_children (repo_id, parent, child) VALUES (?, ?, ?)",
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
                        "DELETE FROM rt_children WHERE repo_id = ? AND parent = ? AND child = ?",
                    )
                    .bind(rid)
                    .bind(from as i64)
                    .bind(child as i64)
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
                    sqlx::query(
                        "INSERT IGNORE INTO rt_children (repo_id, parent, child) VALUES (?, ?, ?)",
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
                        sqlx::query("UPDATE rt_nodes SET prefix = ? WHERE repo_id = ? AND id = ?")
                            .bind(p)
                            .bind(rid)
                            .bind(id as i64)
                            .execute(&mut *tx)
                            .await
                            .map_err(db_err)?;
                    }
                    if let Some(r) = record {
                        sqlx::query("UPDATE rt_nodes SET record = ? WHERE repo_id = ? AND id = ?")
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
fn row_to_symbol(row: &MySqlRow) -> Result<Symbol> {
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
