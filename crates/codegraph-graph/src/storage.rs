//! Radix-node storage — the only persistence surface for the radix tree.
//!
//! Storage chỉ lưu các node của radix tree: prefix + record + children + root
//! của từng shard. Mọi thao tác thay đổi cấu trúc cây đi qua một **transaction**
//! (`Tx`) để áp dụng atomic — không có trạng thái trung gian lộ ra cho reader.
//!
//! Các khái niệm cũ (automaton, entries, blob, shard-compressed) đã bị xoá
//! trong đợt refactor — nếu cần persistence tầng cao hơn thì phải làm ở tầng
//! khác, không phải ở đây.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use codegraph_core::{FileInfo, Symbol};

#[cfg(feature = "sqlite")]
pub mod sqlite;

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

/// Node id 0 là sentinel (rỗng) — dùng để đánh dấu "không có" trong radix.
pub const EMPTY: usize = 0;

/// Encode chain thành bytes (u64 little-endian, 8 byte/element) — format của
/// chain stream. Chain = chuỗi element id (marker + symbol) của một hàm.
pub(crate) fn encode_chain(chain: &[u64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(chain.len() * 8);
    for e in chain {
        out.extend_from_slice(&e.to_le_bytes());
    }
    out
}

/// Decode bytes trong chain stream về `Vec<u64>` element ids.
#[allow(dead_code)] // chỉ dùng qua get_chain (test/sqlite builds)
pub(crate) fn decode_chain(bytes: &[u8]) -> Vec<u64> {
    bytes
        .chunks_exact(8)
        .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

// ==================== Transaction ====================

/// Một mutation lẻ trong transaction.
#[derive(Clone, Debug)]
enum TxOp {
    AddChild {
        parent: usize,
        child: usize,
    },
    MoveChild {
        from: usize,
        to: usize,
        child: usize,
    },
    UpdateNode {
        id: usize,
        prefix: Option<Vec<u8>>,
        record: Option<usize>,
    },
}

/// Transaction — buffer toàn bộ mutation và áp dụng atomic tại `commit`.
///
/// `new_node` reserve id **ngay lập tức** (từ counter của storage) để caller
/// (radix split) có thể dùng id làm tham chiếu trước khi commit; nhưng node
/// chưa lộ ra cho reader cho tới khi `commit` hoàn tất.
///
/// `commit(self: Box<Self>)` tiêu thụ chính transaction — không thể commit 2 lần.
#[async_trait]
pub trait Tx: Send {
    async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize>;
    async fn update_node(
        &mut self,
        id: usize,
        prefix: Option<Vec<u8>>,
        record: Option<usize>,
    ) -> Result<()>;
    async fn add_child(&mut self, parent: usize, child: usize) -> Result<()>;
    async fn move_child(&mut self, from: usize, to: usize, child: usize) -> Result<()>;
    async fn commit(self: Box<Self>) -> Result<()>;
}

// ==================== Storage trait ====================

/// Radix-node storage: node management + transaction.
#[async_trait]
pub trait Storage: Send + Sync {
    // ── Node management ──
    async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize>;
    async fn update_node(
        &mut self,
        id: usize,
        prefix: Option<Vec<u8>>,
        record: Option<usize>,
    ) -> Result<()>;
    async fn get_node(&self, id: usize) -> Result<(Vec<u8>, usize)>;
    async fn get_children(&self, id: usize) -> Result<Vec<usize>>;

    // ── Edge data stream (metadata per edge id — chain model không còn link-edge) ──
    /// Lưu dữ liệu edge (opaque bytes, VD CallEdgeMeta JSON) keyed theo edge id.
    /// Mặc định: no-op.
    #[allow(dead_code)] // API giữ nguyên (protected) — edges suy từ chain trong GraphIndex.
    async fn set_edge_data(&mut self, edge: usize, data: &[u8]) -> Result<()> {
        let _ = (edge, data);
        Ok(())
    }
    /// Đọc dữ liệu edge — `None` nếu edge chưa có. Mặc định: `None`.
    #[allow(dead_code)] // API giữ nguyên (protected).
    async fn get_edge_data(&self, edge: usize) -> Result<Option<Vec<u8>>> {
        let _ = edge;
        Ok(None)
    }
    /// Xoá toàn bộ edge stream (dùng khi rebuild index). Mặc định: no-op.
    async fn clear_edges(&mut self) -> Result<()> {
        Ok(())
    }
    /// Duyệt toàn bộ edge data `(edge_id, meta)` theo thứ tự bất kỳ — dùng để
    /// rebuild edge registry khi reopen (CallEdgeMeta chứa from/to). Mặc định:
    /// không có edge nào.
    #[allow(dead_code)] // dùng qua Search::for_each_edge_data (sqlite builds)
    async fn for_each_edge_data(
        &self,
        f: &mut (dyn for<'a> FnMut(usize, &'a [u8]) -> Result<()> + Send),
    ) -> Result<()> {
        let _ = f;
        Ok(())
    }

    // ── Node metadata stream (Node JSON — migrate từ Db xuống index) ──
    /// Lưu metadata của node (opaque bytes, VD Node JSON) keyed theo element id
    /// (`SYMBOL_BASE + db_node_id`). Mặc định: no-op.
    async fn set_node_meta(&mut self, elem: usize, meta: &[u8]) -> Result<()> {
        let _ = (elem, meta);
        Ok(())
    }
    /// Đọc node metadata — `None` nếu node chưa có. Mặc định: `None`.
    #[allow(dead_code)] // API giữ nguyên (protected) — GraphIndex dùng metas=None.
    async fn get_node_meta(&self, elem: usize) -> Result<Option<Vec<u8>>> {
        let _ = elem;
        Ok(None)
    }
    /// Xoá toàn bộ node stream (dùng khi rebuild index). Mặc định: no-op.
    async fn clear_node_meta(&mut self) -> Result<()> {
        Ok(())
    }

    // ── Chain stream (per-owner chain — marker + symbol element ids) ──
    /// Lưu chain của owner (keyed theo record của owner; u64 LE 8-byte/element).
    /// Mặc định: no-op.
    async fn set_chain(&mut self, record: usize, chain: &[u64]) -> Result<()> {
        let _ = (record, chain);
        Ok(())
    }
    /// Đọc chain của owner — `None` nếu owner chưa có chain. Mặc định: `None`.
    #[allow(dead_code)] // dùng qua Search::get_chain (test/sqlite builds)
    async fn get_chain(&self, record: usize) -> Result<Option<Vec<u64>>> {
        let _ = record;
        Ok(None)
    }
    /// Xoá toàn bộ chains (dùng khi rebuild index). Mặc định: no-op.
    async fn clear_chains(&mut self) -> Result<()> {
        Ok(())
    }

    // ── Shard roots (endpoint) ──
    async fn set_root(&mut self, shard: usize, root: usize) -> Result<()>;
    async fn get_root(&self, shard: usize) -> Result<usize>;

    // ── Metadata & key length ──
    /// Lưu metadata (opaque bytes, VD: call-site info) cho một record.
    /// Nằm tách khỏi radix node — keyed theo record index.
    #[allow(dead_code)] // primitive storage — dùng trong storage tests
    async fn set_meta(&mut self, record: usize, meta: &[u8]) -> Result<()>;
    /// Đọc metadata của record — `None` nếu record chưa có meta.
    async fn get_meta(&self, record: usize) -> Result<Option<Vec<u8>>>;
    /// Lưu độ dài key (số element) của record — dùng filter `depth` khi search.
    async fn set_key_len(&mut self, record: usize, len: usize) -> Result<()>;
    /// Đọc độ dài key của record — `None` nếu record chưa insert.
    async fn get_key_len(&self, record: usize) -> Result<Option<usize>>;

    // ── Shortcuts (auxiliary LIKE-search index) ──
    /// Thêm `node_id` vào shortcut set của element `elem` (encoded bytes).
    /// Shortcut set = mọi node có chứa element này trong prefix của nó — dùng
    /// làm candidate khi tìm substring (KMP + DFS).
    async fn add_shortcut_node(&mut self, shard: usize, elem: &[u8], node_id: usize) -> Result<()>;
    /// Lấy toàn bộ node id chứa element `elem` trong shard.
    async fn get_shortcut_nodes(&self, shard: usize, elem: &[u8]) -> Result<Vec<usize>>;
    /// Xoá toàn bộ shortcut sets (dùng khi rebuild index từ tree).
    async fn clear_shortcuts(&mut self) -> Result<()>;

    // ── Entity store (semgraph model — symbols/chains/callnames/files/version) ──
    // Tầng dữ liệu ngữ nghĩa đã dời xuống storage (db/ cũ bị xoá): mọi backend
    // giữ entity data riêng (InMemory = HashMap, Sqlite = bảng `sg_*`, Redis =
    // hash). Mặc định no-op để backend không cần implement nếu chưa dùng.

    // Method được GraphIndex gọi trực tiếp (ingest/register/flow) — live ở mọi
    // build. Method chỉ dùng qua `rebuild()` (mở lại file — feature `sqlite`)
    // cfg_attr allow cho build không feature đó; `load_symbol`/`load_call_name_index`
    // chưa có caller — giữ allow cho tới khi consumer cần.
    /// Lưu một symbol — mặc định: no-op.
    async fn save_symbol(&mut self, _sym: &Symbol) -> Result<()> {
        Ok(())
    }
    #[allow(dead_code)]
    /// Đọc symbol theo id — mặc định: `None`.
    async fn load_symbol(&self, _id: u64) -> Result<Option<Symbol>> {
        Ok(None)
    }
    /// Đọc toàn bộ symbol (rebuild index khi open) — mặc định: rỗng.
    #[cfg_attr(not(feature = "sqlite"), allow(dead_code))]
    async fn load_all_symbols(&self) -> Result<Vec<Symbol>> {
        Ok(Vec::new())
    }
    /// Lưu `next_id` của symbol registry — mặc định: no-op.
    async fn save_next_id(&mut self, _next: u64) -> Result<()> {
        Ok(())
    }
    /// Đọc `next_id` — mặc định: 0 (chưa có symbol).
    #[cfg_attr(not(feature = "sqlite"), allow(dead_code))]
    async fn load_next_id(&self) -> Result<u64> {
        Ok(0)
    }
    /// Đọc toàn bộ chain `(func_id, chain_bytes u64 LE)` — rebuild engine khi
    /// open — mặc định: rỗng.
    #[cfg_attr(not(feature = "sqlite"), allow(dead_code))]
    async fn all_chains(&self) -> Result<Vec<(u64, Vec<u8>)>> {
        Ok(Vec::new())
    }
    /// Lưu call records của một func (opaque bytes, JSON) — mặc định: no-op.
    async fn set_call_records(&mut self, _func: u64, _records: &[u8]) -> Result<()> {
        Ok(())
    }
    /// Đọc call records của func — mặc định: `None`.
    async fn get_call_records(&self, _func: u64) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    /// Toàn bộ call records `(func_id, bytes)` — mặc định: rỗng.
    #[cfg_attr(not(feature = "sqlite"), allow(dead_code))]
    async fn all_call_records(&self) -> Result<Vec<(u64, Vec<u8>)>> {
        Ok(Vec::new())
    }
    /// Lưu inverted index `call name → call sites` (opaque bytes, JSON) — mặc
    /// định: no-op.
    async fn set_call_name_index(&mut self, _name: &str, _sites: &[u8]) -> Result<()> {
        Ok(())
    }
    #[allow(dead_code)]
    /// Đọc call-name index — mặc định: `None`.
    async fn load_call_name_index(&self, _name: &str) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    /// Toàn bộ call-name index `(name, bytes)` — mặc định: rỗng.
    #[cfg_attr(not(feature = "sqlite"), allow(dead_code))]
    async fn all_call_name_indexes(&self) -> Result<Vec<(String, Vec<u8>)>> {
        Ok(Vec::new())
    }
    /// Upsert file info — mặc định: no-op.
    async fn upsert_file(&mut self, _f: &FileInfo) -> Result<()> {
        Ok(())
    }
    /// Toàn bộ files — mặc định: rỗng.
    #[cfg_attr(not(feature = "sqlite"), allow(dead_code))]
    async fn load_all_files(&self) -> Result<Vec<FileInfo>> {
        Ok(Vec::new())
    }
    /// Version của index (`index_version` — bump mỗi lần ingest) — mặc định: 0.
    #[cfg_attr(not(feature = "sqlite"), allow(dead_code))]
    async fn version(&self) -> Result<u64> {
        Ok(0)
    }
    /// Lưu version — mặc định: no-op.
    async fn set_version(&mut self, _v: u64) -> Result<()> {
        Ok(())
    }
    /// Xoá toàn bộ entity data (symbols/next_id/call_records/call_names/files/
    /// version) — dùng khi full re-index. Mặc định: no-op.
    async fn clear_entities(&mut self) -> Result<()> {
        Ok(())
    }

    // ── Transaction ──
    /// Bắt đầu một transaction (sync, không await — đúng theo cách radix gọi).
    /// Buffer ops; mọi thay đổi chỉ lộ ra khi `commit`.
    fn new_tx(&self) -> Box<dyn Tx>;
}

// ==================== In-Memory Storage ====================

struct MemoryData {
    /// (prefix, record) — index 0 là sentinel.
    nodes: Vec<(Vec<u8>, usize)>,
    /// children list per node (index 0 = sentinel).
    children: Vec<Vec<usize>>,
    /// root id per shard.
    roots: Vec<usize>,
    /// record_idx → metadata (opaque bytes, VD: call-site info).
    meta: HashMap<usize, Vec<u8>>,
    /// record_idx → độ dài key (số element) — dùng filter `depth` khi search.
    key_lens: HashMap<usize, usize>,
    /// shortcuts[shard][elem_bytes] = node ids chứa elem trong prefix.
    shortcuts: Vec<HashMap<Vec<u8>, HashSet<usize>>>,
    /// edge id → dữ liệu edge (opaque bytes, VD EdgeMeta JSON).
    edges: HashMap<usize, Vec<u8>>,
    /// element id → node metadata (Node JSON).
    node_meta: HashMap<usize, Vec<u8>>,
    /// record (owner) → chain bytes (u64 LE 8-byte/element).
    chains: HashMap<usize, Vec<u8>>,
    // ── Entity store (semgraph model) ──
    // Ghi/đọc bởi entity methods qua InMemoryStorage (GraphIndex ingest/rebuild).
    /// symbol id → Symbol.
    symbols: HashMap<u64, Symbol>,
    /// next_id của symbol registry.
    next_id: u64,
    /// func id → call records (JSON).
    call_records: HashMap<u64, Vec<u8>>,
    /// call name → call sites (JSON).
    call_names: HashMap<String, Vec<u8>>,
    /// path → FileInfo.
    files: HashMap<String, FileInfo>,
    /// index version.
    version: u64,
}

/// In-memory radix storage. Thread-safe: toàn bộ state nằm sau 1 RwLock;
/// id được cấp bằng AtomicUsize nên các transaction song song không trùng id.
pub struct InMemoryStorage {
    data: Arc<RwLock<MemoryData>>,
    next_id: Arc<AtomicUsize>,
}

impl InMemoryStorage {
    pub fn new() -> Self {
        Self {
            data: Arc::new(RwLock::new(MemoryData {
                nodes: vec![(vec![], EMPTY)], // sentinel
                children: vec![vec![]],
                roots: vec![],
                meta: HashMap::new(),
                key_lens: HashMap::new(),
                shortcuts: vec![],
                edges: HashMap::new(),
                node_meta: HashMap::new(),
                chains: HashMap::new(),
                symbols: HashMap::new(),
                // Id bắt đầu từ SYMBOL_BASE (marker reserved 1..=99).
                next_id: codegraph_core::SYMBOL_BASE,
                call_records: HashMap::new(),
                call_names: HashMap::new(),
                files: HashMap::new(),
                version: 0,
            })),
            next_id: Arc::new(AtomicUsize::new(1)),
        }
    }
}

impl Default for InMemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryStorage {
    /// Reserve một id mới (dùng chung cho cả new_node trực tiếp lẫn tx).
    fn alloc_id(&self) -> usize {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }
}

#[async_trait]
impl Storage for InMemoryStorage {
    async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize> {
        let id = self.alloc_id();
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        if d.nodes.len() <= id {
            d.nodes.resize(id + 1, (vec![], EMPTY));
            d.children.resize(id + 1, vec![]);
        }
        d.nodes[id] = (prefix, record);
        Ok(id)
    }

    async fn update_node(
        &mut self,
        id: usize,
        prefix: Option<Vec<u8>>,
        record: Option<usize>,
    ) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        if id >= d.nodes.len() {
            return Err(StorageError::BranchOutOfRange(id));
        }
        if let Some(p) = prefix {
            d.nodes[id].0 = p;
        }
        if let Some(r) = record {
            d.nodes[id].1 = r;
        }
        Ok(())
    }

    async fn get_node(&self, id: usize) -> Result<(Vec<u8>, usize)> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        if id >= d.nodes.len() {
            return Err(StorageError::BranchOutOfRange(id));
        }
        Ok(d.nodes[id].clone())
    }

    async fn get_children(&self, id: usize) -> Result<Vec<usize>> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        Ok(d.children.get(id).cloned().unwrap_or_default())
    }

    async fn set_root(&mut self, shard: usize, root: usize) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        if shard >= d.roots.len() {
            d.roots.resize(shard + 1, EMPTY);
        }
        d.roots[shard] = root;
        Ok(())
    }

    async fn get_root(&self, shard: usize) -> Result<usize> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        Ok(d.roots.get(shard).copied().unwrap_or(EMPTY))
    }

    async fn set_meta(&mut self, record: usize, meta: &[u8]) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        d.meta.insert(record, meta.to_vec());
        Ok(())
    }

    async fn get_meta(&self, record: usize) -> Result<Option<Vec<u8>>> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        Ok(d.meta.get(&record).cloned())
    }

    async fn set_key_len(&mut self, record: usize, len: usize) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        d.key_lens.insert(record, len);
        Ok(())
    }

    async fn get_key_len(&self, record: usize) -> Result<Option<usize>> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        Ok(d.key_lens.get(&record).copied())
    }

    async fn add_shortcut_node(&mut self, shard: usize, elem: &[u8], node_id: usize) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        if shard >= d.shortcuts.len() {
            d.shortcuts.resize(shard + 1, HashMap::new());
        }
        d.shortcuts[shard]
            .entry(elem.to_vec())
            .or_default()
            .insert(node_id);
        Ok(())
    }

    async fn get_shortcut_nodes(&self, shard: usize, elem: &[u8]) -> Result<Vec<usize>> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        Ok(d.shortcuts
            .get(shard)
            .and_then(|m| m.get(elem))
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default())
    }

    async fn clear_shortcuts(&mut self) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        for map in d.shortcuts.iter_mut() {
            map.clear();
        }
        Ok(())
    }

    async fn set_edge_data(&mut self, edge: usize, data: &[u8]) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        d.edges.insert(edge, data.to_vec());
        Ok(())
    }

    async fn get_edge_data(&self, edge: usize) -> Result<Option<Vec<u8>>> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        Ok(d.edges.get(&edge).cloned())
    }

    async fn clear_edges(&mut self) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        d.edges.clear();
        Ok(())
    }

    async fn for_each_edge_data(
        &self,
        f: &mut (dyn for<'a> FnMut(usize, &'a [u8]) -> Result<()> + Send),
    ) -> Result<()> {
        let items: Vec<(usize, Vec<u8>)> = {
            let d = self
                .data
                .read()
                .map_err(|_| StorageError::Internal("poison".into()))?;
            d.edges.iter().map(|(&id, data)| (id, data.clone())).collect()
        };
        for (id, data) in items {
            f(id, &data)?;
        }
        Ok(())
    }

    async fn set_node_meta(&mut self, elem: usize, meta: &[u8]) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        d.node_meta.insert(elem, meta.to_vec());
        Ok(())
    }

    async fn get_node_meta(&self, elem: usize) -> Result<Option<Vec<u8>>> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        Ok(d.node_meta.get(&elem).cloned())
    }

    async fn clear_node_meta(&mut self) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        d.node_meta.clear();
        Ok(())
    }

    async fn set_chain(&mut self, record: usize, chain: &[u64]) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        d.chains.insert(record, encode_chain(chain));
        Ok(())
    }

    async fn get_chain(&self, record: usize) -> Result<Option<Vec<u64>>> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        Ok(d.chains.get(&record).map(|b| decode_chain(b)))
    }

    async fn clear_chains(&mut self) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        d.chains.clear();
        Ok(())
    }

    async fn save_symbol(&mut self, sym: &Symbol) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        d.symbols.insert(sym.id, sym.clone());
        Ok(())
    }

    async fn load_symbol(&self, id: u64) -> Result<Option<Symbol>> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        Ok(d.symbols.get(&id).cloned())
    }

    async fn load_all_symbols(&self) -> Result<Vec<Symbol>> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        let mut out: Vec<Symbol> = d.symbols.values().cloned().collect();
        out.sort_by_key(|s| s.id);
        Ok(out)
    }

    async fn save_next_id(&mut self, next: u64) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        d.next_id = next;
        Ok(())
    }

    async fn load_next_id(&self) -> Result<u64> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        Ok(d.next_id)
    }

    async fn all_chains(&self) -> Result<Vec<(u64, Vec<u8>)>> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        let mut out: Vec<(u64, Vec<u8>)> = d
            .chains
            .iter()
            .map(|(&rec, bytes)| (rec as u64, bytes.clone()))
            .collect();
        out.sort_by_key(|(rec, _)| *rec);
        Ok(out)
    }

    async fn set_call_records(&mut self, func: u64, records: &[u8]) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        d.call_records.insert(func, records.to_vec());
        Ok(())
    }

    async fn get_call_records(&self, func: u64) -> Result<Option<Vec<u8>>> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        Ok(d.call_records.get(&func).cloned())
    }

    async fn all_call_records(&self) -> Result<Vec<(u64, Vec<u8>)>> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        Ok(d.call_records.iter().map(|(&f, b)| (f, b.clone())).collect())
    }

    async fn set_call_name_index(&mut self, name: &str, sites: &[u8]) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        d.call_names.insert(name.to_string(), sites.to_vec());
        Ok(())
    }

    async fn load_call_name_index(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        Ok(d.call_names.get(name).cloned())
    }

    async fn all_call_name_indexes(&self) -> Result<Vec<(String, Vec<u8>)>> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        Ok(d.call_names.iter().map(|(n, b)| (n.clone(), b.clone())).collect())
    }

    async fn upsert_file(&mut self, f: &FileInfo) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        d.files.insert(f.path.clone(), f.clone());
        Ok(())
    }

    async fn load_all_files(&self) -> Result<Vec<FileInfo>> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        let mut out: Vec<FileInfo> = d.files.values().cloned().collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    async fn version(&self) -> Result<u64> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        Ok(d.version)
    }

    async fn set_version(&mut self, v: u64) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        d.version = v;
        Ok(())
    }

    async fn clear_entities(&mut self) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        d.symbols.clear();
        d.next_id = codegraph_core::SYMBOL_BASE;
        d.call_records.clear();
        d.call_names.clear();
        d.files.clear();
        d.version = 0;
        Ok(())
    }

    fn new_tx(&self) -> Box<dyn Tx> {
        Box::new(InMemoryTx {
            data: self.data.clone(),
            next_id: self.next_id.clone(),
            nodes: Vec::new(),
            ops: Vec::new(),
        })
    }
}

