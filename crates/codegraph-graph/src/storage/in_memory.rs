use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use codegraph_core::{FileInfo, Symbol};

use super::{
    CategoryStorage, ChainStorage, EMPTY, EdgeDataStorage, EntityStorage, IndexCounts,
    NodeMetaStorage, Result, ShortcutsStorage, StorageError, Tx, TxOp, decode_chain,
    encode_chain,
};

#[cfg(feature = "bloom-search")]
use super::BloomStorage;

// ==================== Transaction ====================

/// Transaction cho `InMemoryStorage`: buffer toàn bộ mutation, áp dụng
/// atomic dưới 1 write lock tại `commit`.
pub(crate) struct InMemoryTx {
    data: Arc<RwLock<MemoryData>>,
    next_id: Arc<AtomicUsize>,
    /// (reserved_id, prefix, record) — được append tại commit.
    nodes: Vec<(usize, Vec<u8>, usize)>,
    ops: Vec<TxOp>,
}

impl InMemoryTx {
    pub(crate) fn new(data: Arc<RwLock<MemoryData>>, next_id: Arc<AtomicUsize>) -> Self {
        Self {
            data,
            next_id,
            nodes: Vec::new(),
            ops: Vec::new(),
        }
    }
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

// ==================== In-Memory Storage ====================

pub(crate) struct MemoryData {
    /// (prefix, record) — index 0 là sentinel.
    pub(crate) nodes: Vec<(Vec<u8>, usize)>,
    /// children list per node (index 0 = sentinel).
    pub(crate) children: Vec<Vec<usize>>,
    /// root id per shard.
    pub(crate) roots: Vec<usize>,
    /// record_idx → metadata (opaque bytes, VD: call-site info).
    pub(crate) meta: HashMap<usize, Vec<u8>>,
    /// record_idx → độ dài key (số element) — dùng filter `depth` khi search.
    pub(crate) key_lens: HashMap<usize, usize>,
    /// shortcuts[shard][elem_bytes] = node ids chứa elem trong prefix.
    pub(crate) shortcuts: Vec<HashMap<Vec<u8>, HashSet<usize>>>,
    /// edge id → dữ liệu edge (opaque bytes, VD EdgeMeta JSON).
    pub(crate) edges: HashMap<usize, Vec<u8>>,
    /// element id → node metadata (Node JSON).
    pub(crate) node_meta: HashMap<usize, Vec<u8>>,
    /// node id → serialize bloom filter (prune nhánh trong search_dfs).
    #[cfg(feature = "bloom-search")]
    pub(crate) blooms: HashMap<usize, Vec<u8>>,
    /// record (owner) → chain bytes (u64 LE 8-byte/element).
    pub(crate) chains: HashMap<usize, Vec<u8>>,
    // ── Entity store (semgraph model) ──
    /// symbol id → Symbol.
    pub(crate) symbols: HashMap<u64, Symbol>,
    /// next_id của symbol registry.
    pub(crate) next_id: u64,
    /// func id → call records (JSON).
    pub(crate) call_records: HashMap<u64, Vec<u8>>,
    /// call name → call sites (JSON).
    pub(crate) call_names: HashMap<String, Vec<u8>>,
    /// path → FileInfo.
    pub(crate) files: HashMap<String, FileInfo>,
    /// index version.
    pub(crate) version: u64,
    /// symbol id → embedding vector (L2-normalized f32).
    pub(crate) embeddings: HashMap<u64, Vec<f32>>,
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
                #[cfg(feature = "bloom-search")]
                blooms: HashMap::new(),
                chains: HashMap::new(),
                symbols: HashMap::new(),
                // Id bắt đầu từ SYMBOL_BASE (marker reserved 1..=99).
                next_id: codegraph_core::SYMBOL_BASE,
                call_records: HashMap::new(),
                call_names: HashMap::new(),
                files: HashMap::new(),
                version: 0,
                embeddings: HashMap::new(),
            })),
            next_id: Arc::new(AtomicUsize::new(1)),
        }
    }

    /// Reserve một id mới (dùng chung cho cả new_node trực tiếp lẫn tx).
    fn alloc_id(&self) -> usize {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }
}

impl Default for InMemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

// ==================== CategoryStorage ====================

#[async_trait]
impl CategoryStorage for InMemoryStorage {
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

    fn new_tx(&self) -> Box<dyn Tx> {
        Box::new(InMemoryTx::new(self.data.clone(), self.next_id.clone()))
    }
}

// ==================== NodeMetaStorage ====================

#[async_trait]
impl NodeMetaStorage for InMemoryStorage {
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
}

// ==================== ShortcutsStorage ====================

#[async_trait]
impl ShortcutsStorage for InMemoryStorage {
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
}

// ==================== EdgeDataStorage ====================

#[async_trait]
impl EdgeDataStorage for InMemoryStorage {
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
}

// ==================== ChainStorage ====================

#[async_trait]
impl ChainStorage for InMemoryStorage {
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
}

