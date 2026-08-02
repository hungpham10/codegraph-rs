use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use std::collections::{BTreeMap, HashMap};
use std::fmt;

// ==================== Error Type ====================

#[derive(Debug)]
pub enum StorageError {
    #[allow(dead_code)]
    BranchOutOfRange(usize),
    Internal(String),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageError::BranchOutOfRange(id) => write!(f, "branch id {id} out of range"),
            StorageError::Internal(msg) => write!(f, "internal error: {msg}"),
        }
    }
}

impl std::error::Error for StorageError {}

pub type Result<T> = std::result::Result<T, StorageError>;

const EMPTY: usize = 0;

/// Serialize-friendly container cho toàn bộ nodes trong 1 shard.
/// Dùng `bincode` + `zstd` để lưu thành 1 Redis key duy nhất.
#[derive(Serialize, Deserialize, Clone)]
pub struct ShardNodeData {
    /// prefixes indexed by node_id (index 0 = sentinel)
    pub prefixes: Vec<Vec<u8>>,
    /// records indexed by node_id
    pub records: Vec<usize>,
    /// children IDs per node, indexed by node_id
    pub children: Vec<Vec<usize>>,
}

#[async_trait]
pub trait Storage: Send + Sync {
    // ── Radix-style: node management ──
    async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize>;
    async fn update_node(
        &mut self,
        id: usize,
        prefix: Option<Vec<u8>>,
        record: Option<usize>,
    ) -> Result<()>;
    async fn add_child(&mut self, parent_id: usize, child_id: usize) -> Result<()>;
    async fn get_node(&self, id: usize) -> Result<(Vec<u8>, usize)>;
    async fn get_children(&self, id: usize) -> Result<Vec<usize>>;
    async fn set_root(&mut self, shard: usize, root_id: usize) -> Result<()>;
    async fn get_root(&self, shard: usize) -> Result<usize>;

    /// Lấy children + prefix + record của từng child trong MỘT lần fetch (batch).
    /// Dùng cho walk-down trong prefix search — tránh O(fanout) `get_node` riêng lẻ.
    /// Default: `get_children` + `get_node` từng child — override ở storage có bulk.
    async fn get_children_with_prefixes(&self, id: usize) -> Result<Vec<(usize, Vec<u8>, usize)>> {
        let children = self.get_children(id).await?;
        let mut out = Vec::with_capacity(children.len());
        for &child in &children {
            let (prefix, record) = self.get_node(child).await?;
            out.push((child, prefix, record));
        }
        Ok(out)
    }

    /// Quét toàn bộ subtree từ `node_id` → `(parent, child, prefix, record)`,
    /// root có `parent = None`. Dùng cho phần "scan ra" của prefix search:
    /// 1 lần fetch cả subtree (override bằng recursive SQL) thay vì
    /// get_node/get_children cho từng node. Caller tái dựng key bằng DFS trong bộ nhớ.
    async fn scan_subtree(
        &self,
        node_id: usize,
    ) -> Result<Vec<(Option<usize>, usize, Vec<u8>, usize)>> {
        let mut out = Vec::new();
        let mut stack = vec![(None, node_id)];
        while let Some((parent, cur)) = stack.pop() {
            let (prefix, record) = self.get_node(cur).await?;
            out.push((parent, cur, prefix, record));
            let children = self.get_children(cur).await?;
            for child in children {
                stack.push((Some(cur), child));
            }
        }
        Ok(out)
    }

    // ── Bulk write mode (VD: rebuild transaction) ──

    /// Bắt đầu bulk insert — backend có thể mở transaction để gộp nhiều insert
    /// thành 1 commit (cắt chi phí autocommit per-write khi rebuild index).
    /// Default no-op. Gọi `end_bulk` để commit.
    async fn begin_bulk(&mut self) -> Result<()> {
        Ok(())
    }

    /// Kết thúc bulk insert — commit transaction (nếu có).
    async fn end_bulk(&mut self) -> Result<()> {
        Ok(())
    }

    // ── Automaton-style: state machine ──
    async fn add_state(&mut self, label: &str) -> Result<usize>;
    async fn set_transition(&mut self, from: usize, label: &str, to: usize) -> Result<()>;
    async fn get_transitions(&self, from: usize) -> Result<Vec<(String, usize)>>;
    async fn set_failure(&mut self, state: usize, fail: usize) -> Result<()>;
    async fn get_failure(&self, state: usize) -> Result<usize>;
    async fn set_output(&mut self, state: usize, pattern_idx: usize) -> Result<()>;
    async fn get_output(&self, state: usize) -> Result<Option<usize>>;
    async fn add_root_input(&mut self, state: usize) -> Result<()>;
    async fn get_root_inputs(&self) -> Result<Vec<usize>>;
    async fn get_label(&self, state: usize) -> Result<String>;
    async fn num_states(&self) -> Result<usize>;

    // ── Tree management ──
    /// Xoá tất cả children của một node (dùng trong split).
    async fn clear_children(&mut self, _parent_id: usize) -> Result<()> {
        // Default no-op để không break implementors cũ
        Ok(())
    }

    /// Xoá một child cụ thể của node (dùng trong split an toàn với Set).
    async fn remove_child(&mut self, _parent_id: usize, _child_id: usize) -> Result<()> {
        // Default no-op
        Ok(())
    }

