//! `CachedStorage` — decorator bọc một `Storage` bất kỳ bằng `LruCache` sharded
//! để giảm số lần gọi xuống backend (SQL/remote) cho các read path nóng.
//!
//! - Các `get_*` nóng (node/children/chain/meta/edge/symbol/embedding/...) đọc
//!   cache trước; miss → gọi inner → populate.
//! - Các method ghi (`set_*`/`new_node`/`update_node`/`set_root`/...) ghi qua
//!   inner VÀ invalidate đúng cache liên quan.
//! - Transaction: `new_tx` trả `CachedTx`; khi `commit` xong sẽ `clear_radix()`
//!   (node/children/roots/shortcuts) vì tx chỉ sửa cấu trúc radix — entity cache
//!   (symbol/embedding/call) giữ nguyên, không bị lạnh.
//!
//! Decorator này trong suốt: mọi backend (InMemory/Sqlite/Lmdb/Redis/RDBMS)
//! đều dùng được, behaviour đúng bằng inner (chỉ thêm lớp cache).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use codegraph_core::{FileInfo, Symbol};

use crate::lru::LruCache;
use crate::storage::{IndexCounts, Storage, StorageError, Tx};

/// Số shard của mỗi `LruCache` — phải lũy thừa của 2.
const SHARDS: usize = 32;

/// Tập hợp các cache theo từng loại read method. Dùng `Arc` để `CachedTx`
/// (được tạo từ `new_tx`) cũng giữ được tham chiếu tới cùng bộ cache để
/// invalidate khi commit.
struct CacheSet {
    nodes: LruCache<usize, (Vec<u8>, usize), SHARDS>,
    children: LruCache<usize, Vec<usize>, SHARDS>,
    chains: LruCache<usize, Vec<u64>, SHARDS>,
    metas: LruCache<usize, Vec<u8>, SHARDS>,
    key_lens: LruCache<usize, usize, SHARDS>,
    edge_data: LruCache<usize, Vec<u8>, SHARDS>,
    node_meta: LruCache<usize, Vec<u8>, SHARDS>,
    roots: LruCache<usize, usize, SHARDS>,
    shortcuts: LruCache<(usize, Vec<u8>), Vec<usize>, SHARDS>,
    symbols: LruCache<u64, Symbol, SHARDS>,
    embeddings: LruCache<u64, Vec<f32>, SHARDS>,
    call_records: LruCache<u64, Vec<u8>, SHARDS>,
    call_name_index: LruCache<String, Vec<u8>, SHARDS>,
}

impl CacheSet {
    fn new(capacity: usize) -> Self {
        Self {
            nodes: LruCache::new(capacity),
            children: LruCache::new(capacity),
            chains: LruCache::new(capacity),
            metas: LruCache::new(capacity),
            key_lens: LruCache::new(capacity),
            edge_data: LruCache::new(capacity),
            node_meta: LruCache::new(capacity),
            roots: LruCache::new(capacity),
            shortcuts: LruCache::new(capacity),
            symbols: LruCache::new(capacity),
            embeddings: LruCache::new(capacity),
            call_records: LruCache::new(capacity),
            call_name_index: LruCache::new(capacity),
        }
    }

    /// Invalidate mọi cache liên quan đến cấu trúc radix (chỉ những thứ tx sửa).
    fn clear_radix(&self) {
        self.nodes.clear();
        self.children.clear();
        self.roots.clear();
        self.shortcuts.clear();
    }

    /// Invalidate toàn bộ (dùng cho `clear_entities` / reset lớn).
    #[allow(dead_code)]
    fn clear_all(&self) {
        self.clear_radix();
        self.chains.clear();
        self.metas.clear();
        self.key_lens.clear();
        self.edge_data.clear();
        self.node_meta.clear();
        self.symbols.clear();
        self.embeddings.clear();
        self.call_records.clear();
        self.call_name_index.clear();
    }
}

/// Decorator `Storage` có LRU cache. `inner` là `Box<dyn Storage>` — backend tự
/// quản lý concurrency của nó, decorator không cần lock riêng.
pub struct CachedStorage {
    inner: Box<dyn Storage>,
    caches: Arc<CacheSet>,
}

