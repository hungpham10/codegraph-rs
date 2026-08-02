//! Search module — KMP + DFS substring ("LIKE") search trên RadixTree + Storage.
//!
//! ## Idea
//! Duy trì **shortcuts** (in-memory map) giúp tìm nhanh các node có chứa ký tự
//! đầu tiên của pattern. Với mỗi candidate, chạy **KMP** matching trên prefix
//! của node; nếu prefix ngắn hơn pattern thì **DFS** xuống children.
//!
//! ## Shortcut structure
//! ```text
//! shortcuts[shard][elem] = HashSet<node_id>
//! ```
//! - `shard` — shard index (0..sharding)
//! - `elem` — u64 element bất kỳ
//! - `HashSet<node_id>` — các node có chứa element đó trong prefix
//!
//! Shortcuts chỉ là **index nhanh** để tìm candidate node, không lưu vị trí.
//! Vị trí được scan trực tiếp từ prefix của node khi search.
//!
//! Shortcuts được cập nhật:
//! - Khi **insert** node mới → `update_shortcuts()`
//! - Khi **split** node → callback `OnSplitCallback` transfer entries từ parent sang leg

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::lru::LruCache;
use crate::radixtree::{self, EMPTY, KeyElement, RadixTree};
use crate::storage::Storage;

#[cfg(feature = "bloom-search")]
use smallvec::SmallVec;

#[cfg(feature = "bloom-search")]
use crate::bloom::BloomFilter;

// ==================== Constants ====================

/// Capacity của node cache (LRU).
/// 25K entries × ~120 bytes ≈ 3MB — rất nhẹ.
const NODE_CACHE_CAPACITY: usize = 25_000;

/// Số shard cho node cache (luỹ thừa của 2).
const NODE_CACHE_SHARDS: usize = 8;

// ==================== Error ====================

#[derive(Debug)]
pub enum SearchError {
    #[allow(dead_code)]
    NotFound,
    Storage(String),
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchError::NotFound => write!(f, "not found"),
            SearchError::Storage(msg) => write!(f, "storage error: {msg}"),
        }
    }
}

impl std::error::Error for SearchError {}