    /// Atomic commit của radix split: update prefix/record + xoá old children
    /// trong một lần. Storage implementation phải đảm bảo hoặc tất cả thành
    /// công hoặc không thay đổi gì, để crash không để lại tree không navigate được.
    async fn commit_split(
        &mut self,
        parent: usize,
        root_prefix: Vec<u8>,
        new_record: usize,
        children_to_remove: &[usize],
    ) -> Result<()> {
        // Default: fallback về sequential (không atomic) — override ở Redis
        for &child in children_to_remove {
            self.remove_child(parent, child).await?;
        }
        self.update_node(parent, Some(root_prefix), Some(new_record))
            .await
    }

    // ── Persistence for reload ──
    async fn save_entries(&mut self, entries: &[(i32, String)]) -> Result<()>;
    async fn load_entries(&self) -> Result<Vec<(i32, String)>>;

    /// Load individual entry by 1-indexed record index.
    /// Dùng trong non-legacy mode để resolve tree's record → (i32, String).
    /// Default: fallback về load_entries() + index (chậm nhưng backward compatible).
    async fn load_entry(&self, idx: usize) -> Result<(i32, String)> {
        let entries = self.load_entries().await?;
        entries
            .get(idx.checked_sub(1).ok_or_else(|| {
                StorageError::Internal("invalid entry index 0 (must be 1-indexed)".into())
            })?)
            .cloned()
            .ok_or_else(|| StorageError::Internal(format!("entry at index {idx} not found")))
    }

    /// Save individual entry (atomic per-entry).
    /// Default: fallback về load_entries() + set + save_entries (chậm).
    async fn save_entry(&mut self, idx: usize, entry_id: i32, name: &str) -> Result<()> {
        let mut entries = self.load_entries().await?;
        let idx0 = idx.checked_sub(1).ok_or_else(|| {
            StorageError::Internal("invalid entry index 0 (must be 1-indexed)".into())
        })?;
        if idx0 >= entries.len() {
            entries.resize(idx0 + 1, (0, String::new()));
        }
        entries[idx0] = (entry_id, name.to_string());
        self.save_entries(&entries).await
    }

    /// Save metadata gắn với một record idx (opaque bytes, VD: call-site info
    /// của edge). Record idx = ID tự nhiên của entry → dùng để enrich.
    /// Default: no-op — backend không hỗ trợ meta.
    async fn save_entry_meta(&mut self, _idx: usize, _meta: &[u8]) -> Result<()> {
        Ok(())
    }

    /// Load metadata gắn với record idx.
    /// Default: None — backend không lưu meta.
    async fn load_entry_meta(&self, _idx: usize) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    /// Count total entries in storage.
    /// Default: load_entries().len() (chậm nhưng backward compatible).
    async fn count_entries(&self) -> Result<usize> {
        Ok(self.load_entries().await?.len())
    }

    /// Atomically allocate a unique record ID.
    ///
    /// - Redis: `INCR {prefix}:record_counter` — atomic across all instances.
    /// - InMemory: local counter.
    ///
    /// Returns a 1-indexed ID that is guaranteed unique across all instances
    /// sharing the same storage backend. This eliminates the race condition
    /// that existed with the local `record_counter` field.
    async fn allocate_record_id(&mut self) -> Result<usize>;

    /// Initialize the record counter for a given count (used during reload).
    ///
    /// - Redis: `SET {prefix}:record_counter {count} NX` — only if not set,
    ///   to avoid overwriting a counter from another active instance.
    /// - InMemory: always resets the local counter.
    async fn init_record_counter(&mut self, count: usize) -> Result<()>;

    // ── Generic blob storage (cho bloom filters, etc.) ──

    /// Save arbitrary binary data by key.
    /// Dùng để persist bloom filters hoặc dữ liệu không cấu trúc khác.
    async fn save_blob(&mut self, key: &str, data: &[u8]) -> Result<()>;

    /// Load arbitrary binary data by key.
    /// Trả về `None` nếu key không tồn tại.
    async fn load_blob(&self, key: &str) -> Result<Option<Vec<u8>>>;

    // ── Shard-level bulk save/load (compressed blob) ──

    /// Save toàn bộ node data của 1 shard thành 1 compressed blob.
    /// Default no-op (not supported by all storage backends).
    async fn save_shard(&mut self, _shard: usize, _data: &ShardNodeData) -> Result<()> {
        Ok(())
    }

    /// Load node data của 1 shard từ compressed blob.
    /// Trả về `None` nếu chưa có blob (not supported hoặc chưa migrate).
    async fn load_shard(&self, _shard: usize) -> Result<Option<ShardNodeData>> {
        Ok(None)
    }
}

// ==================== In-Memory Storage (Radix + Automaton) ====================

pub struct InMemoryStorage {
    // ── Radix data ──
    nodes: Vec<(Vec<u8>, usize)>,
    children: Vec<Vec<usize>>,
    roots: Vec<usize>,

    // ── Automaton data ──
    labels: Vec<String>,
    transitions: Vec<BTreeMap<String, usize>>,
    failures: Vec<usize>,
    outputs: BTreeMap<usize, usize>,
    root_inputs: Vec<usize>,

    // ── Persistence for reload ──
    entries_data: Vec<(i32, String)>,
    /// Metadata theo record idx (1-indexed) — enrich cho entry/edge.
    entries_meta: HashMap<usize, Vec<u8>>,

    // ── Atomic record ID counter (non-legacy mode) ──
    /// Local counter for atomically allocating unique record IDs.
    /// 0-based, increments on each call → returns 1-indexed IDs.
    id_counter: usize,

    // ── Generic blob storage ──
    blobs: HashMap<String, Vec<u8>>,
}