/// Transaction cho `InMemoryStorage`: buffer toàn bộ mutation, áp dụng
/// atomic dưới 1 write lock tại `commit`.
struct InMemoryTx {
    data: Arc<RwLock<MemoryData>>,
    next_id: Arc<AtomicUsize>,
    /// (reserved_id, prefix, record) — được append tại commit.
    nodes: Vec<(usize, Vec<u8>, usize)>,
    ops: Vec<TxOp>,
}

#[async_trait]
impl Tx for InMemoryTx {
    async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
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
        let InMemoryTx {
            data, nodes, ops, ..
        } = *self;

        let mut d = data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;

        // 1. Materialize các node đã reserve (đảm bảo children[leg] tồn tại
        //    trước khi ops move/add trỏ tới).
        for (id, prefix, record) in nodes {
            if d.nodes.len() <= id {
                d.nodes.resize(id + 1, (vec![], EMPTY));
                d.children.resize(id + 1, vec![]);
            }
            d.nodes[id] = (prefix, record);
        }

        // 2. Áp dụng toàn bộ ops — tất cả cùng thành công hoặc cùng thất bại
        //    (single write lock → không lộ trạng thái trung gian).
        for op in ops {
            match op {
                TxOp::AddChild { parent, child } => {
                    if parent < d.children.len() && !d.children[parent].contains(&child) {
                        d.children[parent].push(child);
                    }
                }
                TxOp::MoveChild { from, to, child } => {
                    if from < d.children.len() {
                        d.children[from].retain(|&c| c != child);
                    }
                    if to < d.children.len() && !d.children[to].contains(&child) {
                        d.children[to].push(child);
                    }
                }
                TxOp::UpdateNode { id, prefix, record } => {
                    if id < d.nodes.len() {
                        if let Some(p) = prefix {
                            d.nodes[id].0 = p;
                        }
                        if let Some(r) = record {
                            d.nodes[id].1 = r;
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

// =========================================================================
//  Redis Storage — chỉ build khi feature "redis" được bật.
// =========================================================================

#[cfg(feature = "redis")]
#[allow(dead_code)] // backend redis chỉ được exercise bởi tests của chính nó (chưa có production path)
pub mod redis {
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

    use super::{FileInfo, Result, Storage, StorageError, Symbol, Tx, TxOp};

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

        pub async fn from_multiplexed(conn: MultiplexedConnection, prefix: &str) -> Result<Self> {
            let s = Self {
                conn: Arc::new(Mutex::new(conn)),
                kb: KeyBuilder::new(prefix),
            };
            s.init().await?;
            Ok(s)
        }

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

        async fn add_shortcut_node(
            &mut self,
            shard: usize,
            elem: &[u8],
            node_id: usize,
        ) -> Result<()> {
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
            data.map(|d| {
                serde_json::from_slice(&d).map_err(|e| StorageError::Internal(e.to_string()))
            })
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
                    serde_json::from_slice(&data)
                        .map_err(|e| StorageError::Internal(e.to_string()))?,
                );
            }
            out.sort_by_key(|s| s.id);
            Ok(out)
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
            Ok(next.map(|n| n as u64).unwrap_or(codegraph_core::SYMBOL_BASE))
        }

        async fn all_chains(&self) -> Result<Vec<(u64, Vec<u8>)>> {
            let mut conn = self.lock().await;
            let map: HashMap<i64, Vec<u8>> = cmd("HGETALL")
                .arg(self.kb.key("chains"))
                .query_async(&mut *conn)
                .await
                .map_err(|e: redis::RedisError| StorageError::Internal(e.to_string()))?;
            let mut out: Vec<(u64, Vec<u8>)> = map
                .into_iter()
                .map(|(r, b)| (r as u64, b))
                .collect();
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
            let mut out: Vec<(u64, Vec<u8>)> = map
                .into_iter()
                .map(|(f, b)| (f as u64, b))
                .collect();
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
                    serde_json::from_slice(&data)
                        .map_err(|e| StorageError::Internal(e.to_string()))?,
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
}

// ==================== Tests (InMemory) ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_new_node_and_get_node() {
        let mut s = InMemoryStorage::default();
        let id = s.new_node(b"hello".to_vec(), 42).await.unwrap();
        assert_ne!(id, EMPTY);
        let (prefix, record) = s.get_node(id).await.unwrap();
        assert_eq!(prefix, b"hello");
        assert_eq!(record, 42);
    }

    #[tokio::test]
    async fn test_update_node() {
        let mut s = InMemoryStorage::default();
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
        let mut s = InMemoryStorage::default();
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
        let mut s = InMemoryStorage::default();
        // Chưa có gì → None.
        assert_eq!(s.get_meta(7).await.unwrap(), None);
        assert_eq!(s.get_key_len(7).await.unwrap(), None);
        s.set_meta(7, b"call-site-info".as_slice()).await.unwrap();
        s.set_key_len(7, 5).await.unwrap();
        assert_eq!(
            s.get_meta(7).await.unwrap().as_deref(),
            Some(b"call-site-info".as_slice())
        );
        assert_eq!(s.get_key_len(7).await.unwrap(), Some(5));
        // Ghi đè meta.
        s.set_meta(7, b"updated").await.unwrap();
        s.set_key_len(7, 6).await.unwrap();
        assert_eq!(
            s.get_meta(7).await.unwrap().as_deref(),
            Some(b"updated".as_slice())
        );
        assert_eq!(s.get_key_len(7).await.unwrap(), Some(6));
        // Record khác không ảnh hưởng.
        assert_eq!(s.get_meta(8).await.unwrap(), None);
        assert_eq!(s.get_key_len(8).await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_shortcuts_roundtrip() {
        let mut s = InMemoryStorage::default();
        // Chưa có gì → empty.
        assert!(s.get_shortcut_nodes(1, b"l").await.unwrap().is_empty());
        s.add_shortcut_node(1, b"l", 10).await.unwrap();
        s.add_shortcut_node(1, b"l", 20).await.unwrap();
        s.add_shortcut_node(1, b"o", 10).await.unwrap();
        s.add_shortcut_node(2, b"l", 30).await.unwrap(); // shard khác
        let nodes = s.get_shortcut_nodes(1, b"l").await.unwrap();
        assert!(nodes.contains(&10) && nodes.contains(&20));
        assert_eq!(nodes.len(), 2);
        assert_eq!(s.get_shortcut_nodes(2, b"l").await.unwrap(), vec![30]);

        // Clear → rỗng hết.
        s.clear_shortcuts().await.unwrap();
        assert!(s.get_shortcut_nodes(1, b"l").await.unwrap().is_empty());
        assert!(s.get_shortcut_nodes(2, b"l").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_tx_commit_applies_atomically() {
        let mut s = InMemoryStorage::default();
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
        let s = InMemoryStorage::default();
        let mut tx = s.new_tx();
        let id = tx.new_node(b"pending".to_vec(), 9).await.unwrap();
        // Trước commit, node chưa materialize → get_node lỗi BranchOutOfRange.
        assert!(s.get_node(id).await.is_err());
        tx.commit().await.unwrap();
        assert_eq!(s.get_node(id).await.unwrap().1, 9);
    }

    #[tokio::test]
    async fn test_tx_move_child_migrates() {
        let mut s = InMemoryStorage::default();
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
        let mut s = InMemoryStorage::default();
        // Chưa có edge → None.
        assert_eq!(s.get_edge_data(7).await.unwrap(), None);
        s.set_edge_data(7, b"call-site").await.unwrap();
        assert_eq!(
            s.get_edge_data(7).await.unwrap().as_deref(),
            Some(b"call-site".as_slice())
        );
        // Ghi đè dữ liệu edge.
        s.set_edge_data(7, b"updated").await.unwrap();
        assert_eq!(
            s.get_edge_data(7).await.unwrap().as_deref(),
            Some(b"updated".as_slice())
        );
        // Edge khác không ảnh hưởng.
        assert_eq!(s.get_edge_data(8).await.unwrap(), None);

        // Clear → sạch toàn bộ.
        s.set_edge_data(9, b"x").await.unwrap();
        s.clear_edges().await.unwrap();
        assert_eq!(s.get_edge_data(7).await.unwrap(), None);
        assert_eq!(s.get_edge_data(9).await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_node_meta_roundtrip() {
        let mut s = InMemoryStorage::default();
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
        let mut s = InMemoryStorage::default();
        assert_eq!(s.get_chain(9).await.unwrap(), None);
        s.set_chain(9, &[1, 2, 3]).await.unwrap();
        assert_eq!(s.get_chain(9).await.unwrap(), Some(vec![1, 2, 3]));
        s.set_chain(9, &[4]).await.unwrap();
        assert_eq!(s.get_chain(9).await.unwrap(), Some(vec![4]));
        assert_eq!(s.get_chain(10).await.unwrap(), None);
        s.clear_chains().await.unwrap();
        assert_eq!(s.get_chain(9).await.unwrap(), None);
    }
}