impl From<radixtree::RadixError> for SearchError {
    fn from(e: radixtree::RadixError) -> Self {
        match e {
            radixtree::RadixError::NotFound => SearchError::NotFound,
            _ => SearchError::Storage(e.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, SearchError>;

// ==================== Bloom Filter (pruning) ====================

/// Bloom filter: tỉ lệ false positive ~1% với ~300 items.
#[cfg(feature = "bloom-search")]
const BLOOM_M: usize = 4096;

/// Số hash functions cho bloom filter.
#[cfg(feature = "bloom-search")]
const BLOOM_K: usize = 10;

/// Bloom filter chỉ active khi số candidates >= ngưỡng này.
/// Mặc định: 50. Config qua `SearchIndex::set_bloom_candidates_threshold()`.
#[cfg(feature = "bloom-search")]
const BLOOM_DEFAULT_THRESHOLD: usize = 50;

/// Trích xuất features từ data (T slice) để kiểm tra bloom filter.
///
/// Encode mỗi T → bytes, rồi extract unigrams + bigrams từ encoded bytes.
/// Cần đồng bộ với `rebuild_bloom_from_nodes` — cũng insert cả unigrams + bigrams.
#[cfg(feature = "bloom-search")]
type Feature = SmallVec<[u8; 8]>;

#[cfg(feature = "bloom-search")]
#[inline]
fn extract_bloom_features<T: KeyElement>(data: &[T]) -> SmallVec<[Feature; 8]> {
    if data.is_empty() {
        return smallvec::SmallVec::new();
    }
    // Encode tất cả T values thành bytes để extract features
    let encoded = RadixTree::<T>::encode_key(data);
    let mut features: SmallVec<[Feature; 8]> =
        smallvec::SmallVec::with_capacity(encoded.len().saturating_mul(2));
    // Unigrams: từng byte riêng lẻ
    for &byte in encoded.iter() {
        features.push(smallvec::smallvec![byte]);
    }
    // Bigrams: nếu đủ dài
    if encoded.len() >= 2 {
        for w in encoded.windows(2) {
            features.push(smallvec::smallvec![w[0], w[1]]);
        }
    }
    features
}

// ==================== Shortcut Data ====================

/// shortcuts[shard][elem] = HashSet<node_id>
/// Chỉ lưu node nào có chứa T element đó, không lưu vị trí.
/// Vị trí được scan trực tiếp từ prefix khi search.
type ShortcutData<T> = Vec<HashMap<T, HashSet<usize>>>;

/// (node_id, prefix, record, children) — used by node collection functions.
type NodeData<T> = (usize, Vec<T>, usize, Vec<usize>);

// ==================== Node Cache ====================

/// Dữ liệu cached cho một node: prefix (Vec<T>) + record.
/// Children được fetch lazy (chỉ khi cần DFS xuống children).
/// LRU cache đảm bảo memory bounded, không cần manual invalidation.
/// Dùng `Arc<Vec<T>>` để cache hit không clone prefix — chỉ tăng refcount.
#[derive(Clone)]
struct NodeCacheData<T> {
    prefix: Arc<Vec<T>>,
    record: usize,
}

// ==================== SearchIndex ====================

/// SearchIndex — cho phép tìm kiếm substring (LIKE) trên RadixTree.
///
/// Generic `T` là kiểu element trong key (u8, u16, u32, u64, etc.).
/// Mặc định `T = u8` cho backward compatibility với byte-based keys.
///
/// Có LRU-based node cache để tránh storage round-trips khi search.
/// Cache chỉ active cho non-InMemoryStorage (vd: RedisStorage).
pub struct SearchIndex<T: KeyElement = u8> {
    tree: RadixTree<T>,
    shortcuts: Arc<Mutex<ShortcutData<T>>>,

    /// Node cache: `Some` cho non-InMemoryStorage, `None` cho InMemory.
    /// Dùng `Arc<NodeCacheData<T>>` làm value để `get` không clone Vec.
    /// Cache: prefix + record (get_node).
    node_cache: Option<Arc<LruCache<usize, Arc<NodeCacheData<T>>, NODE_CACHE_SHARDS>>>,

    /// Children cache riêng: `Some` cho non-InMemoryStorage, `None` cho InMemory.
    /// children_ids KHÔNG được cache trong node_cache vì children thay đổi
    /// độc lập với prefix/record (khi split). Dùng cache riêng để dễ invalidate.
    children_cache: Option<Arc<LruCache<usize, Arc<Vec<usize>>, NODE_CACHE_SHARDS>>>,

    /// Bloom filters per node: unigrams + bigrams của toàn bộ subtree.
    /// Dùng để prune candidates trước DFS — giảm storage calls.
    #[cfg(feature = "bloom-search")]
    bloom_filters: HashMap<usize, BloomFilter>,

    /// Bloom filter chỉ prune candidates khi số candidates >= ngưỡng này.
    /// Mặc định: 50. Có thể config qua `set_bloom_candidates_threshold()`.
    #[cfg(feature = "bloom-search")]
    bloom_candidates_threshold: usize,
}

impl<T: KeyElement> SearchIndex<T> {
    // ── Constructor ──
    pub fn new<S: Storage + 'static>(sharding: usize, storage: S, cache_size: usize) -> Self {
        let sharding = sharding.max(1);
        let shortcuts = Arc::new(Mutex::new(
            (0..sharding)
                .map(|_| HashMap::<T, HashSet<usize>>::new())
                .collect::<Vec<_>>(),
        ));

        let mut tree = RadixTree::new(sharding, storage);

        // Cache enabled cho mọi storage — dùng Arc<Vec<u64>> để tránh clone.
        let node_cache = if cache_size > 0 {
            Some(Arc::new(LruCache::new(cache_size)))
        } else {
            None
        };
        let children_cache = if cache_size > 0 {
            Some(Arc::new(LruCache::new(cache_size)))
        } else {
            None
        };

        // Register split callback
        // 1. Thêm shortcuts cho từng u64 element trong leg prefix
        // 2. Xoá parent khỏi element nào không còn trong parent prefix sau split
        // 3. Invalidate node_cache + children_cache cho parent (prefix/children đã thay đổi)
            let cb_shortcuts = shortcuts.clone();
        let cb_cache = node_cache.clone();
        let cb_children = children_cache.clone();
        tree.with_callback(Arc::new(
            move |parent_id, leg_id, old_prefix, breakpoint| {
                let mut sc = match cb_shortcuts.lock() {
                    Ok(s) => s,
                    Err(_) => return Err(radixtree::RadixError::Callback),
                };

                let sharding = sc.len();

                // Elements thuộc về parent (before breakpoint) -> Xóa parent_id
                for (_, elem) in old_prefix.iter().enumerate().take(breakpoint) {
                    let si = radixtree::shard_of(*elem, sharding);
                    if let Some(elem_map) = sc[si].get_mut(elem) {
                        elem_map.remove(&parent_id);
                    }
                }

                // Elements thuộc về leg (at/after breakpoint) -> Thêm leg_id
                for (_, elem) in old_prefix.iter().enumerate().skip(breakpoint) {
                    let si = radixtree::shard_of(*elem, sharding);
                    let elem_map = sc[si].entry(*elem).or_default();
                    elem_map.remove(&parent_id);
                    elem_map.insert(leg_id);
                }

                // Invalidate node cache cho parent
                if let Some(ref cache) = cb_cache {
                    cache.remove(&parent_id);
                }
                // Invalidate children cache cho parent
                if let Some(ref cache) = cb_children {
                    cache.remove(&parent_id);
                }

                Ok(())
            },
        ));

        Self {
            tree,
            shortcuts,
            node_cache,
            children_cache,
            #[cfg(feature = "bloom-search")]
            bloom_filters: HashMap::new(),
            #[cfg(feature = "bloom-search")]
            bloom_candidates_threshold: BLOOM_DEFAULT_THRESHOLD,
        }
    }

    /// Convenience: `SearchIndex` in-storage.
    pub fn in_storage<S: Storage + 'static>(sharding: usize, storage: S) -> Self {
        Self::new(sharding, storage, NODE_CACHE_CAPACITY)
    }

    /// Convenience: `SearchIndex` in-memory.
    pub fn in_memory(sharding: usize) -> Self {
        Self::new(
            sharding,
            crate::storage::InMemoryStorage::default(),
            NODE_CACHE_CAPACITY,
        )
    }

    // ── Insert ──
    /// Thêm một entry vào index.
    ///
    /// - `key` — key để search (dạng T slice, VD: function call chain)
    /// - `entry_id` — ID của entry (VD: function_id)
    /// - `name` — tên hiển thị
    /// - `meta` — metadata tùy chọn (opaque bytes, VD: call-site info file/line)
    pub async fn insert(
        &mut self,
        key: &[T],
        entry_id: i32,
        name: &str,
        meta: Option<&[u8]>,
    ) -> Result<()> {
        if key.is_empty() {
            return Err(SearchError::NotFound);
        }

        // record trong RadixTree là 1-indexed (EMPTY = 0)
        let record_idx = self.next_record_idx().await?;

        // Ghi radix tree vào storage trước
        let (new_node_id, breakpoint) = self.tree.insert(key, record_idx).await?;

        // Nếu tree trả về EMPTY → key đã tồn tại, không tạo node/entry mới (ACID).
        if new_node_id == EMPTY {
            return Ok(());
        }

        // Persist entry (+ meta nếu có) xuống storage TRƯỚC khi update RAM (ACID commit pattern)
        // Nếu crash giữa save và RAM update, reload() sẽ phục hồi từ storage
        self.persist_entry(record_idx, entry_id, name).await?;
        if let Some(meta) = meta {
            self.tree.save_entry_meta(record_idx, meta).await?;
        }

        // Storage confirmed → now safe to update RAM (no local state)
        // ID was atomically allocated by storage (Redis INCR) — no race condition.

        // Cập nhật shortcuts cho node mới
        self.update_shortcuts(key, breakpoint, new_node_id);

        // Cập nhật bloom filters cho ancestors (nếu feature enabled)
        #[cfg(feature = "bloom-search")]
        {
            let blooms = &mut self.bloom_filters;
            Self::update_bloom_for_insert(&mut self.tree, blooms, key, new_node_id).await?;
        }

        Ok(())
    }

    /// Next record index — atomic allocation từ storage (Redis INCR).
    #[inline]
    async fn next_record_idx(&mut self) -> Result<usize> {
        // Uses storage allocation (Redis INCR) — atomic across all instances,
        // eliminating the race condition of a local record_counter.
        Ok(self.tree.allocate_record_id().await?)
    }

    /// Persist entry xuống storage trước khi update RAM.
    #[inline]
    async fn persist_entry(&mut self, record_idx: usize, entry_id: i32, name: &str) -> Result<()> {
        self.tree.save_entry(record_idx, entry_id, name).await?;
        Ok(())
    }

    /// Cập nhật shortcuts cho một node mới: thêm node_id vào set của từng element.
    fn update_shortcuts(&self, key: &[T], breakpoint: usize, node_id: usize) {
        if let Ok(mut shortcuts) = self.shortcuts.lock() {
            let sharding = shortcuts.len();
            for (_, elem) in key.iter().enumerate().skip(breakpoint) {
                let si = radixtree::shard_of(*elem, sharding);
                shortcuts[si].entry(*elem).or_default().insert(node_id);
            }
        }
    }

    // ── Bloom Filter (pruning) ──

    /// Cập nhật bloom filters cho ancestors khi insert key mới.
    /// Dùng `follow_path` để lấy ancestors từ root → leaf.
    ///
    /// Lưu bloom filters xuống storage để tránh rebuild khi reload.
    #[cfg(feature = "bloom-search")]
    async fn update_bloom_for_insert(
        tree: &mut RadixTree<T>,
        blooms: &mut HashMap<usize, BloomFilter>,
        key: &[T],
        new_node_id: usize,
    ) -> Result<()> {
        let features = extract_bloom_features(key);
        if features.is_empty() {
            return Ok(());
        }

        // Thêm features (bigrams + unigrams) cho ancestors
        if let Ok(ancestors) = tree.follow_path(key).await {
            for &aid in &ancestors {
                let bf = blooms
                    .entry(aid)
                    .or_insert_with(|| BloomFilter::new(BLOOM_M, BLOOM_K));
                for f in &features {
                    bf.insert(f);
                }
                // Persist bloom filter của ancestor xuống storage
                let blob = bf.serialize();
                let _ = tree.save_blob(&format!("bloom:{}", aid), &blob).await;
            }
        }

        // Thêm bloom cho chính node mới
        if new_node_id != EMPTY {
            let mut bf = BloomFilter::new(BLOOM_M, BLOOM_K);
            for f in &features {
                bf.insert(f);
            }
            // Persist bloom filter của node mới xuống storage (trước move)
            let blob = bf.serialize();
            let _ = tree
                .save_blob(&format!("bloom:{}", new_node_id), &blob)
                .await;
            blooms.insert(new_node_id, bf);
        }

        Ok(())
    }

    /// Set bloom candidates threshold.
    /// Bloom filter chỉ prune candidates khi số candidates >= ngưỡng này.
    /// Mặc định: 50. Set 0 = luôn bloom, set usize::MAX = không bao giờ bloom.
    #[cfg(feature = "bloom-search")]
    #[inline]
    pub fn set_bloom_candidates_threshold(&mut self, n: usize) {
        self.bloom_candidates_threshold = n;
    }

    // ── Search LIKE ──

    /// Tìm kiếm subsequence — entries có key chứa `pattern` (dạng T slice).
    ///
    /// Dùng KMP + DFS, với shortcut index để tìm candidate nodes.
    ///
    /// Trả về `Vec<(entry_id, name)>`.
    pub async fn search_like(&self, pattern: &[T], limit: usize) -> Result<Vec<(i32, String)>> {
        if pattern.is_empty() {
            return Err(SearchError::NotFound);
        }

        let lps = Self::preprocess_pattern(pattern);
        let first_elem = pattern[0];
        let sharding = self.tree.sharding_count();
        let si = radixtree::shard_of(first_elem, sharding);

        // Collect candidates upfront, drop lock before any .await
        let candidates: Vec<usize> = {
            let shortcuts = self
                .shortcuts
                .lock()
                .map_err(|e| SearchError::Storage(e.to_string()))?;

            shortcuts[si]
                .get(&first_elem)
                .map(|elem_set| elem_set.iter().copied().collect::<Vec<usize>>())
                .unwrap_or_default()
        };

        // Bloom pruning: filter candidates bằng bigram check trên encoded bytes.
        #[cfg(feature = "bloom-search")]
        let candidates = {
            let blooms = &self.bloom_filters;
            if candidates.len() >= self.bloom_candidates_threshold {
                let features = extract_bloom_features(pattern);
                if !features.is_empty() {
                    // Pre-hash tất cả features 1 lần duy nhất
                    let hashed_features: Vec<(u64, u64)> =
                        features.iter().map(|f| BloomFilter::hash128(f)).collect();

                    candidates
                        .into_iter()
                        .filter(|&node_id| {
                            blooms
                                .get(&node_id)
                                .map(|bf| {
                                    hashed_features
                                        .iter()
                                        .all(|&(h1, h2)| bf.contains_raw(h1, h2))
                                })
                                .unwrap_or(true)
                        })
                        .collect::<Vec<_>>()
                } else {
                    candidates
                }
            } else {
                candidates
            }
        };

        let mut results = Vec::new();
        let mut seen = HashSet::new();

        for &node_id in &candidates {
            if results.len() >= limit {
                break;
            }

            let found = self.dfs_search(node_id, pattern, &lps, 0, 0, limit).await?;

            for entry in found {
                if seen.insert(entry.0) {
                    results.push(entry);
                    if results.len() >= limit {
                        break;
                    }
                }
            }
        }

        if results.is_empty() {
            Err(SearchError::NotFound)
        } else {
            Ok(results)
        }
    }

    /// Tìm toàn bộ record có key bắt đầu bằng `prefix` — trả `(full_key, record)`
    /// trần từ RadixTree, KHÔNG load entry_id/name/meta.
    ///
    /// Nhanh hơn `search_prefix_full` 2 query/hit vì hot path (CallIndex traversal)
    /// chỉ cần key để tái dựng chain — record idx (1-indexed) là ID edge ổn định.
    /// NotFound → `Err(NotFound)` (giống `search_prefix_full`).
    pub async fn search_prefix(&self, prefix: &[T]) -> Result<Vec<(Vec<T>, usize)>> {
        let results = self.tree.search_prefix(prefix).await?;
        let mut out = Vec::with_capacity(results.len());
        for (key, record) in results {
            if record == EMPTY {
                continue;
            }
            out.push((key, record));
        }
        if out.is_empty() {
            Err(SearchError::NotFound)
        } else {
            Ok(out)
        }
    }

    /// Tìm toàn bộ entry có key bắt đầu bằng `prefix` — trả về đầy đủ
    /// `(full_key, entry_id, name, meta)` cho TỪNG record (KHÔNG dedup).
    ///
    /// Khác `search_like` (dedup theo entry_id) — mỗi key/leaf là một kết quả,
    /// nên dùng được để liệt kê edge theo per-key. `full_key` cho phép tái dựng
    /// chain (VD: key `[A,B]` → edge A→B).
    ///
    /// Dùng `radix::search_prefix` ở tầng RadixTree — không qua shortcuts.
    pub async fn search_prefix_full(
        &self,
        prefix: &[T],
    ) -> Result<Vec<(Vec<T>, i32, String, Option<Vec<u8>>)>> {
        let results = self.tree.search_prefix(prefix).await?;
        let mut out = Vec::with_capacity(results.len());
        for (key, record) in results {
            if record == EMPTY {
                continue;
            }
            let entry = self.tree.load_entry(record).await?;
            let meta = self.tree.load_entry_meta(record).await?;
            out.push((key, entry.0, entry.1, meta));
        }
        if out.is_empty() {
            Err(SearchError::NotFound)
        } else {
            Ok(out)
        }
    }

    // ── KMP: LPS array ──

    /// Build Longest Proper Prefix which is also Suffix (LPS) array.
    #[inline]
    fn preprocess_pattern(pattern: &[T]) -> Vec<usize> {
        let n = pattern.len();
        let mut lps = vec![0; n];
        let mut j = 0;
        for i in 1..n {
            while j > 0 && pattern[i] != pattern[j] {
                j = lps[j - 1];
            }
            if pattern[i] == pattern[j] {
                j += 1;
                lps[i] = j;
            }
        }
        lps
    }

    // ── DFS Search ──

    /// Load prefix + record, ưu tiên cache nếu active.
    /// Trả về `(Arc<Vec<T>>, usize)` — cache hit chỉ tăng refcount, không clone Vec.
    #[inline]
    async fn load_node_data(&self, node_id: usize) -> Result<(Arc<Vec<T>>, usize)> {
        if let Some(ref cache) = self.node_cache
            && let Some(data) = cache.get(&node_id)
        {
            return Ok((data.prefix.clone(), data.record));
        }

        let (prefix_bytes, record) = self.tree.get_node(node_id).await?;
        let prefix_vec = RadixTree::<T>::decode_to_vec(&prefix_bytes);

        if let Some(ref cache) = self.node_cache {
            let arc_prefix = Arc::new(prefix_vec);
            cache.put(
                node_id,
                Arc::new(NodeCacheData {
                    prefix: arc_prefix.clone(),
                    record,
                }),
            );
            Ok((arc_prefix, record))
        } else {
            Ok((Arc::new(prefix_vec), record))
        }
    }

    /// Load children IDs, ưu tiên cache nếu active.
    /// Dùng `children_cache` riêng (không chung với node_cache) vì
    /// children thay đổi độc lập với prefix/record khi split.
    #[inline]
    async fn load_node_children(&self, node_id: usize) -> Result<Arc<Vec<usize>>> {
        if let Some(ref cache) = self.children_cache
            && let Some(children) = cache.get(&node_id)
        {
            return Ok(children);
        }

        let children = Arc::new(self.tree.get_children_ids(node_id).await?);

        if let Some(ref cache) = self.children_cache {
            cache.put(node_id, children.clone());
        }

        Ok(children)
    }

    /// DFS + KMP: tìm pattern bắt đầu từ `(data_pos, pattern_pos)` trong
    /// subtree của `node_id`.
    #[inline]
    async fn dfs_search(
        &self,
        node_id: usize,
        pattern: &[T],
        lps: &[usize],
        pattern_pos: usize,
        data_pos: usize,
        limit: usize,
    ) -> Result<Vec<(i32, String)>> {
        let (prefix, _record) = self.load_node_data(node_id).await?;

        // Nếu phần còn lại của prefix (từ data_pos) ngắn hơn phần còn lại
        // của pattern → cần đệ quy xuống children
        let remaining = pattern.len().saturating_sub(pattern_pos);
        let effective_prefix_len = prefix.len().saturating_sub(data_pos);
        let do_recursive = effective_prefix_len < remaining;

        let (found, keep, _, new_pattern_pos) =
            Self::kmp_match(pattern, &prefix, lps, pattern_pos, data_pos, do_recursive);

        if found {
            // Match hoàn chỉnh → collect toàn bộ records trong subtree
            let mut records = Vec::new();
            self.collect_subtree_records(node_id, &mut records).await?;
            return self.resolve_records(&records, limit).await;
        }

        // Nếu match thất bại và ta đang bắt đầu fresh (pattern_pos == 0),
        // thử tất cả vị trí còn lại của pattern[0] trong cùng prefix.
        if !found && pattern_pos == 0 && (data_pos + 1) < prefix.len() {
            let mut scan_pos = data_pos + 1;
            while scan_pos < prefix.len() {
                if prefix[scan_pos] == pattern[0] {
                    let do_rec = (prefix.len() - scan_pos) < pattern.len();
                    let (f2, k2, _, pp2) =
                        Self::kmp_match(pattern, &prefix, lps, 0, scan_pos, do_rec);
                    if f2 {
                        let mut records = Vec::new();
                        self.collect_subtree_records(node_id, &mut records).await?;
                        return self.resolve_records(&records, limit).await;
                    }
                    // Partial match → DFS xuống children
                    if do_rec && k2 && pp2 < pattern.len() {
                        let next_elem = pattern[pp2];
                        let children = self.load_node_children(node_id).await?;
                        for &child in children.iter() {
                            let (cp, _) = self.load_node_data(child).await?;
                            if !cp.is_empty() && cp[0] == next_elem {
                                let f =
                                    Box::pin(self.dfs_search(child, pattern, lps, pp2, 0, limit))
                                        .await?;
                                if !f.is_empty() {
                                    return Ok(f);
                                }
                            }
                        }
                    }
                }
                scan_pos += 1;
            }
        }

        // Nếu còn có thể match tiếp và prefix đã hết → DFS xuống children
        if do_recursive && keep && new_pattern_pos < pattern.len() {
            let next_elem = pattern[new_pattern_pos];
            let children = self.load_node_children(node_id).await?;

            for &child in children.iter() {
                let (child_prefix, _) = self.load_node_data(child).await?;
                if !child_prefix.is_empty() && child_prefix[0] == next_elem {
                    let found =
                        Box::pin(self.dfs_search(child, pattern, lps, new_pattern_pos, 0, limit))
                            .await?;

                    if !found.is_empty() {
                        return Ok(found);
                    }
                }
            }
        }

        Ok(Vec::new())
    }

    // ── KMP Matching ──

    /// Chạy KMP trên một `data` slice (prefix của node — Vec<T>).
    ///
    /// Trả về `(found, keep, data_pos, pattern_pos)`:
    /// - `found`: tìm thấy pattern hoàn chỉnh trong data
    /// - `keep`: có tiến triển (partial match) — chỉ có ý nghĩa khi `!found && do_recursive`
    /// - `data_pos` / `pattern_pos`: trạng thái mới sau khi match
    #[inline]
    fn kmp_match(
        pattern: &[T],
        data: &[T],
        lps: &[usize],
        mut pattern_pos: usize,
        mut data_pos: usize,
        do_recursive: bool,
    ) -> (bool, bool, usize, usize) {
        let mut keep = false;

        while data_pos < data.len() {
            if data[data_pos] == pattern[pattern_pos] {
                keep = true;
                data_pos += 1;
                pattern_pos += 1;
            }

            if pattern_pos == pattern.len() {
                return (true, false, data_pos, pattern_pos);
            }

            if data_pos < data.len() && pattern[pattern_pos] != data[data_pos] {
                if !do_recursive {
                    return (false, false, data_pos, pattern_pos);
                }

                if pattern_pos != 0 {
                    pattern_pos = lps[pattern_pos - 1];
                } else {
                    data_pos += 1;
                    keep = false;
                }
            }
        }

        (false, keep, data_pos, pattern_pos)
    }

    // ── Helpers ──

    /// Collect toàn bộ record IDs trong subtree của `node_id` (DFS).
    /// Dùng `records: &mut Vec<usize>` accumulator để tránh tạo Vec mới
    /// ở mỗi cấp đệ quy.
    #[inline]
    async fn collect_subtree_records(
        &self,
        node_id: usize,
        records: &mut Vec<usize>,
    ) -> Result<()> {
        let (_prefix, record) = self.load_node_data(node_id).await?;
        if record != EMPTY {
            records.push(record);
        }

        let children = self.load_node_children(node_id).await?;
        for &child in children.iter() {
            Box::pin(self.collect_subtree_records(child, records)).await?;
        }

        Ok(())
    }

    /// Chuyển đổi record IDs (1-indexed) thành entries.
    /// Load từ storage (HSET — O(1)/entry).
    #[inline]
    async fn resolve_records(
        &self,
        record_ids: &[usize],
        limit: usize,
    ) -> Result<Vec<(i32, String)>> {
        let mut results = Vec::new();
        let mut seen = HashSet::new();
        for &rid in record_ids {
            if rid == EMPTY {
                continue;
            }
            // Skip entries that can't be loaded (e.g., tree has a node with this
            // record_idx but save_entry wasn't completed due to crash).
            // This makes search resilient to incomplete state.
            if let Ok(entry) = self.tree.load_entry(rid).await
                && seen.insert(entry.0)
            {
                results.push(entry);
                if results.len() >= limit {
                    break;
                }
            }
        }
        if results.is_empty() {
            Err(SearchError::NotFound)
        } else {
            Ok(results)
        }
    }

    // ==================== RELOAD (crash recovery / restart) ====================

    /// Reload toàn bộ state từ storage.
    /// Dùng sau crash hoặc restart để phục hồi:
    /// 1. endpoints (roots)
    /// 2. entries list / record counter
    /// 3. shortcuts
    pub async fn reload(&mut self) -> Result<()> {
        // 1. Reload endpoints từ storage
        self.tree.reload_endpoints().await?;

        // 2. Load entries từ storage (populates entries_cache) / restore record counter
        self.load_state_from_storage().await?;

        // 3. Rebuild shortcuts từ radix tree
        self.rebuild_all_shortcuts().await?;

        Ok(())
    }

    /// Mở bulk mode (transaction) — cắt chi phí autocommit per-write khi rebuild.
    /// Phải gọi `end_bulk()` sau đó để commit.
    pub async fn begin_bulk(&mut self) -> Result<()> {
        self.tree.begin_bulk().await?;
        Ok(())
    }

    /// Kết thúc bulk mode — commit transaction.
    pub async fn end_bulk(&mut self) -> Result<()> {
        self.tree.end_bulk().await?;
        Ok(())
    }

    /// Load entries từ storage (populates entries_cache) / restore record counter.
    async fn load_state_from_storage(&mut self) -> Result<()> {
        // Load entries — decompress zstd blob (or fallback to old Hash)
        // and populate entries_cache for fast search-time lookups.
        let entries = self.tree.load_entries_from_storage().await?;
        let count = entries.len();

        // Initialize storage's record counter (Redis: SET NX — only if not set).
        // This ensures the counter matches entry count without overwriting
        // a counter from another active instance sharing the same Redis.
        self.tree.init_record_counter(count).await?;
        Ok(())
    }

    /// Collect toàn bộ (node_id, prefix, record, children) từ tree.
    /// Dùng `get_node` để lấy prefix+record trong 1 storage call.
    #[inline]
    async fn collect_all_nodes(&self) -> Result<Vec<NodeData<T>>> {
        let mut nodes = Vec::new();
        for si in 0..self.tree.sharding_count() {
            let root_id = self.tree.get_storage_root(si).await?;
            if root_id == EMPTY {
                continue;
            }
            Box::pin(Self::collect_nodes_dfs(&self.tree, root_id, &mut nodes)).await?;
        }
        Ok(nodes)
    }

    /// DFS helper: collect (node_id, prefix, record, children) cho subtree.
    async fn collect_nodes_dfs(
        tree: &RadixTree<T>,
        node_id: usize,
        nodes: &mut Vec<NodeData<T>>,
    ) -> Result<()> {
        let (prefix, record) = tree.get_node_decoded(node_id).await?;
        let children = tree.get_children_ids(node_id).await?;
        nodes.push((node_id, prefix, record, children.clone()));
        for &child in &children {
            Box::pin(Self::collect_nodes_dfs(tree, child, nodes)).await?;
        }
        Ok(())
    }

    /// Xoá shortcuts cũ và rebuild từ toàn bộ radix tree.
    /// Đồng thời populate node cache để search không cần gọi storage.
    /// KHÔNG giữ lock qua .await — collect data trước, populate shortcuts sau.
    #[inline]
    async fn rebuild_all_shortcuts(&mut self) -> Result<()> {
        // Bước 1: Collect toàn bộ node data
        // Ưu tiên load từ shard compressed blob (RedisStorage), fallback DFS
        let nodes = self.load_nodes_fast().await?;

        // Bước 2: Populate shortcuts (lock ngắn, không await)
        {
            let sharding = self.tree.sharding_count();
            let mut shortcuts = self
                .shortcuts
                .lock()
                .map_err(|e| SearchError::Storage(e.to_string()))?;

            for map in shortcuts.iter_mut() {
                map.clear();
            }

            for (node_id, prefix, _record, _children) in &nodes {
                for &elem in prefix {
                    let si = radixtree::shard_of(elem, sharding);
                    shortcuts[si].entry(elem).or_default().insert(*node_id);
                }
            }
        } // lock released here

        // Bước 3: Populate node cache + children cache nếu active
        if let Some(ref cache) = self.node_cache {
            for (node_id, prefix, record, _children) in &nodes {
                cache.put(
                    *node_id,
                    Arc::new(NodeCacheData {
                        prefix: Arc::new(prefix.clone()),
                        record: *record,
                    }),
                );
            }
        }
        if let Some(ref cache) = self.children_cache {
            for (node_id, _prefix, _record, children) in &nodes {
                cache.put(*node_id, Arc::new(children.clone()));
            }
        }

        // Bước 4: Load bloom filters từ storage, fallback rebuild nếu chưa có
        #[cfg(feature = "bloom-search")]
        {
            self.bloom_filters = Self::load_or_rebuild_blooms(&mut self.tree, &nodes).await;
        }

        Ok(())
    }

    /// Load nodes từ shard compressed blob nếu có, fallback DFS collect.
    /// Sau khi DFS collect, persist shard blobs để lần sau load nhanh hơn.
    async fn load_nodes_fast(&mut self) -> Result<Vec<NodeData<T>>> {
        let sharding = self.tree.sharding_count();

        // Thử load từ shard blobs trước
        let mut nodes = Vec::new();
        let mut all_from_blob = true;

        for si in 0..sharding {
            let root_id = self.tree.get_storage_root(si).await?;
            if root_id == EMPTY {
                continue;
            }
            match self.tree.load_shard(si).await {
                Ok(Some(data)) => {
                    for node_id in 1..data.prefixes.len() {
                        let prefix = RadixTree::<T>::decode_to_vec(&data.prefixes[node_id]);
                        let record = data.records.get(node_id).copied().unwrap_or(0);
                        let children = data.children.get(node_id).cloned().unwrap_or_default();
                        nodes.push((node_id, prefix, record, children));
                    }
                }
                _ => {
                    all_from_blob = false;
                    break;
                }
            }
        }

        if all_from_blob {
            return Ok(nodes);
        }

        // Fallback: DFS collect qua storage
        nodes = self.collect_all_nodes().await?;

        // Persist shard blobs cho lần reload sau
        // Gom nodes theo shard dựa vào element đầu tiên của prefix
        let mut shard_data: Vec<Vec<NodeData<T>>> = vec![Vec::new(); sharding];
        for node in &nodes {
            let first = match node.1.first() {
                Some(&f) => f,
                None => continue, // sentinel
            };
            let si = radixtree::shard_of(first, sharding);
            shard_data[si].push(node.clone());
        }

        for (si, s_nodes) in shard_data.iter().enumerate() {
            if s_nodes.is_empty() {
                continue;
            }
            let max_id = s_nodes.iter().map(|(id, ..)| *id).max().unwrap_or(0);
            let mut prefixes = vec![Vec::new(); max_id + 1];
            let mut records = vec![0; max_id + 1];
            let mut children = vec![Vec::new(); max_id + 1];

            for (node_id, prefix, record, node_children) in s_nodes {
                prefixes[*node_id] = RadixTree::<T>::encode_key(prefix);
                records[*node_id] = *record;
                children[*node_id] = node_children.clone();
            }

            let data = crate::storage::ShardNodeData {
                prefixes,
                records,
                children,
            };
            // best-effort: không fail reload nếu save_shard lỗi
            let _ = self.tree.save_shard(si, &data).await;
        }

        Ok(nodes)
    }

    /// Load bloom filters từ storage.
    /// Nếu chưa có (first run sau upgrade), rebuild từ nodes và persist xuống storage.
    #[cfg(feature = "bloom-search")]
    async fn load_or_rebuild_blooms(
        tree: &mut RadixTree<T>,
        nodes: &[NodeData<T>],
    ) -> HashMap<usize, BloomFilter> {
        let mut blooms = HashMap::new();
        let mut all_loaded = true;

        for (node_id, _, _, _) in nodes {
            match tree.load_blob(&format!("bloom:{}", node_id)).await {
                Ok(Some(data)) => {
                    if let Some(bf) = BloomFilter::deserialize(&data) {
                        blooms.insert(*node_id, bf);
                    } else {
                        all_loaded = false;
                        break;
                    }
                }
                _ => {
                    all_loaded = false;
                    break;
                }
            }
        }

        if all_loaded && blooms.len() == nodes.len() {
            return blooms;
        }

        // Fallback: rebuild từ đầu và persist để lần sau không cần rebuild lại
        let blooms = Self::rebuild_bloom_from_nodes(nodes);
        // Persist từng bloom filter xuống storage (best-effort)
        for (node_id, bf) in &blooms {
            let _ = tree
                .save_blob(&format!("bloom:{}", node_id), &bf.serialize())
                .await;
        }
        blooms
    }

    /// Rebuild bloom filters từ danh sách nodes (DFS post-order).
    /// Mỗi node's bloom = unigrams + bigrams của prefix (encoded bytes) + boundary
    /// bigrams với children + union của children's blooms.
    #[cfg(feature = "bloom-search")]
    #[inline]
    fn rebuild_bloom_from_nodes(nodes: &[NodeData<T>]) -> HashMap<usize, BloomFilter> {
        use std::collections::HashMap as Map;

        // Build node_id → index mapping
        let mut node_to_idx: Map<usize, usize> = Map::new();
        for (i, (nid, _, _, _)) in nodes.iter().enumerate() {
            node_to_idx.insert(*nid, i);
        }

        let mut blooms: Map<usize, BloomFilter> = Map::new();

        // Post-order: process children before parents
        fn compute_postorder<T: KeyElement>(
            idx: usize,
            nodes: &[NodeData<T>],
            node_to_idx: &Map<usize, usize>,
            blooms: &mut Map<usize, BloomFilter>,
        ) -> BloomFilter {
            let (node_id, ref prefix, _record, ref children) = nodes[idx];

            if let Some(bf) = blooms.get(&node_id) {
                return bf.clone();
            }

            let mut bf = BloomFilter::new(BLOOM_M, BLOOM_K);

            // Unigrams + Bigrams từ prefix encoded bytes
            let encoded = RadixTree::<T>::encode_key(prefix);
            for &byte in encoded.iter() {
                bf.insert(&[byte]);
            }
            for i in 0..encoded.len().saturating_sub(1) {
                bf.insert(&encoded[i..i + 2]);
            }

            // Xử lý children trước (post-order)
            for &child_id in children {
                if let Some(&child_idx) = node_to_idx.get(&child_id) {
                    let child_prefix = &nodes[child_idx].1;

                    // Boundary bigram: last byte của encoded prefix + first byte của encoded child prefix
                    if !prefix.is_empty() && !child_prefix.is_empty() {
                        let parent_encoded = RadixTree::<T>::encode_key(prefix);
                        let child_encoded = RadixTree::<T>::encode_key(child_prefix);
                        let boundary = [parent_encoded[parent_encoded.len() - 1], child_encoded[0]];
                        bf.insert(&boundary);
                    }

                    let child_bloom = compute_postorder(child_idx, nodes, node_to_idx, blooms);
                    bf.union(&child_bloom);
                }
            }

            blooms.insert(node_id, bf.clone());
            bf
        }

        for i in 0..nodes.len() {
            let (node_id, _, _, _) = &nodes[i];
            if !blooms.contains_key(node_id) {
                compute_postorder(i, nodes, &node_to_idx, &mut blooms);
            }
        }

        blooms
    }
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_insert_and_search_like_simple() {
        let mut idx = SearchIndex::in_memory(4);
        idx.insert(b"hello", 1, "Hello").await.unwrap();
        idx.insert(b"world", 2, "World").await.unwrap();
        idx.insert(b"help", 3, "Help").await.unwrap();

        let results = idx.search_like(b"hel", 10).await.unwrap();
        assert_eq!(results.len(), 2, "should find 'hello' and 'help'");
        let ids: Vec<i32> = results.iter().map(|(id, _)| *id).collect();
        assert!(ids.contains(&1));
        assert!(ids.contains(&3));
    }

    #[tokio::test]
    async fn test_search_like_substring() {
        let mut idx = SearchIndex::in_memory(4);
        idx.insert(b"tiem vang", 1, "Tiệm Vàng").await.unwrap();
        idx.insert(b"tiem bac", 2, "Tiệm Bạc").await.unwrap();

        // Search "vang" — should find "tiem vang"
        let results = idx.search_like(b"vang", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 1);
    }

    #[tokio::test]
    async fn test_search_like_partial_match_through_split() {
        let mut idx = SearchIndex::in_memory(4);
        // Insert keys that share prefix → trigger split
        idx.insert(b"hello", 1, "Hello").await.unwrap();
        idx.insert(b"help", 2, "Help").await.unwrap();
        idx.insert(b"held", 3, "Held").await.unwrap();

        // Search "llo" — should find "hello" via DFS after split
        let results = idx.search_like(b"llo", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 1);
    }

    #[tokio::test]
    async fn test_search_like_not_found() {
        let mut idx = SearchIndex::in_memory(2);
        idx.insert(b"hello", 1, "Hello").await.unwrap();

        let result = idx.search_like(b"xyz", 10).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_search_like_empty_pattern() {
        let idx = SearchIndex::in_memory(2);
        assert!(idx.search_like(b"", 10).await.is_err());
    }

    #[tokio::test]
    async fn test_search_like_empty_index() {
        let idx = SearchIndex::in_memory(2);
        assert!(idx.search_like(b"anything", 10).await.is_err());
    }

    #[tokio::test]
    async fn test_search_prefix_raw_returns_records() {
        let mut idx = SearchIndex::in_memory(2);
        idx.insert_with_meta(&[1u64, 2], 12, "a", b"meta-12")
            .await
            .unwrap();
        idx.insert_with_meta(&[1u64, 3], 13, "b", b"meta-13")
            .await
            .unwrap();
        idx.insert_with_meta(&[2u64, 4], 24, "c", b"meta-24")
            .await
            .unwrap();

        // Raw: chỉ (key, record) — KHÔNG load entry_id/name/meta.
        let raw = idx.search_prefix(&[1u64]).await.unwrap();
        assert_eq!(raw.len(), 2);
        let keys: Vec<Vec<u64>> = raw.iter().map(|(k, _)| k.clone()).collect();
        assert!(keys.contains(&vec![1, 2]));
        assert!(keys.contains(&vec![1, 3]));
        // record idx là số dương (1-indexed) — ID edge ổn định.
        for (_, record) in &raw {
            assert_ne!(*record, 0);
        }

        // NotFound → Err
        assert!(idx.search_prefix(&[9u64]).await.is_err());

        // Full vẫn trả entry + meta (dùng cho enrich).
        let full = idx.search_prefix_full(&[1u64]).await.unwrap();
        assert_eq!(full.len(), 2);
        let with_meta: Vec<(Vec<u64>, i32, String, Option<Vec<u8>>)> = full
            .iter()
            .filter(|(_, id, _, _)| *id == 12)
            .cloned()
            .collect();
        assert_eq!(with_meta.len(), 1);
        assert_eq!(with_meta[0].2, "a");
        assert_eq!(with_meta[0].3, Some(b"meta-12".to_vec()));
    }

    #[tokio::test]
    async fn test_search_prefix_full_path_shape() {
        // Key nhiều hơn 2 phần tử (chain path) — scan ra toàn bộ subtree.
        let mut idx = SearchIndex::in_memory(2);
        idx.insert_with_meta(&[1u64, 2, 3], 1, "n1", b"m1")
            .await
            .unwrap();
        idx.insert_with_meta(&[1u64, 2, 4], 2, "n2", b"m2")
            .await
            .unwrap();
        idx.insert_with_meta(&[1u64, 5], 3, "n3", b"m3")
            .await
            .unwrap();

        let raw = idx.search_prefix(&[1u64]).await.unwrap();
        assert_eq!(
            raw.len(),
            3,
            "cả path 3 phần tử + edge 2 phần tử dưới prefix"
        );
        let keys: Vec<Vec<u64>> = raw.iter().map(|(k, _)| k.clone()).collect();
        assert!(keys.contains(&vec![1, 2, 3]));
        assert!(keys.contains(&vec![1, 2, 4]));
        assert!(keys.contains(&vec![1, 5]));

        let raw2 = idx.search_prefix(&[1u64, 2]).await.unwrap();
        assert_eq!(raw2.len(), 2);
    }

    #[tokio::test]
    async fn test_search_like_limit() {
        let mut idx = SearchIndex::in_memory(4);
        for i in 0..10 {
            let name = format!("Item {i}");
            idx.insert(format!("item_{i}").as_bytes(), i, &name)
                .await
                .unwrap();
        }

        // Search "item" — tất cả 10 đều match, nhưng limit=3
        let results = idx.search_like(b"item", 3).await.unwrap();
        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn test_search_like_with_unicode_bytes() {
        let mut idx = SearchIndex::in_memory(4);
        // "Hà Nội" in UTF-8
        let ha_noi = "Hà Nội".as_bytes();
        let sai_gon = "Sài Gòn".as_bytes();

        idx.insert(ha_noi, 1, "Hà Nội").await.unwrap();
        idx.insert(sai_gon, 2, "Sài Gòn").await.unwrap();

        // Search "Nội"
        let results = idx.search_like("Nội".as_bytes(), 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 1);
    }

    #[tokio::test]
    async fn test_search_like_single_character() {
        let mut idx = SearchIndex::in_memory(4);
        idx.insert(b"aaaa", 1, "Aaaa").await.unwrap();
        idx.insert(b"bbbb", 2, "Bbbb").await.unwrap();

        let results = idx.search_like(b"a", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 1);
    }

    #[tokio::test]
    async fn test_insert_duplicate_key() {
        let mut idx = SearchIndex::in_memory(4);
        idx.insert(b"hello", 1, "Hello").await.unwrap();

        // Insert cùng key lần nữa — RadixTree trả về (EMPTY, tail)
        // vì key đã tồn tại. SearchIndex KHÔNG append entries.
        let res = idx.insert(b"hello", 2, "Hello Again").await;
        assert!(res.is_ok(), "duplicate insert không lỗi");

        // search_like vẫn trả về entry cũ (record=1)
        let results = idx.search_like(b"hello", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], (1, "Hello".to_string()));
    }

    #[tokio::test]
    async fn test_search_like_no_dup_results() {
        let mut idx = SearchIndex::in_memory(4);
        // Insert two keys that share a subtree
        idx.insert(b"hello world", 1, "Hello World").await.unwrap();
        idx.insert(b"hello", 2, "Hello").await.unwrap();

        // Search "hello" — both entries should appear (no duplicates)
        let results = idx.search_like(b"hello", 10).await.unwrap();
        assert_eq!(results.len(), 2);
        let ids: Vec<i32> = results.iter().map(|(id, _)| *id).collect();
        assert!(ids.contains(&1));
        assert!(ids.contains(&2));
    }

    #[tokio::test]
    async fn test_search_like_kmp_partial_at_end() {
        // KMP edge case: pattern partially matches at the end of the prefix,
        // then continues in child node
        let mut idx = SearchIndex::in_memory(4);
        // "abcde" stored with root prefix "abcd" and child prefix "e"
        // After insert "abcd" and "abcde", the tree might split
        idx.insert(b"abcd", 1, "ABCD").await.unwrap();
        idx.insert(b"abcde", 2, "ABCDE").await.unwrap();

        // Search "cde" — should find ABCDE via DFS
        let results = idx.search_like(b"cde", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 2);
    }

    // ==================== Benchmarks ====================

    #[tokio::test]
    async fn bench_search_like_bulk() {
        let mut idx = SearchIndex::in_memory(8);
        let store_names = [
            "Tiệm Vàng Hoàng Phát",
            "Tiệm Vàng Minh Châu",
            "Tiệm Vàng Bảo Tín",
            "Vàng Bạc Đá Quý Sài Gòn",
            "PNJ - Vàng Bạc Đá Quý",
            "DOJI - Trang Sức Cao Cấp",
            "Tiệm Vàng Kim Thành",
            "Vàng 9999 - Nguyên Liệu",
            "Tiệm Vàng Hồng Phát",
            "Vàng Mi Hồng - Quận 3",
            "Tiệm Vàng Phú Nhuận",
            "SJC - Công Ty Vàng Bạc Đá Quý",
            "Tiệm Vàng Ngọc Thạch",
            "Bảo Tín Minh Châu",
            "Vàng Thế Giới - Gold Price",
            "Tiệm Vàng An Phát",
            "Vàng 24K - Nữ Trang",
            "Tiệm Vàng Hồng Đức",
            "Vàng Mi Hồng - Cơ Sở 2",
            "Tiệm Vàng Bảo Tín Mạnh Hải",
        ];

        // Insert 100 entries (lặp lại 5 lần với tên khác nhau)
        for i in 0..100 {
            let name = store_names[i % store_names.len()];
            let key = format!("{name} - {i}");
            idx.insert(key.as_bytes(), i as i32, name).await.unwrap();
        }

        // Warmup
        let _ = idx.search_like("Vàng".as_bytes(), 10).await;

        // Benchmark prefix search
        let patterns: &[&[u8]] = &[
            "Vàng".as_bytes(),
            "Tiệm".as_bytes(),
            b"PNJ",
            b"SJC",
            "Bảo Tín".as_bytes(),
            b"9999",
        ];

        let start = std::time::Instant::now();
        let iterations = 50;
        for _ in 0..iterations {
            for pat in patterns {
                let _ = idx.search_like(pat, 10).await;
            }
        }
        let elapsed = start.elapsed();
        let avg_ns = elapsed.as_nanos() as f64 / (iterations * patterns.len()) as f64;

        eprintln!(
            "[bench] search_like bulk: {:.0} ns/call ({} iterations, {} patterns)",
            avg_ns,
            iterations,
            patterns.len()
        );

        // Verify correctness
        let results = idx.search_like("Vàng".as_bytes(), 10).await.unwrap();
        assert!(!results.is_empty());
        assert!(results.len() <= 10);
    }

    #[tokio::test]
    async fn bench_search_like_short_pattern() {
        let mut idx = SearchIndex::in_memory(8);
        let names = [
            "apple",
            "apricot",
            "banana",
            "cherry",
            "date",
            "elderberry",
            "fig",
            "grape",
        ];

        for i in 0..200 {
            let name = names[i % names.len()];
            let key = format!("{name}_{i}");
            idx.insert(key.as_bytes(), i as i32, name).await.unwrap();
        }

        // Single-character pattern (worst case — nhiều candidates)
        let start = std::time::Instant::now();
        for _ in 0..100 {
            let _ = idx.search_like(b"a", 5).await;
        }
        let elapsed = start.elapsed();
        let avg_ns = elapsed.as_nanos() as f64 / 100.0;

        eprintln!("[bench] search_like single-char: {:.0} ns/call", avg_ns);

        // Two-character pattern
        let start = std::time::Instant::now();
        for _ in 0..100 {
            let _ = idx.search_like(b"ap", 5).await;
        }
        let elapsed = start.elapsed();
        let avg_ns = elapsed.as_nanos() as f64 / 100.0;

        eprintln!("[bench] search_like two-char: {:.0} ns/call", avg_ns);
    }

    #[tokio::test]
    async fn bench_search_like_not_found() {
        let mut idx = SearchIndex::in_memory(4);
        for i in 0..100 {
            let key = format!("store_{i}");
            idx.insert(key.as_bytes(), i, &key).await.unwrap();
        }

        // Pattern không tồn tại — đo tốc độ fail fast
        let start = std::time::Instant::now();
        for _ in 0..50 {
            let _ = idx.search_like(b"zzzzz", 10).await;
        }
        let elapsed = start.elapsed();
        let avg_ns = elapsed.as_nanos() as f64 / 50.0;

        eprintln!("[bench] search_like not-found: {:.0} ns/call", avg_ns);
    }

    #[tokio::test]
    async fn test_search_like_false_negative_case_abaa() {
        let mut idx = SearchIndex::in_memory(4);

        // Chèn chuỗi chứa prefix đặc biệt "abaa"
        // Giả sử RadixTree lưu nguyên cụm này thành 1 node prefix hoặc bị split
        idx.insert(b"abaadata", 1, "Target Node abaa")
            .await
            .unwrap();

        // Tìm kiếm "aa"
        // - Vị trí đầu tiên của 'a' là index 0 -> bắt đầu khớp 'a', gặp 'b' -> FAIL.
        // - Nếu lưu mọi vị trí, shortcut sẽ thử tiếp index 2 (chữ 'a' đầu của cặp "aa") -> SUCCESS.
        let results = idx.search_like(b"aa", 10).await;

        assert!(
            results.is_ok(),
            "False negative! Bản cũ chỉ lưu vị trí 'a' đầu tiên nên không bao giờ quét tới cặp 'aa' phía sau."
        );

        let res = results.unwrap();
        assert_eq!(res.len(), 1);
    }

    #[tokio::test]
    async fn test_search_like_multiple_positions_in_single_prefix() {
        let mut idx = SearchIndex::in_memory(4);

        // Chuỗi có ký tự đầu tiên 'a' lặp lại liên tục ở nhiều cụm khác nhau
        idx.insert(b"xyz_ab_ab_ab", 1, "Repeated Pattern")
            .await
            .unwrap();

        // Tìm kiếm "ab"
        let results = idx.search_like(b"ab", 10).await.unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn test_search_like_overlapping_candidates() {
        let mut idx = SearchIndex::in_memory(4);

        // Khớp chồng lấn (Overlapping)
        idx.insert(b"aaaaa", 1, "Five A").await.unwrap();

        // Tìm kiếm "aaa"
        let results = idx.search_like(b"aaa", 10).await.unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn test_search_like_split_retains_all_valid_positions() {
        let mut idx = SearchIndex::in_memory(4);

        // Tạo một node dài chứa nhiều ký tự 'a'
        idx.insert(b"test_abaadata_one", 1, "First").await.unwrap();

        // Kích hoạt split tại vị trí "test_" bằng cách chèn key chung prefix
        // Callback OnSplit phải giữ lại chính xác các vị trí tương đối (rel_pos) của 'a' ở node leg phía sau
        idx.insert(b"test_other_route", 2, "Second").await.unwrap();

        // Kiểm tra xem sau khi split, các shortcut 'a' ở leg node vẫn tìm được "aa" hay không
        let results = idx.search_like(b"aa", 10).await.unwrap();
        assert_eq!(results.len(), 1);
    }

    // ── Edge case: retry từ vị trí mà KMP đã match nhưng không phải start —

    #[tokio::test]
    async fn test_retry_from_within_kmp_matched_bytes() {
        // pattern "aab", key "aaab".
        // KMP từ data_pos=0: match 'a'=p0, 'a'=p1, fail 'a'≠'b' (p2).
        // new_data_pos=2. data_pos+1=1 → 'a' ở 1 → start tại 1 → FOUND.
        let mut idx = SearchIndex::in_memory(4);
        idx.insert(b"aaabyz", 1, "Target").await.unwrap();
        let results = idx.search_like(b"aab", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 1);
    }

    #[tokio::test]
    async fn test_retry_cascade_across_dfs_boundary() {
        // pattern "abc", keys: "xaa" + "xaabcde"
        // Tree: Node_A "xaa", Child "bcde"
        // shortcut['a'] = {A}. 'a' ở A[1] và A[2].
        // - data_pos=1: KMP keep=true, DFS không match vì 'b' ở child → Vec::new()
        // - data_pos=2: KMP keep=true, DFS match vì 'b' ở child → FOUND
        // Retry loop KHÔNG return ngay nếu data_pos=1 rỗng → thử data_pos=2 → OK.
        let mut idx = SearchIndex::in_memory(4);
        idx.insert(b"xaa", 1, "First").await.unwrap();
        idx.insert(b"xaabcde", 2, "Target").await.unwrap();

        let results = idx.search_like(b"abc", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 2);
    }

    #[tokio::test]
    async fn test_retry_cascade_all_empty() {
        // pattern "abc" nhưng KHÔNG có trong tree → retry hết mọi vị trí đều rỗng
        let mut idx = SearchIndex::in_memory(4);
        idx.insert(b"xaa", 1, "First").await.unwrap();
        idx.insert(b"xaaxyzw", 2, "Other").await.unwrap();

        let result = idx.search_like(b"abc", 10).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_span_three_nodes() {
        // Tree: "ab" + "cde" + "f"  (keys "abcde" + "abcdef")
        // Insert "ab" → Node_A = "ab"
        // Insert "abcde" → split A: "ab" + "cde"
        // Insert "abcdef" → thêm child "f" dưới "cde"
        // Pattern "bcdef" trải A + B + C
        let mut idx = SearchIndex::in_memory(4);
        idx.insert(b"ab", 1, "AB").await.unwrap();
        idx.insert(b"abcde", 2, "ABCDE").await.unwrap();
        idx.insert(b"abcdef", 3, "ABCDEF").await.unwrap();

        let results = idx.search_like(b"bcdef", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 3);
    }

    #[tokio::test]
    async fn test_partial_match_exhausts_prefix_then_child() {
        // Prefix đủ dài tính toán (do_recursive=false) nhưng KMP match hết prefix
        // cần tiếp tục ở child
        // Tree như test 3 node ở trên
        let mut idx = SearchIndex::in_memory(4);
        idx.insert(b"ab", 1, "AB").await.unwrap();
        idx.insert(b"abcde", 2, "ABCDE").await.unwrap();
        idx.insert(b"abcdef", 3, "ABCDEF").await.unwrap();

        // "cdef" bắt đầu từ vị trí 2 ở A, match 'c','d','e' hết prefix B,
        // cần 'f' ở C
        let results = idx.search_like(b"cdef", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 3);
    }
}