impl Default for InMemoryStorage {
    fn default() -> Self {
        Self {
            // Radix sentinel tại index 0
            nodes: vec![(vec![], 0)],
            children: vec![vec![]],
            roots: vec![],

            // Automaton root state tại index 0 (dùng chung sentinel với radix)
            labels: vec![String::new()],
            transitions: vec![BTreeMap::new()],
            failures: vec![0],
            outputs: BTreeMap::new(),
            root_inputs: Vec::new(),
            entries_data: Vec::new(),
            entries_meta: HashMap::new(),
            id_counter: 0,
            blobs: HashMap::new(),
        }
    }
}

#[async_trait]
impl Storage for InMemoryStorage {
    // ==================== Radix Methods ====================

    async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize> {
        let id = self.nodes.len();
        self.nodes.push((prefix, record));
        self.children.push(Vec::new());
        Ok(id)
    }

    async fn update_node(
        &mut self,
        id: usize,
        prefix: Option<Vec<u8>>,
        record: Option<usize>,
    ) -> Result<()> {
        if let Some(p) = prefix {
            self.nodes[id].0 = p;
        }
        if let Some(r) = record {
            self.nodes[id].1 = r;
        }
        Ok(())
    }

    async fn add_child(&mut self, parent_id: usize, child_id: usize) -> Result<()> {
        self.children[parent_id].push(child_id);
        Ok(())
    }

    async fn clear_children(&mut self, parent_id: usize) -> Result<()> {
        if parent_id < self.children.len() {
            self.children[parent_id].clear();
        }
        Ok(())
    }

    async fn remove_child(&mut self, parent_id: usize, child_id: usize) -> Result<()> {
        if parent_id < self.children.len() {
            self.children[parent_id].retain(|&c| c != child_id);
        }
        Ok(())
    }

    async fn get_node(&self, id: usize) -> Result<(Vec<u8>, usize)> {
        if id >= self.nodes.len() {
            return Err(StorageError::BranchOutOfRange(id));
        }
        Ok(self.nodes[id].clone())
    }

    async fn get_children(&self, id: usize) -> Result<Vec<usize>> {
        if id >= self.children.len() {
            return Ok(vec![]);
        }
        Ok(self.children[id].clone())
    }

    async fn set_root(&mut self, shard: usize, root_id: usize) -> Result<()> {
        if shard >= self.roots.len() {
            self.roots.resize(shard + 1, 0);
        }
        self.roots[shard] = root_id;
        Ok(())
    }

    async fn get_root(&self, shard: usize) -> Result<usize> {
        Ok(self.roots.get(shard).copied().unwrap_or(EMPTY))
    }

    // ── Persistence for reload ──
    async fn save_entries(&mut self, entries: &[(i32, String)]) -> Result<()> {
        self.entries_data = entries.to_vec();
        Ok(())
    }

    async fn load_entries(&self) -> Result<Vec<(i32, String)>> {
        Ok(self.entries_data.clone())
    }

    async fn load_entry(&self, idx: usize) -> Result<(i32, String)> {
        let idx0 = idx.checked_sub(1).ok_or_else(|| {
            StorageError::Internal("invalid entry index 0 (must be 1-indexed)".into())
        })?;
        self.entries_data
            .get(idx0)
            .cloned()
            .ok_or_else(|| StorageError::Internal(format!("entry at index {idx} not found")))
    }

    async fn save_entry(&mut self, idx: usize, entry_id: i32, name: &str) -> Result<()> {
        let idx0 = idx.checked_sub(1).ok_or_else(|| {
            StorageError::Internal("invalid entry index 0 (must be 1-indexed)".into())
        })?;
        if idx0 >= self.entries_data.len() {
            self.entries_data.resize(idx0 + 1, (0, String::new()));
        }
        self.entries_data[idx0] = (entry_id, name.to_string());
        Ok(())
    }

    async fn count_entries(&self) -> Result<usize> {
        Ok(self.entries_data.len())
    }

    async fn save_entry_meta(&mut self, idx: usize, meta: &[u8]) -> Result<()> {
        self.entries_meta.insert(idx, meta.to_vec());
        Ok(())
    }

    async fn load_entry_meta(&self, idx: usize) -> Result<Option<Vec<u8>>> {
        Ok(self.entries_meta.get(&idx).cloned())
    }

    async fn allocate_record_id(&mut self) -> Result<usize> {
        self.id_counter += 1;
        Ok(self.id_counter)
    }

    async fn init_record_counter(&mut self, count: usize) -> Result<()> {
        self.id_counter = count;
        Ok(())
    }

    async fn save_blob(&mut self, key: &str, data: &[u8]) -> Result<()> {
        self.blobs.insert(key.to_string(), data.to_vec());
        Ok(())
    }