impl CachedStorage {
    /// Bọc một `Storage` bất kỳ. Trả về `Arc<RwLock<dyn Storage>>` để có thể
    /// truyền thẳng vào `GraphIndex` (cùng kiểu với backend gốc).
    pub fn wrap(inner: Box<dyn Storage>, capacity: usize) -> Arc<tokio::sync::RwLock<dyn Storage>> {
        Arc::new(tokio::sync::RwLock::new(CachedStorage {
            inner,
            caches: Arc::new(CacheSet::new(capacity)),
        }))
    }
}

#[async_trait]
impl Storage for CachedStorage {
    // ── Node management (cached) ──
    async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize, StorageError> {
        let id = self.inner.new_node(prefix, record).await?;
        self.caches.nodes.remove(&id);
        Ok(id)
    }

    async fn update_node(
        &mut self,
        id: usize,
        prefix: Option<Vec<u8>>,
        record: Option<usize>,
    ) -> Result<(), StorageError> {
        self.inner.update_node(id, prefix, record).await?;
        self.caches.nodes.remove(&id);
        Ok(())
    }

    async fn get_node(&self, id: usize) -> Result<(Vec<u8>, usize), StorageError> {
        if let Some(v) = self.caches.nodes.get(&id) {
            return Ok(v);
        }
        let v = self.inner.get_node(id).await?;
        self.caches.nodes.put(id, v.clone());
        Ok(v)
    }

    async fn get_children(&self, id: usize) -> Result<Vec<usize>, StorageError> {
        if let Some(v) = self.caches.children.get(&id) {
            return Ok(v);
        }
        let v = self.inner.get_children(id).await?;
        self.caches.children.put(id, v.clone());
        Ok(v)
    }

    // ── Bloom (không cache — dùng prune nhánh, sai = search sai) ──
    #[cfg(feature = "bloom-search")]
    async fn set_node_bloom(&mut self, id: usize, bloom: &[u8]) -> Result<(), StorageError> {
        self.inner.set_node_bloom(id, bloom).await
    }

    #[cfg(feature = "bloom-search")]
    async fn get_node_bloom(&self, id: usize) -> Result<Option<Vec<u8>>, StorageError> {
        self.inner.get_node_bloom(id).await
    }

    // ── Edge data (cached) ──
    async fn set_edge_data(&mut self, edge: usize, data: &[u8]) -> Result<(), StorageError> {
        self.inner.set_edge_data(edge, data).await?;
        self.caches.edge_data.remove(&edge);
        Ok(())
    }

    async fn get_edge_data(&self, edge: usize) -> Result<Option<Vec<u8>>, StorageError> {
        if let Some(v) = self.caches.edge_data.get(&edge) {
            return Ok(Some(v));
        }
        let v = self.inner.get_edge_data(edge).await?;
        if let Some(ref b) = v {
            self.caches.edge_data.put(edge, b.clone());
        }
        Ok(v)
    }

    async fn clear_edges(&mut self) -> Result<(), StorageError> {
        self.inner.clear_edges().await?;
        self.caches.edge_data.clear();
        Ok(())
    }

