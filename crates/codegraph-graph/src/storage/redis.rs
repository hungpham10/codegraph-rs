//! Redis-backed radix-node storage.
//!
//! Cấu trúc key:
//! | Key                      | Kiểu  | Mục đích                  |
//! |--------------------------|-------|---------------------------|
//! | `{prefix}:branch`        | List  | prefix của từng node      |
//! | `{prefix}:record`        | List  | record của từng node      |
//! | `{prefix}:forward:{id}`  | Set   | children list của node    |
//! | `{prefix}:endpoint`      | Hash  | root ID cho mỗi shard     |
//! | `{prefix}:meta`          | Hash  | record_idx → metadata     |
//! | `{prefix}:keylen`        | Hash  | record_idx → key length   |
//! | `{prefix}:edgedata`      | Hash  | edge id → edge metadata   |
//! | `{prefix}:nodemeta`      | Hash  | element id → node metadata|
//! | `{prefix}:chains`        | Hash  | record → chain bytes      |
//! | `{prefix}:shortcut:{shard}:{elem}` | Set | node ids chứa elem |
//! | `{prefix}:symbols`       | Hash  | symbol id → Symbol JSON   |
//! | `{prefix}:embeddings`    | Hash  | symbol id → embedding BLOB (f32 little-endian) |
//! | `{prefix}:nextid`        | String| next symbol registry id  |
//! | `{prefix}:callrecords`   | Hash  | func id → call records    |
//! | `{prefix}:callnames`     | Hash  | call name → call sites    |
//! | `{prefix}:files`         | Hash  | path → FileInfo JSON      |
//! | `{prefix}:version`       | String| index version            |

use std::collections::HashMap;
use std::sync::Arc;

use redis::aio::MultiplexedConnection;
use tokio::sync::Mutex;

use async_trait::async_trait;

use super::{decode_vector, encode_vector, FileInfo, Result, Storage, StorageError, Symbol, Tx, TxOp};

// ==================== KeyBuilder ====================

type KeyFormatter = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// Cấu hình key cho Redis storage.
#[derive(Clone)]
pub struct KeyBuilder {
    prefix: String,
    formatter: Option<KeyFormatter>,
}

impl KeyBuilder {
    pub fn new(prefix: &str) -> Self {
        Self {
            prefix: prefix.to_string(),
            formatter: None,
        }
    }

    #[allow(dead_code)] // API tiện ích (caller tạo KeyBuilder tuỳ biến) — chưa dùng nội bộ.
    pub fn with_formatter(prefix: &str, f: KeyFormatter) -> Self {
        Self {
            prefix: prefix.to_string(),
            formatter: Some(f),
        }
    }

    /// `key("branch")` → `"{prefix}:branch"`
    pub fn key(&self, name: &str) -> String {
        match &self.formatter {
            Some(f) => f(name),
            None => format!("{}:{}", self.prefix, name),
        }
    }

    /// `indexed("forward", 5)` → `"{prefix}:forward:5"`
    pub fn indexed(&self, name: &str, idx: usize) -> String {
        self.key(&format!("{name}:{idx}"))
    }

    /// `shortcut(3, [0x01])` → `"{prefix}:shortcut:3:{0x01}"`
    /// (bytes của elem nối trực tiếp — Redis key binary-safe).
    pub fn shortcut(&self, shard: usize, elem: &[u8]) -> Vec<u8> {
        let mut k = self.key(&format!("shortcut:{shard}")).into_bytes();
        k.push(b':');
        k.extend_from_slice(elem);
        k
    }

    /// Prefix chung của mọi shortcut key: `"{prefix}:shortcut:"`.
    /// Dùng làm MATCH pattern khi SCAN để xoá toàn bộ shortcuts.
    pub fn shortcut_prefix(&self) -> String {
        self.key("shortcut") + ":"
    }
}

/// Helper shorthand: `cmd("LLEN")` → `redis::cmd("LLEN")`
fn cmd(name: &str) -> redis::Cmd {
    redis::cmd(name)
}