    async fn load_blob(&self, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.blobs.get(key).cloned())
    }

    // ==================== Automaton Methods ====================

    async fn add_state(&mut self, label: &str) -> Result<usize> {
        let id = self.labels.len();
        self.labels.push(label.to_string());
        self.transitions.push(BTreeMap::new());
        self.failures.push(0);
        Ok(id)
    }

    async fn set_transition(&mut self, from: usize, label: &str, to: usize) -> Result<()> {
        self.transitions[from].insert(label.to_string(), to);
        Ok(())
    }

    async fn get_transitions(&self, from: usize) -> Result<Vec<(String, usize)>> {
        Ok(self.transitions[from].clone().into_iter().collect())
    }

    async fn set_failure(&mut self, state: usize, fail: usize) -> Result<()> {
        self.failures[state] = fail;
        Ok(())
    }

    async fn get_failure(&self, state: usize) -> Result<usize> {
        Ok(self.failures[state])
    }

    async fn set_output(&mut self, state: usize, pattern_idx: usize) -> Result<()> {
        self.outputs.insert(state, pattern_idx);
        Ok(())
    }

    async fn get_output(&self, state: usize) -> Result<Option<usize>> {
        Ok(self.outputs.get(&state).copied())
    }

    async fn add_root_input(&mut self, state: usize) -> Result<()> {
        self.root_inputs.push(state);
        Ok(())
    }

    async fn get_root_inputs(&self) -> Result<Vec<usize>> {
        Ok(self.root_inputs.clone())
    }

    async fn get_label(&self, state: usize) -> Result<String> {
        if state >= self.labels.len() {
            return Err(StorageError::BranchOutOfRange(state));
        }
        Ok(self.labels[state].clone())
    }

    async fn num_states(&self) -> Result<usize> {
        Ok(self.transitions.len())
    }
}

// =========================================================================
//  Redis Storage
//  (chỉ build khi feature "redis" được bật)
// =========================================================================

#[cfg(feature = "redis")]
pub mod redis {
    //! Redis-backed Storage implementation (Radix + Automaton).
    //!
    //! ## Cấu trúc key
    //!
    //! | Key                        | Kiểu  | Mục đích                        |
    //! |----------------------------|-------|---------------------------------|
    //! | `{prefix}:branch`          | List  | prefix của từng node            |
    //! | `{prefix}:record`          | List  | record của từng node            |
    //! | `{prefix}:forward:{id}`    | Set   | children list của node          |
    //! | `{prefix}:endpoint`        | Hash  | root ID cho mỗi shard           |
    //! | `{prefix}:entries_blob`    | String| entries zstd blob               |
    //! | `{prefix}:record_counter`  | String| atomic counter (INCR)           |
    //! | `{prefix}:shard:{shard}`   | String| node data zstd blob per shard   |
    //! | `{prefix}:{blob_key}`      | String| binary blobs (bloom filters...) |
    //! | `{prefix}:label`           | List  | label của từng state            |
    //! | `{prefix}:trans:{id}`      | Hash  | transitions của state           |
    //! | `{prefix}:failure`         | List  | failure link của state          |
    //! | `{prefix}:output`          | Hash  | output (pattern_idx) của state  |
    //! | `{prefix}:root_inputs`     | List  | danh sách root input states     |

    use std::sync::Arc;

    use redis::aio::MultiplexedConnection;
    use tokio::sync::{Mutex, RwLock};

    use super::{Result, ShardNodeData, Storage, StorageError};

    // ==================== KeyBuilder ====================

    type KeyFormatter = Arc<dyn Fn(&str) -> String + Send + Sync>;

    /// Cấu hình key cho Redis storage.
    ///
    /// Mặc định format: `{prefix}:{name}` và `{prefix}:{name}:{id}`.
    /// Có thể dùng `with_formatter` để custom hoàn toàn.
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

