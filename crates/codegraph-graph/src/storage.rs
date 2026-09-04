//! Storage layer cho `codegraph-graph`.
//!
//! Tách làm 2 phần rõ ràng (thay cho `Storage` cũ gồm ~40 method trộn lẫn):
//!
//! - **Radix-node storage** — `CategoryStorage` + 5 trait phụ
//!   (`NodeMetaStorage` / `ShortcutsStorage` / `EdgeDataStorage` /
//!   `ChainStorage` / `BloomStorage`). Phần này dùng bởi `Radix` + `Search`
//!   để duy trì cây radix + stream kèm theo. Bắt nguồn từ `opsense-libs`.
//!
//! - **Entity store** — `EntityStorage` trait (mới, chỉ có trong
//!   `codegraph-graph`). Lưu symbols/files/embeddings/version/stats/call
//!   records/call-name index. Chỉ `GraphIndex` / `SharedGraphIndex` dùng.
//!
//! - **`Storage` umbrella** — gộp 2 phần trên (cho `Arc<RwLock<dyn Storage>>`
//!   trong `GraphIndex`). Backend implement 7 `impl` block riêng (1 cho
//!   `CategoryStorage`, 5 cho trait phụ, 1 cho `EntityStorage`, 1 marker rỗng
//!   cho `Storage`).

use std::collections::HashMap;
use std::fmt;

use async_trait::async_trait;
use codegraph_core::{FileInfo, Symbol};

/// Decorator `Storage` bọc LRU cache (giảm gọi xuống backend).
pub mod cached;

#[cfg(feature = "sqlite")]
pub mod sqlite;

#[cfg(feature = "redis")]
pub mod redis;

#[cfg(feature = "lmdb")]
pub mod lmdb;

#[cfg(feature = "postgres")]
pub mod postgres;

#[cfg(feature = "mysql")]
pub mod mysql;

mod in_memory;

pub use in_memory::InMemoryStorage;

// ==================== Error Type ====================

/// Lỗi storage. Các trait con (`CategoryStorage`, `EntityStorage`, ...) đều
/// trả cùng kiểu `StorageError` để caller có thể dùng `?` xuyên qua trait
/// object.
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

// ==================== Helpers (chain + vector encoding) ====================

/// Mã hoá vector f32 thành BLOB little-endian (4 byte/phần tử) — chia sẻ cho
/// mọi backend persist (sqlite/lmdb/rdbms/redis) để lưu embedding vào storage.
#[allow(dead_code)]
pub(crate) fn encode_vector(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Giải mã BLOB little-endian thành vector f32. Trả `None` nếu độ dài không
/// chia hết cho 4 (corrupt).
#[allow(dead_code)]
pub(crate) fn decode_vector(b: &[u8]) -> Option<Vec<f32>> {
    if !b.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(b.len() / 4);
    for chunk in b.as_chunks::<4>().0 {
        out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Some(out)
}

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
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| u64::from_le_bytes(*c))
        .collect()
}

// ==================== IndexCounts ====================

/// Counts tổng hợp của index — `codegraph_status` đọc O(1) từ đĩa mà không
/// cần rebuild in-memory `GraphIndex` (vốn rất đắt trên repo lớn).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndexCounts {
    pub symbols: u64,
    pub chains: u64,
    pub edges: u64,
    pub files: u64,
    pub next_id: u64,
}

// ==================== Radix-node storage (CategoryStorage) ====================

/// Node id 0 là sentinel (rỗng) — dùng để đánh dấu "không có" trong radix.
pub const EMPTY: usize = 0;

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

// ── Bloom filter storage (feature-gated) ──

/// Lưu/đọc serialized bloom filter của mỗi node để `Radix::search_dfs` prune
/// nhánh không chứa substring. Tách riêng để trait lõi (`CategoryStorage`)
/// không bị rưới `#[cfg]` feature. `CategoryStorage` super-bound trait này
/// khi feature bật → method gọi được qua `dyn CategoryStorage` như cũ.
/// Backend không override → default no-op.
#[cfg(feature = "bloom-search")]
#[async_trait]
pub trait BloomStorage: Send + Sync {
    async fn set_node_bloom(&mut self, _id: usize, _bloom: &[u8]) -> Result<()> {
        Ok(())
    }
    async fn get_node_bloom(&self, _: usize) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
}