// ==================== RedisStorage ====================

pub struct RedisStorage {
    conn: Arc<Mutex<MultiplexedConnection>>,
    kb: KeyBuilder,
}

impl RedisStorage {
    async fn lock(&self) -> tokio::sync::MutexGuard<'_, MultiplexedConnection> {
        self.conn.lock().await
    }

    pub async fn new(client: redis::Client, prefix: &str) -> Result<Self> {
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        let s = Self {
            conn: Arc::new(Mutex::new(conn)),
            kb: KeyBuilder::new(prefix),
        };
        s.init().await?;
        Ok(s)
    }

    #[allow(dead_code)] // helper — chưa có caller nội bộ.
    pub async fn from_multiplexed(conn: MultiplexedConnection, prefix: &str) -> Result<Self> {
        let s = Self {
            conn: Arc::new(Mutex::new(conn)),
            kb: KeyBuilder::new(prefix),
        };
        s.init().await?;
        Ok(s)
    }

    #[allow(dead_code)] // helper — chưa có caller nội bộ.
    pub async fn with_key_builder(client: redis::Client, kb: KeyBuilder) -> Result<Self> {
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
        let s = Self {
            conn: Arc::new(Mutex::new(conn)),
            kb,
        };
        s.init().await?;
        Ok(s)
    }

    async fn init(&self) -> Result<()> {
        let mut conn = self.lock().await;
        let exists: bool = cmd("EXISTS")
            .arg(self.kb.key("branch"))
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        if !exists {
            redis::pipe()
                .atomic()
                .rpush(self.kb.key("branch"), b"" as &[u8])
                .rpush(self.kb.key("record"), 0i64)
                .exec_async(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        }
        Ok(())
    }

    /// Độ dài hiện tại của branch list = số node (gồm sentinel).
    /// Node id tiếp theo = len - 1.
    #[allow(dead_code)] // helper — chưa có caller nội bộ.
    async fn node_len(&self) -> Result<usize> {
        let mut conn = self.lock().await;
        let len: usize = cmd("LLEN")
            .arg(self.kb.key("branch"))
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(len)
    }
}