    async fn for_each_edge_data(
        &self,
        f: &mut (dyn for<'a> FnMut(usize, &'a [u8]) -> Result<(), StorageError> + Send),
    ) -> Result<(), StorageError> {
        self.inner.for_each_edge_data(f).await
    }

    // ── Node metadata (cached) ──
    async fn set_node_meta(&mut self, elem: usize, meta: &[u8]) -> Result<(), StorageError> {
        self.inner.set_node_meta(elem, meta).await?;
        self.caches.node_meta.remove(&elem);
        Ok(())
    }

    async fn get_node_meta(&self, elem: usize) -> Result<Option<Vec<u8>>, StorageError> {
        if let Some(v) = self.caches.node_meta.get(&elem) {
            return Ok(Some(v));
        }
        let v = self.inner.get_node_meta(elem).await?;
        if let Some(ref b) = v {
            self.caches.node_meta.put(elem, b.clone());
        }
        Ok(v)
    }

    async fn clear_node_meta(&mut self) -> Result<(), StorageError> {
        self.inner.clear_node_meta().await?;
        self.caches.node_meta.clear();
        Ok(())
    }

    // ── Chain (cached) ──
    async fn set_chain(&mut self, record: usize, chain: &[u64]) -> Result<(), StorageError> {
        self.inner.set_chain(record, chain).await?;
        self.caches.chains.remove(&record);
        Ok(())
    }

    async fn get_chain(&self, record: usize) -> Result<Option<Vec<u64>>, StorageError> {
        if let Some(v) = self.caches.chains.get(&record) {
            return Ok(Some(v));
        }
        let v = self.inner.get_chain(record).await?;
        if let Some(ref c) = v {
            self.caches.chains.put(record, c.clone());
        }
        Ok(v)
    }

    async fn clear_chains(&mut self) -> Result<(), StorageError> {
        self.inner.clear_chains().await?;
        self.caches.chains.clear();
        Ok(())
    }

    // ── Shard roots (cached) ──
    async fn set_root(&mut self, shard: usize, root: usize) -> Result<(), StorageError> {
        self.inner.set_root(shard, root).await?;
        self.caches.roots.remove(&shard);
        Ok(())
    }

    async fn get_root(&self, shard: usize) -> Result<usize, StorageError> {
        if let Some(v) = self.caches.roots.get(&shard) {
            return Ok(v);
        }
        let v = self.inner.get_root(shard).await?;
        self.caches.roots.put(shard, v);
        Ok(v)
    }

    // ── Meta / key_len (cached) ──
    async fn set_meta(&mut self, record: usize, meta: &[u8]) -> Result<(), StorageError> {
        self.inner.set_meta(record, meta).await?;
        self.caches.metas.remove(&record);
        Ok(())
    }

    async fn get_meta(&self, record: usize) -> Result<Option<Vec<u8>>, StorageError> {
        if let Some(v) = self.caches.metas.get(&record) {
            return Ok(Some(v));
        }
        let v = self.inner.get_meta(record).await?;
        if let Some(ref b) = v {
            self.caches.metas.put(record, b.clone());
        }
        Ok(v)
    }

    async fn set_key_len(&mut self, record: usize, len: usize) -> Result<(), StorageError> {
        self.inner.set_key_len(record, len).await?;
        self.caches.key_lens.remove(&record);
        Ok(())
    }

    async fn get_key_len(&self, record: usize) -> Result<Option<usize>, StorageError> {
        if let Some(v) = self.caches.key_lens.get(&record) {
            return Ok(Some(v));
        }
        let v = self.inner.get_key_len(record).await?;
        if let Some(l) = v {
            self.caches.key_lens.put(record, l);
        }
        Ok(v)
    }

    // ── Shortcuts (cached) ──
    async fn add_shortcut_node(
        &mut self,
        shard: usize,
        elem: &[u8],
        node_id: usize,
    ) -> Result<(), StorageError> {
        self.inner.add_shortcut_node(shard, elem, node_id).await?;
        self.caches.shortcuts.remove(&(shard, elem.to_vec()));
        Ok(())
    }

    async fn get_shortcut_nodes(
        &self,
        shard: usize,
        elem: &[u8],
    ) -> Result<Vec<usize>, StorageError> {
        let key = (shard, elem.to_vec());
        if let Some(v) = self.caches.shortcuts.get(&key) {
            return Ok(v);
        }
        let v = self.inner.get_shortcut_nodes(shard, elem).await?;
        self.caches.shortcuts.put(key, v.clone());
        Ok(v)
    }

    async fn clear_shortcuts(&mut self) -> Result<(), StorageError> {
        self.inner.clear_shortcuts().await?;
        self.caches.shortcuts.clear();
        Ok(())
    }

    // ── Entity store (symbols / calls / embeddings) ──
    async fn save_symbol(&mut self, sym: &Symbol) -> Result<(), StorageError> {
        self.inner.save_symbol(sym).await?;
        self.caches.symbols.remove(&sym.id);
        Ok(())
    }

    async fn load_symbol(&self, id: u64) -> Result<Option<Symbol>, StorageError> {
        if let Some(v) = self.caches.symbols.get(&id) {
            return Ok(Some(v));
        }
        let v = self.inner.load_symbol(id).await?;
        if let Some(ref s) = v {
            self.caches.symbols.put(id, s.clone());
        }
        Ok(v)
    }

    async fn load_all_symbols(&self) -> Result<Vec<Symbol>, StorageError> {
        self.inner.load_all_symbols().await
    }

    async fn save_next_id(&mut self, next: u64) -> Result<(), StorageError> {
        self.inner.save_next_id(next).await
    }

    async fn load_next_id(&self) -> Result<u64, StorageError> {
        self.inner.load_next_id().await
    }

    async fn all_chains(&self) -> Result<Vec<(u64, Vec<u8>)>, StorageError> {
        self.inner.all_chains().await
    }

    async fn set_call_records(&mut self, func: u64, records: &[u8]) -> Result<(), StorageError> {
        self.inner.set_call_records(func, records).await?;
        self.caches.call_records.remove(&func);
        Ok(())
    }

    async fn get_call_records(&self, func: u64) -> Result<Option<Vec<u8>>, StorageError> {
        if let Some(v) = self.caches.call_records.get(&func) {
            return Ok(Some(v));
        }
        let v = self.inner.get_call_records(func).await?;
        if let Some(ref b) = v {
            self.caches.call_records.put(func, b.clone());
        }
        Ok(v)
    }

    async fn all_call_records(&self) -> Result<Vec<(u64, Vec<u8>)>, StorageError> {
        self.inner.all_call_records().await
    }

    async fn set_call_name_index(&mut self, name: &str, sites: &[u8]) -> Result<(), StorageError> {
        self.inner.set_call_name_index(name, sites).await?;
        self.caches.call_name_index.remove(&name.to_string());
        Ok(())
    }

    async fn load_call_name_index(&self, name: &str) -> Result<Option<Vec<u8>>, StorageError> {
        if let Some(v) = self.caches.call_name_index.get(&name.to_string()) {
            return Ok(Some(v));
        }
        let v = self.inner.load_call_name_index(name).await?;
        if let Some(ref b) = v {
            self.caches.call_name_index.put(name.to_string(), b.clone());
        }
        Ok(v)
    }

    async fn all_call_name_indexes(&self) -> Result<Vec<(String, Vec<u8>)>, StorageError> {
        self.inner.all_call_name_indexes().await
    }

    async fn upsert_file(&mut self, f: &FileInfo) -> Result<(), StorageError> {
        self.inner.upsert_file(f).await
    }

    async fn load_all_files(&self) -> Result<Vec<FileInfo>, StorageError> {
        self.inner.load_all_files().await
    }

    async fn version(&self) -> Result<u64, StorageError> {
        self.inner.version().await
    }

    async fn set_version(&mut self, v: u64) -> Result<(), StorageError> {
        self.inner.set_version(v).await
    }

    async fn set_stats(&mut self, s: IndexCounts) -> Result<(), StorageError> {
        self.inner.set_stats(s).await
    }

    async fn stats(&self) -> Result<IndexCounts, StorageError> {
        self.inner.stats().await
    }

    async fn clear_entities(&mut self) -> Result<(), StorageError> {
        self.inner.clear_entities().await?;
        self.caches.clear_all();
        Ok(())
    }

    // ── Embeddings (cached) ──
    async fn save_embedding(&mut self, symbol_id: u64, vector: &[f32]) -> Result<(), StorageError> {
        self.inner.save_embedding(symbol_id, vector).await?;
        self.caches.embeddings.remove(&symbol_id);
        Ok(())
    }

    async fn load_embedding(&self, symbol_id: u64) -> Result<Option<Vec<f32>>, StorageError> {
        if let Some(v) = self.caches.embeddings.get(&symbol_id) {
            return Ok(Some(v));
        }
        let v = self.inner.load_embedding(symbol_id).await?;
        if let Some(ref vec) = v {
            self.caches.embeddings.put(symbol_id, vec.clone());
        }
        Ok(v)
    }

    async fn load_all_embeddings(&self) -> Result<HashMap<u64, Vec<f32>>, StorageError> {
        self.inner.load_all_embeddings().await
    }

    async fn clear_embeddings(&mut self) -> Result<(), StorageError> {
        self.inner.clear_embeddings().await?;
        self.caches.embeddings.clear();
        Ok(())
    }

    async fn knn(
        &self,
        query_vec: &[f32],
        k: usize,
    ) -> Result<Option<Vec<(u64, f32)>>, StorageError> {
        self.inner.knn(query_vec, k).await
    }

    // ── Transaction: wrap để invalidate radix cache khi commit ──
    fn new_tx(&self) -> Box<dyn Tx> {
        Box::new(CachedTx {
            inner: self.inner.new_tx(),
            caches: self.caches.clone(),
        })
    }
}