// ── Node metadata storage ──

/// Node-metadata storage: lưu/đọc metadata của node (opaque bytes) keyed theo
/// element id, cùng `clear`. Tách riêng để trait lõi gọn. `CategoryStorage`
/// super-bound trait này (luôn) → method gọi được qua `dyn CategoryStorage`.
/// Mặc định no-op.
#[async_trait]
pub trait NodeMetaStorage: Send + Sync {
    /// Lưu metadata của node (opaque bytes, VD Node JSON) keyed theo element id.
    async fn set_node_meta(&mut self, _elem: usize, _meta: &[u8]) -> Result<()> {
        Ok(())
    }
    /// Đọc node metadata — `None` nếu node chưa có.
    #[allow(dead_code)] // API giữ nguyên (protected) — GraphIndex dùng metas=None.
    async fn get_node_meta(&self, _elem: usize) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    /// Xoá toàn bộ node stream (dùng khi rebuild index).
    async fn clear_node_meta(&mut self) -> Result<()> {
        Ok(())
    }
    /// Lưu metadata (opaque bytes, VD: call-site info) cho một record — keyed
    /// theo record index (không phải element id).
    async fn set_meta(&mut self, _record: usize, _meta: &[u8]) -> Result<()> {
        Ok(())
    }
    /// Đọc metadata của record — `None` nếu record chưa có meta.
    async fn get_meta(&self, _record: usize) -> Result<Option<Vec<u8>>>;
    /// Lưu độ dài key (số element) của record — dùng filter `depth` khi search.
    async fn set_key_len(&mut self, _record: usize, _len: usize) -> Result<()> {
        Ok(())
    }
    /// Đọc độ dài key của record — `None` nếu record chưa insert.
    async fn get_key_len(&self, _record: usize) -> Result<Option<usize>>;
}

// ── Shortcut storage ──

/// Shortcut storage: auxiliary index for LIKE-search substring matching.
/// Stores which nodes contain each element in their prefix for fast candidate
/// lookup (KMP + DFS). Tách riêng để trait lõi gọn. `CategoryStorage`
/// super-bound trait này (luôn) → method gọi được qua `dyn CategoryStorage`.
/// Mặc định no-op.
#[async_trait]
pub trait ShortcutsStorage: Send + Sync {
    /// Thêm `node_id` vào shortcut set của element `elem` (encoded bytes).
    async fn add_shortcut_node(
        &mut self,
        _shard: usize,
        _elem: &[u8],
        _node_id: usize,
    ) -> Result<()> {
        Ok(())
    }
    /// Lấy toàn bộ node id chứa element `elem` trong shard.
    async fn get_shortcut_nodes(&self, _shard: usize, _elem: &[u8]) -> Result<Vec<usize>> {
        Ok(vec![])
    }
    /// Xoá toàn bộ shortcut sets (dùng khi rebuild index).
    async fn clear_shortcuts(&mut self) -> Result<()> {
        Ok(())
    }
}

// ── Edge data storage ──

/// Edge-data storage: lưu/đọc metadata của mỗi edge id (opaque bytes) keyed
/// theo edge id. Tách riêng để trait lõi gọn. `CategoryStorage` super-bound
/// trait này (luôn) → method gọi được qua `dyn CategoryStorage`. Mặc định no-op.
#[async_trait]
pub trait EdgeDataStorage: Send + Sync {
    /// Lưu dữ liệu edge (opaque bytes, VD CallEdgeMeta JSON) keyed theo edge id.
    async fn set_edge_data(&mut self, _edge: usize, _data: &[u8]) -> Result<()> {
        Ok(())
    }
    /// Đọc dữ liệu edge — `None` nếu edge chưa có.
    async fn get_edge_data(&self, _edge: usize) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    /// Xoá toàn bộ edge stream (dùng khi rebuild index).
    async fn clear_edges(&mut self) -> Result<()> {
        Ok(())
    }
}