#[async_trait]
impl Storage for RedisStorage {
    async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize> {
        let mut conn = self.lock().await;
        let result: redis::Value = redis::pipe()
            .atomic()
            .rpush(self.kb.key("branch"), &prefix[..])
            .rpush(self.kb.key("record"), record as i64)
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;

        let len: usize = match result {
            redis::Value::Array(ref items) => match items.first() {
                Some(redis::Value::Int(n)) => *n as usize,
                _ => cmd("LLEN")
                    .arg(self.kb.key("branch"))
                    .query_async::<usize>(&mut *conn)
                    .await
                    .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?,
            },
            _ => cmd("LLEN")
                .arg(self.kb.key("branch"))
                .query_async::<usize>(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?,
        };

        Ok(len - 1)
    }

    async fn update_node(
        &mut self,
        id: usize,
        prefix: Option<Vec<u8>>,
        record: Option<usize>,
    ) -> Result<()> {
        let mut conn = self.lock().await;
        let mut pipe = redis::pipe();
        pipe.atomic();
        if let Some(p) = prefix {
            pipe.lset(self.kb.key("branch"), id as isize, &p[..]);
        }
        if let Some(r) = record {
            pipe.lset(self.kb.key("record"), id as isize, r as i64);
        }
        pipe.exec_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_node(&self, id: usize) -> Result<(Vec<u8>, usize)> {
        let mut conn = self.lock().await;
        let prefix: Vec<u8> = cmd("LINDEX")
            .arg(self.kb.key("branch"))
            .arg(id as isize)
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        let rec: i64 = cmd("LINDEX")
            .arg(self.kb.key("record"))
            .arg(id as isize)
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok((prefix, rec as usize))
    }

    async fn get_children(&self, id: usize) -> Result<Vec<usize>> {
        let mut conn = self.lock().await;
        let children: Vec<i64> = cmd("SMEMBERS")
            .arg(self.kb.indexed("forward", id))
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(children.into_iter().map(|x| x as usize).collect())
    }

    #[cfg(feature = "bloom-search")]
    async fn set_node_bloom(&mut self, id: usize, bloom: &[u8]) -> Result<()> {
        let mut conn = self.lock().await;
        cmd("HSET")
            .arg(self.kb.key("node_bloom"))
            .arg(id)
            .arg(bloom)
            .query_async::<()>(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    #[cfg(feature = "bloom-search")]
    async fn get_node_bloom(&self, id: usize) -> Result<Option<Vec<u8>>> {
        let mut conn = self.lock().await;
        let bloom: Option<Vec<u8>> = cmd("HGET")
            .arg(self.kb.key("node_bloom"))
            .arg(id)
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(bloom)
    }

    async fn set_root(&mut self, shard: usize, root: usize) -> Result<()> {
        let mut conn = self.lock().await;
        cmd("HSET")
            .arg(self.kb.key("endpoint"))
            .arg(shard as i64)
            .arg(root as i64)
            .query_async::<()>(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_root(&self, shard: usize) -> Result<usize> {
        let mut conn = self.lock().await;
        let root: Option<i64> = cmd("HGET")
            .arg(self.kb.key("endpoint"))
            .arg(shard as i64)
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(root.unwrap_or(0) as usize)
    }

    async fn set_meta(&mut self, record: usize, meta: &[u8]) -> Result<()> {
        let mut conn = self.lock().await;
        cmd("HSET")
            .arg(self.kb.key("meta"))
            .arg(record as i64)
            .arg(meta)
            .query_async::<()>(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_meta(&self, record: usize) -> Result<Option<Vec<u8>>> {
        let mut conn = self.lock().await;
        let meta: Option<Vec<u8>> = cmd("HGET")
            .arg(self.kb.key("meta"))
            .arg(record as i64)
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(meta)
    }

    async fn set_key_len(&mut self, record: usize, len: usize) -> Result<()> {
        let mut conn = self.lock().await;
        cmd("HSET")
            .arg(self.kb.key("keylen"))
            .arg(record as i64)
            .arg(len as i64)
            .query_async::<()>(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_key_len(&self, record: usize) -> Result<Option<usize>> {
        let mut conn = self.lock().await;
        let len: Option<i64> = cmd("HGET")
            .arg(self.kb.key("keylen"))
            .arg(record as i64)
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(len.map(|x| x as usize))
    }

    async fn add_shortcut_node(&mut self, shard: usize, elem: &[u8], node_id: usize) -> Result<()> {
        let mut conn = self.lock().await;
        cmd("SADD")
            .arg(self.kb.shortcut(shard, elem))
            .arg(node_id as i64)
            .query_async::<()>(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_shortcut_nodes(&self, shard: usize, elem: &[u8]) -> Result<Vec<usize>> {
        let mut conn = self.lock().await;
        let nodes: Vec<i64> = cmd("SMEMBERS")
            .arg(self.kb.shortcut(shard, elem))
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(nodes.into_iter().map(|x| x as usize).collect())
    }

    async fn clear_shortcuts(&mut self) -> Result<()> {
        let mut conn = self.lock().await;
        let pattern = format!("{}*", self.kb.shortcut_prefix());
        let mut cursor: u64 = 0;
        loop {
            let (next_cursor, keys): (u64, Vec<String>) = cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(500)
                .query_async(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
            for key in keys {
                cmd("DEL")
                    .arg(key)
                    .query_async::<()>(&mut *conn)
                    .await
                    .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
            }
            cursor = next_cursor;
            if cursor == 0 {
                break;
            }
        }
        Ok(())
    }

    async fn set_edge_data(&mut self, edge: usize, data: &[u8]) -> Result<()> {
        let mut conn = self.lock().await;
        cmd("HSET")
            .arg(self.kb.key("edgedata"))
            .arg(edge as i64)
            .arg(data)
            .query_async::<()>(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_edge_data(&self, edge: usize) -> Result<Option<Vec<u8>>> {
        let mut conn = self.lock().await;
        let data: Option<Vec<u8>> = cmd("HGET")
            .arg(self.kb.key("edgedata"))
            .arg(edge as i64)
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(data)
    }

    async fn clear_edges(&mut self) -> Result<()> {
        let mut conn = self.lock().await;
        cmd("DEL")
            .arg(self.kb.key("edgedata"))
            .query_async::<()>(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn for_each_edge_data(
        &self,
        f: &mut (dyn for<'a> FnMut(usize, &'a [u8]) -> Result<()> + Send),
    ) -> Result<()> {
        let mut conn = self.lock().await;
        let items: Vec<(i64, Vec<u8>)> = cmd("HGETALL")
            .arg(self.kb.key("edgedata"))
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        for (id, data) in items {
            f(id as usize, &data)?;
        }
        Ok(())
    }

    async fn set_node_meta(&mut self, elem: usize, meta: &[u8]) -> Result<()> {
        let mut conn = self.lock().await;
        cmd("HSET")
            .arg(self.kb.key("nodemeta"))
            .arg(elem as i64)
            .arg(meta)
            .query_async::<()>(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_node_meta(&self, elem: usize) -> Result<Option<Vec<u8>>> {
        let mut conn = self.lock().await;
        let meta: Option<Vec<u8>> = cmd("HGET")
            .arg(self.kb.key("nodemeta"))
            .arg(elem as i64)
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(meta)
    }

    async fn clear_node_meta(&mut self) -> Result<()> {
        let mut conn = self.lock().await;
        cmd("DEL")
            .arg(self.kb.key("nodemeta"))
            .query_async::<()>(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn set_chain(&mut self, record: usize, chain: &[u64]) -> Result<()> {
        let mut conn = self.lock().await;
        cmd("HSET")
            .arg(self.kb.key("chains"))
            .arg(record as i64)
            .arg(super::encode_chain(chain))
            .query_async::<()>(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_chain(&self, record: usize) -> Result<Option<Vec<u64>>> {
        let mut conn = self.lock().await;
        let bytes: Option<Vec<u8>> = cmd("HGET")
            .arg(self.kb.key("chains"))
            .arg(record as i64)
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(bytes.map(|b| super::decode_chain(&b)))
    }

    async fn clear_chains(&mut self) -> Result<()> {
        let mut conn = self.lock().await;
        cmd("DEL")
            .arg(self.kb.key("chains"))
            .query_async::<()>(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn save_symbol(&mut self, sym: &Symbol) -> Result<()> {
        let mut conn = self.lock().await;
        let data = serde_json::to_vec(sym).map_err(|e| StorageError::Internal(e.to_string()))?;
        cmd("HSET")
            .arg(self.kb.key("symbols"))
            .arg(sym.id as i64)
            .arg(data)
            .query_async::<()>(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn load_symbol(&self, id: u64) -> Result<Option<Symbol>> {
        let mut conn = self.lock().await;
        let data: Option<Vec<u8>> = cmd("HGET")
            .arg(self.kb.key("symbols"))
            .arg(id as i64)
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        data.map(|d| serde_json::from_slice(&d).map_err(|e| StorageError::Internal(e.to_string())))
            .transpose()
    }

    async fn load_all_symbols(&self) -> Result<Vec<Symbol>> {
        let mut conn = self.lock().await;
        let map: HashMap<String, Vec<u8>> = cmd("HGETALL")
            .arg(self.kb.key("symbols"))
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        let mut out: Vec<Symbol> = Vec::with_capacity(map.len());
        for data in map.into_values() {
            out.push(
                serde_json::from_slice(&data).map_err(|e| StorageError::Internal(e.to_string()))?,
            );
        }
        out.sort_by_key(|s| s.id);
        Ok(out)
    }

    async fn save_embedding(&mut self, symbol_id: u64, vector: &[f32]) -> Result<()> {
        let mut conn = self.lock().await;
        cmd("HSET")
            .arg(self.kb.key("embeddings"))
            .arg(symbol_id as i64)
            .arg(encode_vector(vector))
            .query_async::<()>(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn load_embedding(&self, symbol_id: u64) -> Result<Option<Vec<f32>>> {
        let mut conn = self.lock().await;
        let data: Option<Vec<u8>> = cmd("HGET")
            .arg(self.kb.key("embeddings"))
            .arg(symbol_id as i64)
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(data.and_then(|b| decode_vector(&b)))
    }

    async fn load_all_embeddings(&self) -> Result<HashMap<u64, Vec<f32>>> {
        let mut conn = self.lock().await;
        let map: HashMap<String, Vec<u8>> = cmd("HGETALL")
            .arg(self.kb.key("embeddings"))
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        let mut out = HashMap::with_capacity(map.len());
        for (k, v) in map {
            if let (Ok(id), Some(vec)) = (k.parse::<u64>(), decode_vector(&v)) {
                out.insert(id, vec);
            }
        }
        Ok(out)
    }

    async fn clear_embeddings(&mut self) -> Result<()> {
        let mut conn = self.lock().await;
        cmd("DEL")
            .arg(self.kb.key("embeddings"))
            .query_async::<()>(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn save_next_id(&mut self, next: u64) -> Result<()> {
        let mut conn = self.lock().await;
        cmd("SET")
            .arg(self.kb.key("nextid"))
            .arg(next as i64)
            .query_async::<()>(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn load_next_id(&self) -> Result<u64> {
        let mut conn = self.lock().await;
        let next: Option<i64> = cmd("GET")
            .arg(self.kb.key("nextid"))
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        // Registry chưa có symbol — bắt đầu từ SYMBOL_BASE (giống sqlite init).
        Ok(next
            .map(|n| n as u64)
            .unwrap_or(codegraph_core::SYMBOL_BASE))
    }

    async fn all_chains(&self) -> Result<Vec<(u64, Vec<u8>)>> {
        let mut conn = self.lock().await;
        let map: HashMap<i64, Vec<u8>> = cmd("HGETALL")
            .arg(self.kb.key("chains"))
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        let mut out: Vec<(u64, Vec<u8>)> = map.into_iter().map(|(r, b)| (r as u64, b)).collect();
        out.sort_by_key(|(r, _)| *r);
        Ok(out)
    }

    async fn set_call_records(&mut self, func: u64, records: &[u8]) -> Result<()> {
        let mut conn = self.lock().await;
        cmd("HSET")
            .arg(self.kb.key("callrecords"))
            .arg(func as i64)
            .arg(records)
            .query_async::<()>(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn get_call_records(&self, func: u64) -> Result<Option<Vec<u8>>> {
        let mut conn = self.lock().await;
        let records: Option<Vec<u8>> = cmd("HGET")
            .arg(self.kb.key("callrecords"))
            .arg(func as i64)
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(records)
    }

    async fn all_call_records(&self) -> Result<Vec<(u64, Vec<u8>)>> {
        let mut conn = self.lock().await;
        let map: HashMap<i64, Vec<u8>> = cmd("HGETALL")
            .arg(self.kb.key("callrecords"))
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        let mut out: Vec<(u64, Vec<u8>)> = map.into_iter().map(|(f, b)| (f as u64, b)).collect();
        out.sort_by_key(|(f, _)| *f);
        Ok(out)
    }

    async fn set_call_name_index(&mut self, name: &str, sites: &[u8]) -> Result<()> {
        let mut conn = self.lock().await;
        cmd("HSET")
            .arg(self.kb.key("callnames"))
            .arg(name)
            .arg(sites)
            .query_async::<()>(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn load_call_name_index(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let mut conn = self.lock().await;
        let sites: Option<Vec<u8>> = cmd("HGET")
            .arg(self.kb.key("callnames"))
            .arg(name)
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(sites)
    }

    async fn all_call_name_indexes(&self) -> Result<Vec<(String, Vec<u8>)>> {
        let mut conn = self.lock().await;
        let map: HashMap<String, Vec<u8>> = cmd("HGETALL")
            .arg(self.kb.key("callnames"))
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        let mut out: Vec<(String, Vec<u8>)> = map.into_iter().collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    async fn upsert_file(&mut self, f: &FileInfo) -> Result<()> {
        let mut conn = self.lock().await;
        let data = serde_json::to_vec(f).map_err(|e| StorageError::Internal(e.to_string()))?;
        cmd("HSET")
            .arg(self.kb.key("files"))
            .arg(&f.path)
            .arg(data)
            .query_async::<()>(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn load_all_files(&self) -> Result<Vec<FileInfo>> {
        let mut conn = self.lock().await;
        let map: HashMap<String, Vec<u8>> = cmd("HGETALL")
            .arg(self.kb.key("files"))
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        let mut out: Vec<FileInfo> = Vec::with_capacity(map.len());
        for data in map.into_values() {
            out.push(
                serde_json::from_slice(&data).map_err(|e| StorageError::Internal(e.to_string()))?,
            );
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    async fn version(&self) -> Result<u64> {
        let mut conn = self.lock().await;
        let v: Option<i64> = cmd("GET")
            .arg(self.kb.key("version"))
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(v.map(|n| n as u64).unwrap_or(0))
    }

    async fn set_version(&mut self, v: u64) -> Result<()> {
        let mut conn = self.lock().await;
        cmd("SET")
            .arg(self.kb.key("version"))
            .arg(v as i64)
            .query_async::<()>(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn clear_entities(&mut self) -> Result<()> {
        let mut conn = self.lock().await;
        cmd("DEL")
            .arg(self.kb.key("symbols"))
            .arg(self.kb.key("embeddings"))
            .arg(self.kb.key("nextid"))
            .arg(self.kb.key("callrecords"))
            .arg(self.kb.key("callnames"))
            .arg(self.kb.key("files"))
            .arg(self.kb.key("version"))
            .query_async::<()>(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }

    fn new_tx(&self) -> Box<dyn Tx> {
        Box::new(RedisTx {
            conn: self.conn.clone(),
            kb: self.kb.clone(),
            nodes: Vec::new(),
            ops: Vec::new(),
        })
    }
}

// ==================== Redis Transaction ====================

/// Transaction cho `RedisStorage`.
///
/// - `new_node` snapshot độ dài branch list lúc tạo tx, id = base + n
///   (giả định single-connection — toàn bộ command đi qua cùng 1 mutex).
/// - `commit` build một MULTI/EXEC pipeline: RPUSH toàn bộ node mới trước,
///   rồi áp dụng các op cấu trúc — atomic, không lộ trạng thái trung gian.
pub struct RedisTx {
    conn: Arc<Mutex<MultiplexedConnection>>,
    kb: KeyBuilder,
    nodes: Vec<(usize, Vec<u8>, usize)>,
    ops: Vec<TxOp>,
}

#[async_trait]
impl Tx for RedisTx {
    async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize> {
        let base = self.node_len_checked().await?;
        let id = base + self.nodes.len();
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
        let RedisTx {
            conn,
            kb,
            nodes,
            ops,
            ..
        } = *self;

        let mut conn = conn.lock().await;
        let mut pipe = redis::pipe();
        pipe.atomic();

        // 1. RPUSH toàn bộ node mới (sentinel đã có sẵn ở index 0).
        for (_, prefix, record) in &nodes {
            pipe.rpush(kb.key("branch"), &prefix[..]);
            pipe.rpush(kb.key("record"), *record as i64);
        }

        // 2. Áp dụng ops.
        for op in ops {
            match op {
                TxOp::AddChild { parent, child } => {
                    pipe.cmd("SADD")
                        .arg(kb.indexed("forward", parent))
                        .arg(child as i64)
                        .ignore();
                }
                TxOp::MoveChild { from, to, child } => {
                    pipe.cmd("SREM")
                        .arg(kb.indexed("forward", from))
                        .arg(child as i64)
                        .ignore();
                    pipe.cmd("SADD")
                        .arg(kb.indexed("forward", to))
                        .arg(child as i64)
                        .ignore();
                }
                TxOp::UpdateNode { id, prefix, record } => {
                    if let Some(p) = prefix {
                        pipe.lset(kb.key("branch"), id as isize, &p[..]);
                    }
                    if let Some(r) = record {
                        pipe.lset(kb.key("record"), id as isize, r as i64);
                    }
                }
            }
        }

        pipe.exec_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(())
    }
}

impl RedisTx {
    async fn node_len_checked(&self) -> Result<usize> {
        let mut conn = self.conn.lock().await;
        let len: usize = cmd("LLEN")
            .arg(self.kb.key("branch"))
            .query_async(&mut *conn)
            .await
            .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
        Ok(len)
    }
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU16, Ordering};

    use super::*;
    use crate::radix::EMPTY;
    use crate::storage::Storage;

    static COUNTER: AtomicU16 = AtomicU16::new(0);

    async fn new_test_storage() -> RedisStorage {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let client = redis::Client::open("redis://127.0.0.1:6379/15")
            .expect("redis connection failed — is redis-server running?");
        RedisStorage::new(client, &format!("test:radix:{}:{n}", pid))
            .await
            .expect("init failed")
    }

    #[tokio::test]
    async fn test_new_node_and_get_node() {
        let mut s = new_test_storage().await;
        let id = s.new_node(b"hello".to_vec(), 42).await.unwrap();
        assert_ne!(id, EMPTY);
        let (prefix, record) = s.get_node(id).await.unwrap();
        assert_eq!(prefix, b"hello");
        assert_eq!(record, 42);
    }

    #[tokio::test]
    async fn test_meta_roundtrip() {
        let mut s = new_test_storage().await;
        assert_eq!(s.get_meta(42).await.unwrap(), None);
        assert_eq!(s.get_key_len(42).await.unwrap(), None);
        s.set_meta(42, b"call-site-info").await.unwrap();
        s.set_key_len(42, 5).await.unwrap();
        assert_eq!(
            s.get_meta(42).await.unwrap().as_deref(),
            Some(b"call-site-info".as_slice())
        );
        assert_eq!(s.get_key_len(42).await.unwrap(), Some(5));
        s.set_meta(42, b"updated").await.unwrap();
        assert_eq!(
            s.get_meta(42).await.unwrap().as_deref(),
            Some(b"updated".as_slice())
        );
    }

    #[tokio::test]
    async fn test_shortcuts_roundtrip() {
        let mut s = new_test_storage().await;
        assert!(s.get_shortcut_nodes(1, b"l").await.unwrap().is_empty());
        s.add_shortcut_node(1, b"l", 10).await.unwrap();
        s.add_shortcut_node(1, b"l", 20).await.unwrap();
        s.add_shortcut_node(1, b"o", 10).await.unwrap();
        let nodes = s.get_shortcut_nodes(1, b"l").await.unwrap();
        assert!(nodes.contains(&10) && nodes.contains(&20));
        assert_eq!(nodes.len(), 2);
        s.clear_shortcuts().await.unwrap();
        assert!(s.get_shortcut_nodes(1, b"l").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_tx_split_commit() {
        let mut s = new_test_storage().await;
        let parent = s.new_node(b"hello".to_vec(), 1).await.unwrap();

        let mut tx = s.new_tx();
        let new_id = tx.new_node(b"p".to_vec(), 2).await.unwrap();
        let leg_id = tx.new_node(b"lo".to_vec(), 1).await.unwrap();
        tx.move_child(parent, leg_id, 0).await.unwrap();
        tx.add_child(parent, leg_id).await.unwrap();
        tx.add_child(parent, new_id).await.unwrap();
        tx.update_node(parent, Some(b"hel".to_vec()), Some(0))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let (prefix, _) = s.get_node(parent).await.unwrap();
        assert_eq!(prefix, b"hel");
        let children = s.get_children(parent).await.unwrap();
        assert!(children.contains(&leg_id));
        assert!(children.contains(&new_id));
    }
}