        /// Dùng custom formatter thay vì default `{prefix}:{name}`.
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
    }

    /// Helper shorthand: `cmd("LLEN")` → `redis::cmd("LLEN")`
    fn cmd(name: &str) -> redis::Cmd {
        redis::cmd(name)
    }

    // ==================== RedisStorage ====================

    pub struct RedisStorage {
        conn: Arc<Mutex<MultiplexedConnection>>,
        kb: KeyBuilder,
        /// In-memory cache of entries, loaded from compressed zstd blob or old Hash.
        /// `load_entry()` reads from here — zero Redis calls at search time.
        entries_cache: RwLock<Vec<(i32, String)>>,
    }

    impl RedisStorage {
        /// Helper: lock the mutex, unwrap on poison.
        async fn lock(&self) -> tokio::sync::MutexGuard<'_, MultiplexedConnection> {
            self.conn.lock().await
        }

        /// Tạo storage từ `redis::Client` (async).
        pub async fn new(client: redis::Client, prefix: &str) -> Result<Self> {
            let conn = client
                .get_multiplexed_async_connection()
                .await
                .map_err(|e| StorageError::Internal(e.to_string()))?;

            let s = Self {
                conn: Arc::new(Mutex::new(conn)),
                kb: KeyBuilder::new(prefix),
                entries_cache: RwLock::new(Vec::new()),
            };
            s.init().await?;
            Ok(s)
        }

        /// Tạo storage từ `MultiplexedConnection` có sẵn (vd từ `Resolver::cache()`).
        pub async fn from_multiplexed(conn: MultiplexedConnection, prefix: &str) -> Result<Self> {
            let s = Self {
                conn: Arc::new(Mutex::new(conn)),
                kb: KeyBuilder::new(prefix),
                entries_cache: RwLock::new(Vec::new()),
            };
            s.init().await?;
            Ok(s)
        }

        /// Tạo storage với `KeyBuilder` tuỳ chỉnh + client.
        pub async fn with_key_builder(client: redis::Client, kb: KeyBuilder) -> Result<Self> {
            let conn = client
                .get_multiplexed_async_connection()
                .await
                .map_err(|e| StorageError::Internal(e.to_string()))?;

            let s = Self {
                conn: Arc::new(Mutex::new(conn)),
                kb,
                entries_cache: RwLock::new(Vec::new()),
            };
            s.init().await?;
            Ok(s)
        }

        /// Tạo storage với `MultiplexedConnection` + `KeyBuilder` custom.
        pub async fn from_multiplexed_with_key_builder(
            conn: MultiplexedConnection,
            kb: KeyBuilder,
        ) -> Result<Self> {
            let s = Self {
                conn: Arc::new(Mutex::new(conn)),
                kb,
                entries_cache: RwLock::new(Vec::new()),
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
                    .rpush(self.kb.key("label"), "")
                    .rpush(self.kb.key("failure"), 0i64)
                    .exec_async(&mut *conn)
                    .await
                    .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
            }

            Ok(())
        }

        // ── Compression helpers for entries ──

        /// Serialize + zstd-compress entries vector.
        fn compress_entries(entries: &[(i32, String)]) -> Result<Vec<u8>> {
            let bytes = bincode::serialize(entries)
                .map_err(|e| StorageError::Internal(format!("bincode: {e}")))?;
            zstd::encode_all(&bytes[..], 3)
                .map_err(|e| StorageError::Internal(format!("zstd compress: {e}")))
        }

        /// zstd-decompress + deserialize entries vector.
        fn decompress_entries(data: &[u8]) -> Result<Vec<(i32, String)>> {
            let bytes = zstd::decode_all(data)
                .map_err(|e| StorageError::Internal(format!("zstd decompress: {e}")))?;
            bincode::deserialize(&bytes)
                .map_err(|e| StorageError::Internal(format!("bincode: {e}")))
        }
    }

    #[async_trait::async_trait]
    impl Storage for RedisStorage {
        // ==================== Radix Methods ====================

        async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize> {
            let mut conn = self.lock().await;

            // Atomic pipeline: cả 2 RPUSH trong cùng MULTI/EXEC.
            // EXEC trả về array [len_branch, len_record] — lấy len từ RPUSH branch.
            // Cách này tránh race condition LLEN sau atomic pipe (nếu 2 connections
            // cùng gọi new_node, LLEN có thể thấy tổng cả 2).
            // ⚡ query_async trả về Value (exec_async trả về () — không dùng được)
            let result: redis::Value = redis::pipe()
                .atomic()
                .rpush(self.kb.key("branch"), &prefix[..])
                .rpush(self.kb.key("record"), record as i64)
                .query_async(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;

            // Parse EXEC response: Value::Array([Value::Int(len), Value::Int(...)])
            let len: usize = match result {
                redis::Value::Array(ref items) => match items.first() {
                    Some(redis::Value::Int(n)) => *n as usize,
                    _ => {
                        // Fallback: LLEN (nếu response format khác mong đợi)
                        let llen = cmd("LLEN")
                            .arg(self.kb.key("branch"))
                            .query_async::<usize>(&mut *conn)
                            .await;
                        match llen {
                            Ok(l) => l,
                            Err(e) => return Err(StorageError::Internal(e.to_string())),
                        }
                    }
                },
                _ => {
                    let llen = cmd("LLEN")
                        .arg(self.kb.key("branch"))
                        .query_async::<usize>(&mut *conn)
                        .await;
                    match llen {
                        Ok(l) => l,
                        Err(e) => return Err(StorageError::Internal(e.to_string())),
                    }
                }
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

        async fn add_child(&mut self, parent_id: usize, child_id: usize) -> Result<()> {
            let mut conn = self.lock().await;

            cmd("SADD")
                .arg(self.kb.indexed("forward", parent_id))
                .arg(child_id as i64)
                .query_async::<()>(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;

            Ok(())
        }

        async fn clear_children(&mut self, parent_id: usize) -> Result<()> {
            let mut conn = self.lock().await;

            cmd("DEL")
                .arg(self.kb.indexed("forward", parent_id))
                .query_async::<()>(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;

            Ok(())
        }

        async fn remove_child(&mut self, parent_id: usize, child_id: usize) -> Result<()> {
            let mut conn = self.lock().await;

            cmd("SREM")
                .arg(self.kb.indexed("forward", parent_id))
                .arg(child_id as i64)
                .query_async::<()>(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;

            Ok(())
        }

        /// Atomic split commit: update prefix/record + SREM tất cả old children
        /// trong một MULTI/EXEC, đảm bảo crash không để tree ở trạng thái không
        /// navigate được (old prefix + children đã xoá).
        async fn commit_split(
            &mut self,
            parent: usize,
            root_prefix: Vec<u8>,
            new_record: usize,
            children_to_remove: &[usize],
        ) -> Result<()> {
            let mut conn = self.lock().await;

            let mut pipe = redis::pipe();
            pipe.atomic();
            pipe.lset(self.kb.key("branch"), parent as isize, &root_prefix[..]);
            pipe.lset(self.kb.key("record"), parent as isize, new_record as i64);
            for &child in children_to_remove {
                pipe.cmd("SREM")
                    .arg(self.kb.indexed("forward", parent))
                    .arg(child as i64)
                    .ignore();
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

        async fn set_root(&mut self, shard: usize, root_id: usize) -> Result<()> {
            let mut conn = self.lock().await;

            cmd("HSET")
                .arg(self.kb.key("endpoint"))
                .arg(shard as i64)
                .arg(root_id as i64)
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

        // ── Persistence for reload ──
        // Entries stored as compressed zstd blob: {prefix}:entries_blob
        //   value = bincode(Vec<(i32, String)>) compressed with zstd level 3
        //
        // In-memory cache `entries_cache` avoids Redis calls at search time.
        //
        // Lưu ý: `save_entry` chỉ update cache (không gọi Redis).
        // Blob được persist qua `save_entries` (gọi sau insert batch).

        /// Save entries: compress to zstd blob + update cache.
        async fn save_entries(&mut self, entries: &[(i32, String)]) -> Result<()> {
            *self.entries_cache.write().await = entries.to_vec();

            let compressed = Self::compress_entries(entries)?;
            let mut conn = self.lock().await;
            cmd("SET")
                .arg(self.kb.key("entries_blob"))
                .arg(&compressed)
                .query_async::<()>(&mut *conn)
                .await
                .map_err(|e| StorageError::Internal(e.to_string()))?;
            Ok(())
        }

        /// Load entries từ compressed blob, populate cache.
        async fn load_entries(&self) -> Result<Vec<(i32, String)>> {
            {
                let cache = self.entries_cache.read().await;
                if !cache.is_empty() {
                    return Ok(cache.clone());
                }
            }

            let mut conn = self.lock().await;
            let blob: Option<Vec<u8>> = cmd("GET")
                .arg(self.kb.key("entries_blob"))
                .query_async(&mut *conn)
                .await
                .map_err(|e| StorageError::Internal(e.to_string()))?;

            let entries = match blob {
                Some(data) => Self::decompress_entries(&data)?,
                None => Vec::new(),
            };

            *self.entries_cache.write().await = entries.clone();
            Ok(entries)
        }

        /// Load individual entry từ in-memory cache (zero Redis calls).
        async fn load_entry(&self, idx: usize) -> Result<(i32, String)> {
            let idx0 = idx.checked_sub(1).ok_or_else(|| {
                StorageError::Internal("invalid entry index 0 (must be 1-indexed)".into())
            })?;

            let cache = self.entries_cache.read().await;
            cache.get(idx0).cloned().ok_or_else(|| {
                StorageError::Internal(format!("entry at index {idx} not found (cache cold?)"))
            })
        }

        /// Save individual entry: update cache (không gọi Redis).
        /// Blob được persist qua `save_entries` sau insert batch.
        async fn save_entry(&mut self, idx: usize, entry_id: i32, name: &str) -> Result<()> {
            let idx0 = idx.checked_sub(1).ok_or_else(|| {
                StorageError::Internal("invalid entry index 0 (must be 1-indexed)".into())
            })?;

            let mut cache = self.entries_cache.write().await;
            if idx0 >= cache.len() {
                cache.resize(idx0 + 1, (0, String::new()));
            }
            cache[idx0] = (entry_id, name.to_string());
            Ok(())
        }

        async fn count_entries(&self) -> Result<usize> {
            let cache = self.entries_cache.read().await;
            if !cache.is_empty() {
                return Ok(cache.len());
            }
            // Cold start: load entries to populate cache
            drop(cache);
            self.load_entries().await?;
            Ok(self.entries_cache.read().await.len())
        }

        async fn allocate_record_id(&mut self) -> Result<usize> {
            let mut conn = self.lock().await;
            let id: i64 = cmd("INCR")
                .arg(self.kb.key("record_counter"))
                .query_async(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
            Ok(id as usize)
        }

        async fn init_record_counter(&mut self, count: usize) -> Result<()> {
            let mut conn = self.lock().await;
            // SET NX: only set if key doesn't exist yet.
            // Prevents overwriting a counter from another active instance.
            let _: Option<String> = cmd("SET")
                .arg(self.kb.key("record_counter"))
                .arg(count as i64)
                .arg("NX")
                .query_async(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
            Ok(())
        }

        async fn save_blob(&mut self, key: &str, data: &[u8]) -> Result<()> {
            let mut conn = self.lock().await;
            cmd("SET")
                .arg(self.kb.key(key))
                .arg(data)
                .query_async::<()>(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
            Ok(())
        }

        async fn load_blob(&self, key: &str) -> Result<Option<Vec<u8>>> {
            let mut conn = self.lock().await;
            let val: Option<Vec<u8>> = cmd("GET")
                .arg(self.kb.key(key))
                .query_async(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
            Ok(val)
        }

        // ── Shard-level compressed blob (override Storage trait defaults) ──

        async fn save_shard(&mut self, shard: usize, data: &ShardNodeData) -> Result<()> {
            let bytes = bincode::serialize(data)
                .map_err(|e| StorageError::Internal(format!("bincode shard: {e}")))?;
            let compressed = zstd::encode_all(&bytes[..], 3)
                .map_err(|e| StorageError::Internal(format!("zstd shard: {e}")))?;

            let mut conn = self.lock().await;
            cmd("SET")
                .arg(self.kb.indexed("shard", shard))
                .arg(&compressed)
                .query_async::<()>(&mut *conn)
                .await
                .map_err(|e| StorageError::Internal(e.to_string()))?;
            Ok(())
        }

        async fn load_shard(&self, shard: usize) -> Result<Option<ShardNodeData>> {
            let mut conn = self.lock().await;
            let blob: Option<Vec<u8>> = cmd("GET")
                .arg(self.kb.indexed("shard", shard))
                .query_async(&mut *conn)
                .await
                .map_err(|e| StorageError::Internal(e.to_string()))?;

            match blob {
                Some(data) => {
                    let bytes = zstd::decode_all(&data[..])
                        .map_err(|e| StorageError::Internal(format!("zstd shard: {e}")))?;
                    let shard_data: ShardNodeData = bincode::deserialize(&bytes)
                        .map_err(|e| StorageError::Internal(format!("bincode shard: {e}")))?;
                    Ok(Some(shard_data))
                }
                None => Ok(None),
            }
        }

        // ==================== Automaton Methods ====================

        async fn add_state(&mut self, label: &str) -> Result<usize> {
            let mut conn = self.lock().await;

            // Atomic pipeline: label + failure trong cùng MULTI/EXEC
            // EXEC trả về [len_label, len_failure] — parse từ phần tử đầu
            let result: redis::Value = redis::pipe()
                .atomic()
                .rpush(self.kb.key("label"), label)
                .rpush(self.kb.key("failure"), 0i64)
                .query_async(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;

            let len: usize = match result {
                redis::Value::Array(ref items) => match items.first() {
                    Some(redis::Value::Int(n)) => *n as usize,
                    _ => {
                        let llen = cmd("LLEN")
                            .arg(self.kb.key("label"))
                            .query_async::<usize>(&mut *conn)
                            .await;
                        match llen {
                            Ok(l) => l,
                            Err(e) => return Err(StorageError::Internal(e.to_string())),
                        }
                    }
                },
                _ => {
                    let llen = cmd("LLEN")
                        .arg(self.kb.key("label"))
                        .query_async::<usize>(&mut *conn)
                        .await;
                    match llen {
                        Ok(l) => l,
                        Err(e) => return Err(StorageError::Internal(e.to_string())),
                    }
                }
            };

            Ok(len - 1)
        }

        async fn set_transition(&mut self, from: usize, label: &str, to: usize) -> Result<()> {
            let mut conn = self.lock().await;

            cmd("HSET")
                .arg(self.kb.indexed("trans", from))
                .arg(label)
                .arg(to as i64)
                .query_async::<()>(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;

            Ok(())
        }

        async fn get_transitions(&self, from: usize) -> Result<Vec<(String, usize)>> {
            let mut conn = self.lock().await;

            let pairs: Vec<(String, String)> = cmd("HGETALL")
                .arg(self.kb.indexed("trans", from))
                .query_async(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;

            Ok(pairs
                .into_iter()
                .map(|(k, v)| (k, v.parse::<usize>().unwrap_or(0)))
                .collect())
        }

        async fn set_failure(&mut self, state: usize, fail: usize) -> Result<()> {
            let mut conn = self.lock().await;

            cmd("LSET")
                .arg(self.kb.key("failure"))
                .arg(state as isize)
                .arg(fail as i64)
                .query_async::<()>(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;

            Ok(())
        }

        async fn get_failure(&self, state: usize) -> Result<usize> {
            let mut conn = self.lock().await;

            let val: Option<i64> = cmd("LINDEX")
                .arg(self.kb.key("failure"))
                .arg(state as isize)
                .query_async(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;

            Ok(val.unwrap_or(0) as usize)
        }

        async fn set_output(&mut self, state: usize, pattern_idx: usize) -> Result<()> {
            let mut conn = self.lock().await;

            cmd("HSET")
                .arg(self.kb.key("output"))
                .arg(state as i64)
                .arg(pattern_idx as i64)
                .query_async::<()>(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;

            Ok(())
        }

        async fn get_output(&self, state: usize) -> Result<Option<usize>> {
            let mut conn = self.lock().await;

            let val: Option<i64> = cmd("HGET")
                .arg(self.kb.key("output"))
                .arg(state as i64)
                .query_async(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;

            Ok(val.map(|v| v as usize))
        }

        async fn add_root_input(&mut self, state: usize) -> Result<()> {
            let mut conn = self.lock().await;

            cmd("RPUSH")
                .arg(self.kb.key("root_inputs"))
                .arg(state as i64)
                .query_async::<()>(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;

            Ok(())
        }

        async fn get_root_inputs(&self) -> Result<Vec<usize>> {
            let mut conn = self.lock().await;

            let vals: Vec<i64> = cmd("LRANGE")
                .arg(self.kb.key("root_inputs"))
                .arg(0i64)
                .arg(-1i64)
                .query_async(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;

            Ok(vals.into_iter().map(|v| v as usize).collect())
        }

        async fn get_label(&self, state: usize) -> Result<String> {
            let mut conn = self.lock().await;

            let val: Option<Vec<u8>> = cmd("LINDEX")
                .arg(self.kb.key("label"))
                .arg(state as isize)
                .query_async(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;

            match val {
                Some(bytes) => {
                    String::from_utf8(bytes).map_err(|e| StorageError::Internal(e.to_string()))
                }
                None => Ok(String::new()),
            }
        }

        async fn num_states(&self) -> Result<usize> {
            let mut conn = self.lock().await;

            let n: usize = cmd("LLEN")
                .arg(self.kb.key("label"))
                .query_async(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;

            Ok(n)
        }
    }

    // ── Tests ──────────────────────────────────────────────────────────

    #[cfg(test)]
    mod tests {
        use std::sync::atomic::{AtomicU16, Ordering};

        use super::*;
        use crate::storage::Storage;

        static COUNTER: AtomicU16 = AtomicU16::new(0);

        /// Tạo RedisStorage mới với prefix unique (cần tokio runtime).
        /// Dùng PID + counter để tránh collision với stale data từ test run cũ.
        async fn new_test_storage() -> RedisStorage {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let client = redis::Client::open("redis://127.0.0.1:6379/15")
                .expect("redis connection failed — is redis-server running?");
            RedisStorage::new(client, &format!("test:merged:{}:{n}", pid))
                .await
                .expect("init failed")
        }

        // ── Radix-style tests ──

        #[tokio::test]
        async fn test_new_node_and_get_node() {
            let mut s = new_test_storage().await;
            let id = s.new_node(b"hello".to_vec(), 42).await.unwrap();
            assert_ne!(id, 0, "id should not be the sentinel");

            let (prefix, record) = s.get_node(id).await.unwrap();
            assert_eq!(prefix, b"hello");
            assert_eq!(record, 42);
        }

        #[tokio::test]
        async fn test_update_node() {
            let mut s = new_test_storage().await;
            let id = s.new_node(b"init".to_vec(), 1).await.unwrap();

            s.update_node(id, Some(b"updated".to_vec()), Some(99))
                .await
                .unwrap();

            let (prefix, record) = s.get_node(id).await.unwrap();
            assert_eq!(prefix, b"updated");
            assert_eq!(record, 99);
        }

        #[tokio::test]
        async fn test_add_child_and_get_children() {
            let mut s = new_test_storage().await;
            let parent = s.new_node(b"parent".to_vec(), 0).await.unwrap();
            let child1 = s.new_node(b"child1".to_vec(), 1).await.unwrap();
            let child2 = s.new_node(b"child2".to_vec(), 2).await.unwrap();

            s.add_child(parent, child1).await.unwrap();
            s.add_child(parent, child2).await.unwrap();

            let children = s.get_children(parent).await.unwrap();
            // Set → không đảm bảo thứ tự, chỉ kiểm tra nội dung
            assert_eq!(children.len(), 2);
            assert!(children.contains(&child1));
            assert!(children.contains(&child2));
        }

        #[tokio::test]
        async fn test_remove_child() {
            let mut s = new_test_storage().await;
            let parent = s.new_node(b"parent".to_vec(), 0).await.unwrap();
            let child1 = s.new_node(b"child1".to_vec(), 1).await.unwrap();
            let child2 = s.new_node(b"child2".to_vec(), 2).await.unwrap();
            let child3 = s.new_node(b"child3".to_vec(), 3).await.unwrap();

            s.add_child(parent, child1).await.unwrap();
            s.add_child(parent, child2).await.unwrap();
            s.add_child(parent, child3).await.unwrap();

            let children = s.get_children(parent).await.unwrap();
            assert_eq!(children.len(), 3);

            // Xoá child2
            s.remove_child(parent, child2).await.unwrap();
            let children = s.get_children(parent).await.unwrap();
            assert_eq!(children.len(), 2);
            assert!(children.contains(&child1));
            assert!(children.contains(&child3));
            assert!(!children.contains(&child2));

            // Xoá không tồn tại → không lỗi
            s.remove_child(parent, 999).await.unwrap();
            let children = s.get_children(parent).await.unwrap();
            assert_eq!(children.len(), 2);
        }

        #[tokio::test]
        async fn test_root() {
            let mut s = new_test_storage().await;

            assert_eq!(s.get_root(3).await.unwrap(), 0, "fresh shard returns 0");

            s.set_root(3, 42).await.unwrap();
            assert_eq!(s.get_root(3).await.unwrap(), 42);

            s.set_root(3, 99).await.unwrap();
            assert_eq!(s.get_root(3).await.unwrap(), 99);
        }

        #[tokio::test]
        async fn test_consecutive_ids() {
            let mut s = new_test_storage().await;
            let a = s.new_node(b"a".to_vec(), 10).await.unwrap();
            let b = s.new_node(b"b".to_vec(), 20).await.unwrap();
            let c = s.new_node(b"c".to_vec(), 30).await.unwrap();

            assert_eq!(a, 1);
            assert_eq!(b, 2);
            assert_eq!(c, 3);
        }

        // ── Automaton-style tests ──

        #[tokio::test]
        async fn test_add_state() {
            let mut s = new_test_storage().await;
            let id = s.add_state("a").await.unwrap();
            assert_eq!(id, 1, "first real state gets ID 1");
            assert_eq!(s.num_states().await.unwrap(), 2);
        }

        #[tokio::test]
        async fn test_label() {
            let mut s = new_test_storage().await;
            let id = s.add_state("hello").await.unwrap();
            assert_eq!(s.get_label(id).await.unwrap(), "hello");
            assert_eq!(s.get_label(0).await.unwrap(), "");
        }

        #[tokio::test]
        async fn test_transitions() {
            let mut s = new_test_storage().await;
            let s1 = s.add_state("a").await.unwrap();
            let s2 = s.add_state("b").await.unwrap();
            s.set_transition(0, "x", s1).await.unwrap();
            s.set_transition(s1, "y", s2).await.unwrap();

            let t0 = s.get_transitions(0).await.unwrap();
            assert!(t0.contains(&("x".into(), s1)));

            let t1 = s.get_transitions(s1).await.unwrap();
            assert!(t1.contains(&("y".into(), s2)));
        }

        #[tokio::test]
        async fn test_failure() {
            let mut s = new_test_storage().await;
            let id = s.add_state("test").await.unwrap();
            assert_eq!(s.get_failure(id).await.unwrap(), 0);
            s.set_failure(id, 42).await.unwrap();
            assert_eq!(s.get_failure(id).await.unwrap(), 42);
        }

        #[tokio::test]
        async fn test_output() {
            let mut s = new_test_storage().await;
            let id = s.add_state("term").await.unwrap();
            assert_eq!(s.get_output(id).await.unwrap(), None);
            s.set_output(id, 7).await.unwrap();
            assert_eq!(s.get_output(id).await.unwrap(), Some(7));
        }

        #[tokio::test]
        async fn test_root_inputs() {
            let mut s = new_test_storage().await;
            let s1 = s.add_state("s1").await.unwrap();
            let s2 = s.add_state("s2").await.unwrap();
            s.add_root_input(s1).await.unwrap();
            s.add_root_input(s2).await.unwrap();

            let inputs = s.get_root_inputs().await.unwrap();
            assert_eq!(inputs, vec![s1, s2]);
        }
    }
}