// ── Chain storage ──

/// Chain storage: lưu/đọc per-owner chain (marker + symbol element ids),
/// encode u64 LE 8-byte/element. Tách riêng để trait lõi gọn.
/// `CategoryStorage` super-bound trait này (luôn) → method gọi được qua
/// `dyn CategoryStorage`. Mặc định no-op.
#[async_trait]
pub trait ChainStorage: Send + Sync {
    /// Lưu chain của owner (keyed theo record của owner; u64 LE 8-byte/element).
    async fn set_chain(&mut self, _record: usize, _chain: &[u64]) -> Result<()> {
        Ok(())
    }
    /// Đọc chain của owner — `None` nếu owner chưa có chain.
    async fn get_chain(&self, _record: usize) -> Result<Option<Vec<u64>>> {
        Ok(None)
    }
    /// Xoá toàn bộ chains (dùng khi rebuild index).
    async fn clear_chains(&mut self) -> Result<()> {
        Ok(())
    }
}

// ── CategoryStorage umbrella ──

/// Khai báo `CategoryStorage` — macro emit TOÀN BỘ trait (gồm `#[async_trait]`)
/// nên async_trait biến đổi ĐÚNG sau khi macro nở (khắc lỗi macro body trong
/// trait). `$bounds` = danh sách supertrait: luôn `Send + Sync + NodeMetaStorage`,
/// cộng `BloomStorage` khi feature `bloom-search`. Thân method không có `#[cfg]`
/// rải rác.
macro_rules! declare_category_storage {
    ($($bounds:tt)*) => {
        /// Radix-node storage: node management + transaction + 5 stream phụ.
        #[async_trait]
        pub trait CategoryStorage: $($bounds)* {
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

            // ── Shard roots (endpoint) ──
            async fn set_root(&mut self, shard: usize, root: usize) -> Result<()>;
            async fn get_root(&self, shard: usize) -> Result<usize>;

            // ── Transaction ──
            /// Bắt đầu một transaction (sync, không await — đúng theo cách radix gọi).
            /// Buffer ops; mọi thay đổi chỉ lộ ra khi `commit`.
            fn new_tx(&self) -> Box<dyn Tx>;
        }
    };
}

#[cfg(feature = "bloom-search")]
declare_category_storage!(
    Send + Sync
        + NodeMetaStorage
        + ShortcutsStorage
        + EdgeDataStorage
        + ChainStorage
        + BloomStorage
);

#[cfg(not(feature = "bloom-search"))]
declare_category_storage!(
    Send + Sync + NodeMetaStorage + ShortcutsStorage + EdgeDataStorage + ChainStorage
);

// ==================== Entity storage (chỉ codegraph-graph) ====================

/// Entity store — gồm symbol registry, call records, call-name index, files,
/// version, stats, embeddings. Tách khỏi radix-node storage vì:
/// - Chỉ `GraphIndex` / `SharedGraphIndex` dùng (`Radix` / `Search` không cần).
/// - Backend tối giản có thể bỏ qua (vd: chỉ cần `CategoryStorage` cho test).
/// - Cho phép phát triển/scale entity layer độc lập với radix.
#[async_trait]
pub trait EntityStorage: Send + Sync {
    // ── Symbol registry ──
    /// Lưu một symbol — mặc định: no-op.
    async fn save_symbol(&mut self, _sym: &Symbol) -> Result<()> {
        Ok(())
    }
    /// Đọc symbol theo id — mặc định: `None`.
    #[allow(dead_code)]
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
    /// Đọc `next_id` — mặc định: 0.
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

    // ── Call records ──
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