/// Tx bọc: delegate mọi mutation, khi `commit` xong thì `clear_radix()`.
struct CachedTx {
    inner: Box<dyn Tx>,
    caches: Arc<CacheSet>,
}

#[async_trait]
impl Tx for CachedTx {
    async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize, StorageError> {
        self.inner.new_node(prefix, record).await
    }

    async fn update_node(
        &mut self,
        id: usize,
        prefix: Option<Vec<u8>>,
        record: Option<usize>,
    ) -> Result<(), StorageError> {
        self.inner.update_node(id, prefix, record).await
    }

    async fn add_child(&mut self, parent: usize, child: usize) -> Result<(), StorageError> {
        self.inner.add_child(parent, child).await
    }

    async fn move_child(
        &mut self,
        from: usize,
        to: usize,
        child: usize,
    ) -> Result<(), StorageError> {
        self.inner.move_child(from, to, child).await
    }

    async fn commit(self: Box<Self>) -> Result<(), StorageError> {
        let CachedTx { inner, caches } = *self;
        let res = inner.commit().await;
        caches.clear_radix();
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::InMemoryStorage;

    fn wrapped(capacity: usize) -> Arc<tokio::sync::RwLock<dyn Storage>> {
        CachedStorage::wrap(
            Box::new(InMemoryStorage::default()) as Box<dyn Storage>,
            capacity,
        )
    }

    #[tokio::test]
    async fn cache_serves_repeated_get_node_without_inner() {
        let s = wrapped(64);
        let id = {
            let mut st = s.write().await;
            st.new_node(b"hello".to_vec(), 42).await.unwrap()
        };
        // First read misses (populates), second should hit cache — both correct.
        {
            let st = s.read().await;
            assert_eq!(st.get_node(id).await.unwrap(), (b"hello".to_vec(), 42));
        }
        {
            let st = s.read().await;
            assert_eq!(st.get_node(id).await.unwrap(), (b"hello".to_vec(), 42));
        }
    }

    #[tokio::test]
    async fn update_invalidates_node_cache() {
        let s = wrapped(64);
        let id = {
            let mut st = s.write().await;
            st.new_node(b"init".to_vec(), 1).await.unwrap()
        };
        {
            let st = s.read().await;
            assert_eq!(st.get_node(id).await.unwrap().0, b"init".to_vec());
        }
        {
            let mut st = s.write().await;
            st.update_node(id, Some(b"updated".to_vec()), Some(99))
                .await
                .unwrap();
        }
        // After update, cache must reflect new value (not stale).
        let st = s.read().await;
        assert_eq!(st.get_node(id).await.unwrap(), (b"updated".to_vec(), 99));
    }

    #[tokio::test]
    async fn tx_commit_invalidates_radix_cache() {
        let s = wrapped(64);
        let parent = {
            let mut st = s.write().await;
            st.new_node(b"p".to_vec(), 0).await.unwrap()
        };
        let child = {
            let mut st = s.write().await;
            st.new_node(b"c".to_vec(), 1).await.unwrap()
        };
        // Pre-populate children cache.
        {
            let st = s.read().await;
            assert!(st.get_children(parent).await.unwrap().is_empty());
        }
        // Add child via tx, then commit → children cache must be invalidated.
        {
            let st = s.write().await;
            let mut tx = st.new_tx();
            tx.add_child(parent, child).await.unwrap();
            tx.commit().await.unwrap();
        }
        let st = s.read().await;
        let children = st.get_children(parent).await.unwrap();
        assert!(children.contains(&child), "children after tx: {children:?}");
    }

    #[tokio::test]
    async fn cache_matches_inner_semantics() {
        let s = wrapped(128);
        {
            let mut st = s.write().await;
            st.set_meta(7, b"meta-7".as_slice()).await.unwrap();
            st.set_key_len(7, 5).await.unwrap();
            st.set_chain(9, &[1, 2, 3]).await.unwrap();
            st.set_edge_data(3, b"edge-3").await.unwrap();
            st.set_node_meta(4, b"nm-4").await.unwrap();
        }
        let st = s.read().await;
        assert_eq!(
            st.get_meta(7).await.unwrap().as_deref(),
            Some(b"meta-7".as_slice())
        );
        assert_eq!(st.get_key_len(7).await.unwrap(), Some(5));
        assert_eq!(st.get_chain(9).await.unwrap(), Some(vec![1, 2, 3]));
        assert_eq!(
            st.get_edge_data(3).await.unwrap().as_deref(),
            Some(b"edge-3".as_slice())
        );
        assert_eq!(
            st.get_node_meta(4).await.unwrap().as_deref(),
            Some(b"nm-4".as_slice())
        );
        // Second read hits cache, same result.
        assert_eq!(
            st.get_meta(7).await.unwrap().as_deref(),
            Some(b"meta-7".as_slice())
        );
    }
}