// ==================== BloomStorage (feature-gated) ====================

#[cfg(feature = "bloom-search")]
#[async_trait]
impl BloomStorage for InMemoryStorage {
    async fn set_node_bloom(&mut self, id: usize, bloom: &[u8]) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        d.blooms.insert(id, bloom.to_vec());
        Ok(())
    }

    async fn get_node_bloom(&self, id: usize) -> Result<Option<Vec<u8>>> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        Ok(d.blooms.get(&id).cloned())
    }
}

// ==================== EntityStorage ====================

#[async_trait]
impl EntityStorage for InMemoryStorage {
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
        Ok(d.call_records
            .iter()
            .map(|(&f, b)| (f, b.clone()))
            .collect())
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
        Ok(d.call_names
            .iter()
            .map(|(n, b)| (n.clone(), b.clone()))
            .collect())
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

    async fn set_stats(&mut self, _s: IndexCounts) -> Result<()> {
        // In-memory không persist stats (rebuild O(1) thông qua len() các map).
        Ok(())
    }

    async fn stats(&self) -> Result<IndexCounts> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        Ok(IndexCounts {
            symbols: d.symbols.len() as u64,
            chains: d.chains.len() as u64,
            edges: d.edges.len() as u64,
            files: d.files.len() as u64,
            next_id: d.next_id,
        })
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
        d.embeddings.clear();
        Ok(())
    }

    async fn save_embedding(&mut self, symbol_id: u64, vector: &[f32]) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        d.embeddings.insert(symbol_id, vector.to_vec());
        Ok(())
    }

    async fn load_embedding(&self, symbol_id: u64) -> Result<Option<Vec<f32>>> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        Ok(d.embeddings.get(&symbol_id).cloned())
    }

    async fn load_all_embeddings(&self) -> Result<HashMap<u64, Vec<f32>>> {
        let d = self
            .data
            .read()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        Ok(d.embeddings.clone())
    }

    async fn clear_embeddings(&mut self) -> Result<()> {
        let mut d = self
            .data
            .write()
            .map_err(|_| StorageError::Internal("poison".into()))?;
        d.embeddings.clear();
        Ok(())
    }

    // knn mặc định: trả None → caller fallback `VectorIndex` in-memory.
}

// ==================== Storage umbrella (empty marker) ====================

use super::Storage;

#[async_trait]
impl Storage for InMemoryStorage {}

// ==================== Tests ====================

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
        assert_eq!(s.get_meta(7).await.unwrap(), None);
        assert_eq!(s.get_key_len(7).await.unwrap(), None);
        s.set_meta(7, b"call-site-info".as_slice()).await.unwrap();
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
        let mut s = InMemoryStorage::default();
        assert!(s.get_shortcut_nodes(1, b"l").await.unwrap().is_empty());
        s.add_shortcut_node(1, b"l", 10).await.unwrap();
        s.add_shortcut_node(1, b"l", 20).await.unwrap();
        s.add_shortcut_node(1, b"o", 10).await.unwrap();
        s.add_shortcut_node(2, b"l", 30).await.unwrap();
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
        let mut s = InMemoryStorage::default();
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
        assert_eq!(s.get_edge_data(7).await.unwrap(), None);
        s.set_edge_data(7, b"call-site").await.unwrap();
        assert_eq!(
            s.get_edge_data(7).await.unwrap().as_deref(),
            Some(b"call-site".as_slice())
        );
        s.set_edge_data(7, b"updated").await.unwrap();
        assert_eq!(
            s.get_edge_data(7).await.unwrap().as_deref(),
            Some(b"updated".as_slice())
        );
        assert_eq!(s.get_edge_data(8).await.unwrap(), None);
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

    #[tokio::test]
    async fn test_entity_symbols() {
        use codegraph_core::{ScopeLevel, SymbolKind};
        let mut s = InMemoryStorage::default();
        let sym = Symbol {
            id: 100,
            name: "foo".into(),
            kind: SymbolKind::Function,
            scope: ScopeLevel::Global,
            scope_id: 0,
            type_ref: 0,
            type_name: None,
            file: String::new(),
            line: 0,
            end_line: 0,
            signature: None,
            doc: None,
            annotations: Vec::new(),
            language: "rust".into(),
        };
        s.save_symbol(&sym).await.unwrap();
        let loaded = s.load_symbol(100).await.unwrap();
        assert_eq!(loaded.unwrap().name, "foo");
        assert_eq!(s.load_all_symbols().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_entity_clear() {
        let mut s = InMemoryStorage::default();
        s.set_call_records(1, b"rec").await.unwrap();
        s.set_call_name_index("name", b"sites").await.unwrap();
        s.set_version(5).await.unwrap();
        s.clear_entities().await.unwrap();
        assert_eq!(s.get_call_records(1).await.unwrap(), None);
        assert_eq!(s.load_call_name_index("name").await.unwrap(), None);
        assert_eq!(s.version().await.unwrap(), 0);
    }
}