    // ── Call-name index ──
    /// Lưu inverted index `call name → call sites` (opaque bytes, JSON) — mặc
    /// định: no-op.
    async fn set_call_name_index(&mut self, _name: &str, _sites: &[u8]) -> Result<()> {
        Ok(())
    }
    /// Đọc call-name index — mặc định: `None`.
    #[allow(dead_code)]
    async fn load_call_name_index(&self, _name: &str) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    /// Toàn bộ call-name index `(name, bytes)` — mặc định: rỗng.
    #[cfg_attr(not(feature = "sqlite"), allow(dead_code))]
    async fn all_call_name_indexes(&self) -> Result<Vec<(String, Vec<u8>)>> {
        Ok(Vec::new())
    }

    // ── Files ──
    /// Upsert file info — mặc định: no-op.
    async fn upsert_file(&mut self, _f: &FileInfo) -> Result<()> {
        Ok(())
    }
    /// Toàn bộ files — mặc định: rỗng.
    #[cfg_attr(not(feature = "sqlite"), allow(dead_code))]
    async fn load_all_files(&self) -> Result<Vec<FileInfo>> {
        Ok(Vec::new())
    }

    // ── Version ──
    /// Version của index — mặc định: 0.
    #[cfg_attr(not(feature = "sqlite"), allow(dead_code))]
    async fn version(&self) -> Result<u64> {
        Ok(0)
    }
    /// Lưu version — mặc định: no-op.
    async fn set_version(&mut self, _v: u64) -> Result<()> {
        Ok(())
    }

    // ── Stats (counts tổng hợp) ──
    /// Lưu counts tổng hợp (symbols/chains/edges/files) — mặc định: no-op.
    async fn set_stats(&mut self, _s: IndexCounts) -> Result<()> {
        Ok(())
    }
    /// Đọc counts tổng hợp từ đĩa — mặc định: `IndexCounts::default()`.
    async fn stats(&self) -> Result<IndexCounts> {
        Ok(IndexCounts::default())
    }

    /// Xoá toàn bộ entity data (symbols/next_id/call_records/call_names/files/
    /// version/embeddings) — dùng khi full re-index. Mặc định: no-op.
    async fn clear_entities(&mut self) -> Result<()> {
        Ok(())
    }

    // ── Embeddings (vector per symbol id) ──
    /// Lưu vector embedding cho một symbol. Vector đã L2-normalize. Mặc định: no-op.
    async fn save_embedding(&mut self, _symbol_id: u64, _vector: &[f32]) -> Result<()> {
        Ok(())
    }
    /// Đọc vector embedding của symbol — mặc định: `None`.
    async fn load_embedding(&self, _symbol_id: u64) -> Result<Option<Vec<f32>>> {
        Ok(None)
    }
    /// Đọc toàn bộ embeddings — mặc định: rỗng.
    async fn load_all_embeddings(&self) -> Result<HashMap<u64, Vec<f32>>> {
        Ok(HashMap::new())
    }
    /// Xoá toàn bộ embeddings — mặc định: no-op.
    async fn clear_embeddings(&mut self) -> Result<()> {
        Ok(())
    }
    /// KNN backend-native (SQLite + sqlite-vss). Trả `Some(hits)` nếu backend
    /// hỗ trợ ANN, `None` để caller fallback sang `VectorIndex` in-memory.
    /// Mặc định: `None`.
    async fn knn(&self, _query_vec: &[f32], _k: usize) -> Result<Option<Vec<(u64, f32)>>> {
        Ok(None)
    }
}

// ==================== Storage umbrella ====================

/// Umbrella trait cho `Arc<RwLock<dyn Storage>>` trong `GraphIndex`.
///
/// Gộp `CategoryStorage` + 5 trait phụ + `EntityStorage`. Backend implement
/// 7 `impl` block riêng biệt — review từng phần độc lập được.
#[async_trait]
pub trait Storage:
    CategoryStorage
    + NodeMetaStorage
    + ShortcutsStorage
    + EdgeDataStorage
    + ChainStorage
    + EntityStorage
    + Send
    + Sync
{
}
