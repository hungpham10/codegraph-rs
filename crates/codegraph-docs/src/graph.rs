use crate::config::DocConfig;
use crate::intern::Interner;
use crate::ir::{Document, Kind, Node, Scalar};
use crate::tokenize::{DocTag, DocToken};
use anyhow::Result;
use codegraph_graph::Search;
use codegraph_graph::Storage;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::RwLock as TokioRwLock;

/// Record-id bases so the same `node_id` can appear in several tries
/// without colliding on the storage key.
const PATH_RECORD_BASE: u64 = 100_000_000_000;
const TYPE_RECORD_BASE: u64 = 200_000_000_000;
const VALUE_RECORD_BASE: u64 = 300_000_000_000;
const STRUCT_RECORD_BASE: u64 = 400_000_000_000;
const PATTERN_RECORD_BASE: u64 = 500_000_000_000;
/// Dải riêng cho doc id — node id và doc id phải không đè nhau (hydrate/
/// storage key dùng chung namespace `set_node_meta`). Node id ≥ `doc_base`
/// (~1e9), pattern id ở dải 5e11, doc id ở dải này.
const DOC_ID_BASE: u64 = 600_000_000_000;

/// Sentinel storage keys for persisted node/doc lists (node ids are u64
/// that never reach these small constants because real ids start at
/// `DOC_BASE` ≈ 1e9).
const DOC_NODE_LIST_RECORD: u64 = 0;
const DOC_LIST_RECORD: u64 = 1;
const DOC_META_BASE: u64 = 10_000_000_000;
/// Sentinel cho interner (mảng string JSON theo thứ tự id). Doc id nằm ở
/// dải ≥ DOC_ID_BASE nên các slot nhỏ này không đụng doc metadata.
const DOC_INTERNER_RECORD: u64 = DOC_META_BASE + 1;
/// Sentinel cho pattern registry (JSON) — pattern id P# giữ ổn định qua
/// các lần mine và qua restart.
const DOC_PATTERNS_RECORD: u64 = DOC_META_BASE + 2;

/// Default sharding for document tries (mirrors code graph).
const DEFAULT_SHARDING: usize = 64;

/// Global graph of structured documents.
///
/// * `Document` IR is the source of truth for node content.
/// * `Search<DocToken>` tries are materialized projections (path/type/value/struct).
/// * Bloom/DFS/KMP/Radix are reused from `codegraph-graph` without changes.
pub struct DocumentGraph {
    storage: Arc<TokioRwLock<dyn Storage>>,
    docs: HashMap<u64, Document>,
    /// Cache node đọc theo nhu cầu (`node()`) — KHÔNG load sẵn toàn bộ khi
    /// open. Mutex (không tokio) vì chỉ giữ trong RAM, không span await.
    nodes: std::sync::Mutex<HashMap<u64, Node>>,
    intern: Interner,
    path_trie: Search<DocToken>,
    type_trie: Search<DocToken>,
    value_trie: Search<DocToken>,
    struct_trie: Search<DocToken>,
    /// Pattern-mining trie — index các structural pattern đã mine (leaf lưu
    /// `PATTERN_RECORD_BASE + pattern_id`).
    pattern_trie: Search<DocToken>,
    /// Registry các pattern đã mine — pattern id (P#) ổn định qua các lần
    /// mine và qua restart. Persist ở `DOC_PATTERNS_RECORD`.
    patterns: std::sync::Mutex<PatternRegistry>,
    /// Base id global cho node/doc — id nhỏ hơn đây là id local của parser.
    doc_base: u64,
    next_doc_id: u64,
    next_node_id: u64,
}

impl DocumentGraph {
    /// Create a new in-memory document graph backed by `storage` for the
    /// persistent tries. `config` controls id bases and bloom cap.
    pub fn new(storage: Arc<TokioRwLock<dyn Storage>>, config: DocConfig) -> Self {
        let doc_base = config.doc_base();
        let sharding = DEFAULT_SHARDING;
        Self {
            storage: storage.clone(),
            docs: HashMap::new(),
            nodes: std::sync::Mutex::new(HashMap::new()),
            intern: Interner::new(),
            // 4 trie projection dùng CHUNG một storage — radix lưu root/shortcut
            // theo shard index (0..sharding) nên mỗi trie phải ở một dải shard
            // riêng (bias * sharding), nếu không root pointer ghi đè lẫn nhau
            // và search chỉ thấy trie insert sau cùng.
            path_trie: Search::with_shard_bias(sharding, storage.clone(), 0),
            type_trie: Search::with_shard_bias(sharding, storage.clone(), 1),
            value_trie: Search::with_shard_bias(sharding, storage.clone(), 2),
            struct_trie: Search::with_shard_bias(sharding, storage.clone(), 3),
            pattern_trie: Search::with_shard_bias(sharding, storage.clone(), 4),
            patterns: std::sync::Mutex::new(PatternRegistry::default()),
            doc_base,
            next_doc_id: DOC_ID_BASE,
            next_node_id: doc_base,
        }
    }

    /// Open an existing graph from persistent storage — **lazy**: chỉ load
    /// doc list + doc metadata (số lượng file, nhỏ) và resume id counters.
    /// KHÔNG materialize toàn bộ node metadata — 186k nodes ở repo document
    /// lớn làm open chờ hàng chục giây. Node được đọc **theo nhu cầu** từng
    /// cái (`node()` — hydrate/collect_path), có cache LRU ở storage layer
    /// (`CachedStorage`) và cache in-memory trong `self.nodes`.
    pub async fn open(storage: Arc<TokioRwLock<dyn Storage>>, config: DocConfig) -> Result<Self> {
        let mut graph = Self::new(storage, config);
        // Load docs list + metadata (theo doc, không theo node).
        let doc_ids = graph.load_doc_list().await?;
        for id in doc_ids {
            let bytes = {
                let guard = graph.storage.read().await;
                guard.get_node_meta((DOC_META_BASE + id) as usize).await?
            };
            if let Some(bytes) = bytes
                && let Ok(doc) = serde_json::from_slice::<Document>(&bytes)
            {
                graph.docs.insert(doc.id, doc);
            }
        }
        // Resume id counters từ trạng thái đã persist — reset về `doc_base`
        // sẽ đè lên id cũ khi ingest tiếp. next_node_id lấy từ max node id
        // trong chain (1 lần đọc chain id, không đọc từng meta).
        let max_doc = graph.docs.keys().copied().max().unwrap_or(0);
        let max_node = {
            let guard = graph.storage.read().await;
            guard
                .get_chain(DOC_NODE_LIST_RECORD as usize)
                .await?
                .map(|c| c.iter().copied().max().unwrap_or(0))
                .unwrap_or(0)
        };
        graph.next_doc_id = graph.next_doc_id.max(max_doc + 1);
        graph.next_node_id = graph.next_node_id.max(max_node + 1);
        // Interner chỉ sống trong RAM — persist kèm mỗi upsert, restore tại
        // đây để id trong tries persist vẫn resolve được. Thiếu blob (graph
        // cũ) → dựng lại từ doc metadata theo thứ tự doc id (không clear trie
        // — Search::clear xoá cả namespace dùng chung của storage).
        if !graph.restore_interner().await? {
            graph.rebuild_interner_from_docs();
        }
        graph.restore_patterns().await?;
        graph.materialize_node_cache();
        Ok(graph)
    }

    /// Load interner đã persist. Trả `false` nếu chưa có blob (graph cũ).
    async fn restore_interner(&mut self) -> Result<bool> {
        let bytes = {
            let guard = self.storage.read().await;
            guard.get_node_meta(DOC_INTERNER_RECORD as usize).await?
        };
        let Some(bytes) = bytes else {
            return Ok(false);
        };
        if bytes.is_empty() {
            return Ok(false);
        }
        let strings: Vec<String> = serde_json::from_slice(&bytes)
            .map_err(|e| anyhow::anyhow!("corrupt interner blob: {e}"))?;
        self.intern = Interner::with_strings(strings);
        Ok(true)
    }

    /// Dựng interner từ keys + scalar values của các doc đã load (theo thứ tự
    /// doc id — trùng thứ tự intern lúc ingest tuần tự).
    fn rebuild_interner_from_docs(&mut self) {
        self.intern = Interner::new();
        let mut docs: Vec<&Document> = self.docs.values().collect();
        docs.sort_by_key(|d| d.id);
        for doc in docs {
            for node in &doc.nodes {
                if let Some(k) = &node.key {
                    self.intern.intern(k.clone());
                }
                if let Some(Scalar::String(s)) = &node.value {
                    self.intern.intern(s.clone());
                }
            }
        }
    }

    /// Persist interner — gọi sau mỗi upsert để lần `open()` sau vẫn khớp
    /// token payload đã ghi vào tries.
    async fn persist_interner(&self) -> Result<()> {
        let blob =
            serde_json::to_vec(&self.intern.strings()).map_err(|e| anyhow::anyhow!("{e}"))?;
        self.storage
            .write()
            .await
            .set_node_meta(DOC_INTERNER_RECORD as usize, &blob)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }

    /// Materialize node cache từ doc metadata (Document serialize đủ nodes) —
    /// hydrate/path_tokens/search_value đọc từ cache trước storage.
    fn materialize_node_cache(&self) {
        let mut cache = self.nodes.lock().unwrap();
        cache.clear();
        for doc in self.docs.values() {
            for node in &doc.nodes {
                cache.insert(node.id, node.clone());
            }
        }
    }

    /// Ingest một file từ disk: đọc, detect format theo extension (override
    /// bằng `format`), parse rồi upsert. Trùng `path` với doc đã có → thay thế
    /// tại chỗ (re-ingest khi chạy lại `codegraph init` là idempotent).
    pub async fn ingest_file(&mut self, path: &str, format: Option<&str>) -> Result<u64> {
        let source = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read {path}: {e}"))?;
        let format = match format {
            Some(f) => f.to_string(),
            None => crate::parsers::detect_format(path)?,
        };
        let existing = self.docs.values().find(|d| d.path == path).map(|d| d.id);
        let doc_id = existing.unwrap_or(0);
        let parser = crate::parsers::parser_for(&format)?;
        let doc = parser.parse(path, &source, doc_id)?;
        self.upsert_document(doc).await
    }

    /// Ingest a document, replacing any previous version with the same id.
    pub async fn upsert_document(&mut self, mut doc: Document) -> Result<u64> {
        if let Some(old) = self.docs.get(&doc.id) {
            self.remove_document_nodes(old).await?;
        }
        let doc_id = if doc.id == 0 {
            let id = self.next_doc_id;
            self.next_doc_id += 1;
            doc.id = id;
            id
        } else {
            doc.id
        };
        // Ensure nodes have global ids and wire parent/children.
        let doc = self.assign_node_ids(doc);
        // Persist nodes and doc metadata.
        for node in &doc.nodes {
            self.storage
                .write()
                .await
                .set_node_meta(node.id as usize, &serde_json::to_vec(node)?)
                .await?;
        }
        self.storage
            .write()
            .await
            .set_node_meta(
                (DOC_META_BASE + doc_id) as usize,
                &serde_json::to_vec(&doc)?,
            )
            .await?;
        // Update lists.
        self.add_doc_id(doc_id).await?;
        let node_ids: Vec<u64> = doc.nodes.iter().map(|n| n.id).collect();
        self.add_node_ids(&node_ids).await?;
        // Materialize nodes vào cache TRƯỚC khi tokenize — `path_tokens`/
        // `type_tokens` đi lên tổ tiên qua cache, cache thiếu thì path chain
        // chỉ còn `[root]` (bug gốc: tries insert trước, cache sau).
        {
            let mut cache = self.nodes.lock().unwrap();
            for node in &doc.nodes {
                cache.insert(node.id, node.clone());
            }
        }
        // Insert into tries.
        for node in &doc.nodes {
            self.insert_node_into_tries(node).await?;
        }
        self.docs.insert(doc_id, doc.clone());
        self.persist_interner().await?;
        Ok(doc_id)
    }

    /// Remove a document and its subtree from the graph and tries.
    pub async fn remove_document(&mut self, doc_id: u64) -> Result<()> {
        if let Some(doc) = self.docs.remove(&doc_id) {
            self.remove_document_nodes(&doc).await?;
            // Remove persisted metadata.
            self.storage
                .write()
                .await
                .set_node_meta((DOC_META_BASE + doc_id) as usize, &[])
                .await?;
        }
        Ok(())
    }

    /// Đọc một node theo nhu cầu: cache in-memory trước, thiếu thì xuống
    /// storage (`CachedStorage` LRU ở giữa). Trả `None` nếu id không tồn tại.
    async fn node(&self, node_id: u64) -> Option<Node> {
        if let Some(n) = self.nodes.lock().unwrap().get(&node_id) {
            return Some(n.clone());
        }
        let bytes = {
            let guard = self.storage.read().await;
            guard.get_node_meta(node_id as usize).await.ok().flatten()?
        };
        if bytes.is_empty() {
            return None; // meta đã bị clear (node removed).
        }
        let node = serde_json::from_slice::<Node>(&bytes).ok()?;
        self.nodes.lock().unwrap().insert(node_id, node.clone());
        Some(node)
    }

    /// Return the document owning `node_id`, if any.
    pub async fn doc_of(&self, node_id: u64) -> Option<&Document> {
        let node = self.node(node_id).await?;
        self.docs.get(&node.doc)
    }

    /// Hydrate a node into a small payload suitable for LLM reasoning.
    /// Đọc node + tổ tiên (cho path) + con theo nhu cầu từ storage.
    pub async fn hydrate(&self, node_id: u64) -> Option<NodePayload> {
        self.hydrate_depth(node_id, None).await
    }

    /// Như `hydrate` nhưng giới hạn số tầng con đi xuống (`max_depth = Some(2)`
    /// là payload 2 tầng — giữ payload nhỏ cho LLM trên doc lớn).
    pub async fn hydrate_depth(
        &self,
        node_id: u64,
        max_depth: Option<usize>,
    ) -> Option<NodePayload> {
        self.hydrate_inner(node_id, max_depth, 0).await
    }

    fn hydrate_inner(
        &self,
        node_id: u64,
        max_depth: Option<usize>,
        level: usize,
    ) -> Pin<Box<dyn Future<Output = Option<NodePayload>> + Send + '_>> {
        Box::pin(async move {
            let node = self.node(node_id).await?;
            let path = self.collect_path(node_id).await;
            let mut children = Vec::new();
            let descend = max_depth.is_none_or(|d| level < d);
            if descend {
                for c in &node.children {
                    if let Some(payload) = self.hydrate_inner(*c, max_depth, level + 1).await {
                        children.push(payload);
                    }
                }
            }
            Some(NodePayload {
                id: node.id,
                path,
                kind: node.kind,
                value: node.value.clone(),
                key: node.key.clone(),
                index: node.index,
                doc: node.doc,
                children,
            })
        })
    }

    // ── Query pipeline (reuses Search::search_resumable) ──────────────

    pub async fn search_path(
        &self,
        pattern: &[DocToken],
        depth: Option<usize>,
    ) -> Result<Vec<u64>> {
        self.search_trie(&self.path_trie, pattern, depth).await
    }
    pub async fn search_type(
        &self,
        pattern: &[DocToken],
        depth: Option<usize>,
    ) -> Result<Vec<u64>> {
        self.search_trie(&self.type_trie, pattern, depth).await
    }
    pub async fn search_value(
        &self,
        pattern: &[DocToken],
        depth: Option<usize>,
    ) -> Result<Vec<u64>> {
        self.search_trie(&self.value_trie, pattern, depth).await
    }
    pub async fn search_struct(
        &self,
        pattern: &[DocToken],
        depth: Option<usize>,
    ) -> Result<Vec<u64>> {
        self.search_trie(&self.struct_trie, pattern, depth).await
    }

    async fn search_trie(
        &self,
        trie: &Search<DocToken>,
        pattern: &[DocToken],
        depth: Option<usize>,
    ) -> Result<Vec<u64>> {
        let pages = trie.search(pattern, depth).await?;
        let mut ids = Vec::new();
        for (record, _meta) in pages {
            if let Some(node_id) = self.decode_record(record) {
                ids.push(node_id);
            }
        }
        Ok(ids)
    }

    // ── Doc listing / value lookup (dùng bởi MCP + CLI) ───────────────

    /// Liệt kê các document đã ingest kèm metadata (path, format, root, số node).
    pub fn list_docs(&self) -> Vec<DocInfo> {
        let mut infos: Vec<DocInfo> = self
            .docs
            .values()
            .map(|d| DocInfo {
                doc_id: d.id,
                path: d.path.clone(),
                format: d.format.clone(),
                root_node_id: d.root,
                nodes: d.nodes.len(),
            })
            .collect();
        infos.sort_by_key(|i| i.doc_id);
        infos
    }

    /// Tra id đã intern cho một key — cầu nối query text → `DocToken::field`.
    pub fn intern_id(&self, s: &str) -> Option<u64> {
        self.intern.get(s)
    }

    /// Giải ngược id interned thành chuỗi (resolve kết quả search).
    pub fn intern_str(&self, id: u64) -> Option<String> {
        self.intern.resolve(id).map(str::to_string)
    }

    /// Tìm node scalar chứa `query` (case-insensitive) — quét node cache,
    /// không cần trie. `limit` chặn kết quả cho payload LLM.
    pub fn search_value_substring(&self, query: &str, limit: usize) -> Vec<Node> {
        let q = query.to_lowercase();
        let cache = self.nodes.lock().unwrap();
        let mut hits = Vec::new();
        for node in cache.values() {
            let matched = match &node.value {
                Some(Scalar::String(s)) => s.to_lowercase().contains(&q),
                Some(Scalar::Number(n)) => format!("{n}").contains(&q),
                _ => false,
            };
            if matched {
                hits.push(node.clone());
                if hits.len() >= limit {
                    break;
                }
            }
        }
        hits.sort_by_key(|n| n.id);
        hits
    }

    /// Tìm node có key chứa `query` (case-insensitive) — fallback cho
    /// `search_path` khi pattern không phải full path từ root (chain radix
    /// luôn bắt đầu từ root nên tên key đơn lẻ không match được).
    pub fn search_key_substring(&self, query: &str, limit: usize) -> Vec<Node> {
        let q = query.to_lowercase();
        let cache = self.nodes.lock().unwrap();
        let mut hits: Vec<Node> = cache
            .values()
            .filter(|n| {
                n.key
                    .as_deref()
                    .is_some_and(|k| k.to_lowercase().contains(&q))
            })
            .cloned()
            .collect();
        hits.sort_by_key(|n| n.id);
        hits.truncate(limit);
        hits
    }

    /// Fuzzy key match — similarity (exact > prefix > contains > Levenshtein)
    /// cộng bonus IDF của key: key càng hiếm (xuất hiện ở ít document) càng
    /// khử tuyến, node match key hiếm lên trước. Query `~tên` ở `doc_search`
    /// rẽ vào đây.
    pub fn search_key_fuzzy(&self, query: &str, limit: usize) -> Vec<KeyHit> {
        let q = query.to_lowercase();
        let total_docs = self.docs.len().max(1) as f64;
        // Điểm similarity cho từng distinct key + đếm doc chứa key.
        let cache = self.nodes.lock().unwrap();
        let mut key_score: HashMap<&str, f64> = HashMap::new();
        let mut key_docs: HashMap<&str, std::collections::HashSet<u64>> = HashMap::new();
        for node in cache.values() {
            let Some(k) = node.key.as_deref() else { continue };
            let kl = k.to_lowercase();
            let sim = if kl == q {
                1.0
            } else if kl.starts_with(&q) {
                0.8
            } else if kl.contains(&q) {
                0.6
            } else {
                let ratio = levenshtein_similarity(&q, &kl);
                if ratio >= 0.7 {
                    ratio
                } else {
                    continue;
                }
            };
            let best = key_score.entry(k).or_insert(0.0);
            *best = best.max(sim);
            key_docs.entry(k).or_default().insert(node.doc);
        }
        if key_score.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<KeyHit> = cache
            .values()
            .filter_map(|node| {
                let k = node.key.as_deref()?;
                let sim = *key_score.get(k)?;
                let key_doc_count = key_docs[k].len().max(1) as f64;
                // IDF của key — log2(total/df); key độc nhất df=1 → bonus lớn.
                let idf = (total_docs / key_doc_count).log2().max(0.0);
                let score = sim + 0.1 * idf;
                Some(KeyHit {
                    node: node.clone(),
                    matched_key: k.to_string(),
                    score,
                })
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.node.id.cmp(&b.node.id))
        });
        hits.truncate(limit);
        hits
    }

    // ── Pattern mining (P#) + structural ranking ─────────────────────

    /// Mine structural patterns: đếm kind chain (root → node lá scalar,
    /// FIELD/IDX wildcard) trên cửa sổ `max_depth` phần tử cuối. Chain mới
    /// được cấp pattern id kế tiếp (id ổn định — registry persist); counts
    /// refresh mỗi lần mine. Kết quả sort theo `doc_freq` tăng dần trong
    /// nhóm đủ `min_count` — pattern đặc trưng (hiếm) lên đầu, noise nền
    /// (~1.0) xuống cuối; kèm token pattern_trie để `search_patterns`.
    pub async fn mine_patterns(
        &mut self,
        top_k: usize,
        min_count: usize,
        max_depth: usize,
    ) -> Result<Vec<PatternEntry>> {
        let total_docs = self.docs.len().max(1);
        // Đếm chain: chain key → (node count, docs set, tokens).
        let mut counts: HashMap<String, (usize, std::collections::HashSet<u64>, Vec<String>)> =
            HashMap::new();
        {
            let cache = self.nodes.lock().unwrap();
            for node in cache.values() {
                if node.value.is_none() {
                    continue; // chỉ node lá scalar — shape "MAP→FIELD→NUMBER".
                }
                let chain = self.kind_chain_of(node.id, &cache);
                let slice = &chain[chain.len().saturating_sub(max_depth)..];
                let labels = kind_chain_labels(slice);
                let key = labels.join("\u{1}");
                let entry = counts
                    .entry(key)
                    .or_insert_with(|| (0, std::collections::HashSet::new(), labels));
                entry.0 += 1;
                entry.1.insert(node.doc);
            }
        }
        // Merge vào registry: chain cũ giữ id, chain mới cấp id kế tiếp.
        // Guard pattern registry phải đóng TRƯỚC mọi .await (non-Send).
        let (mined, chains): (Vec<PatternEntry>, Vec<(Vec<DocToken>, u64)>) = {
            let mut registry = self.patterns.lock().unwrap();
            let mut mined: Vec<PatternEntry> = Vec::new();
            for (_key, (node_count, docs, tokens)) in counts {
                if node_count < min_count {
                    continue;
                }
                let id = match registry.by_chain.get(&_key) {
                    Some(&id) => id,
                    None => {
                        let id = registry.next_id;
                        registry.next_id += 1;
                        registry.by_chain.insert(_key.clone(), id);
                        id
                    }
                };
                mined.push(PatternEntry {
                    pattern_id: id,
                    tokens,
                    node_count,
                    doc_count: docs.len(),
                    doc_freq: docs.len() as f64 / total_docs as f64,
                });
            }
            mined.sort_by(|a, b| {
                a.doc_freq
                    .partial_cmp(&b.doc_freq)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(b.node_count.cmp(&a.node_count))
                    .then(a.pattern_id.cmp(&b.pattern_id))
            });
            mined.truncate(top_k);
            // Registry = union(chain đã biết, kết quả lần này) — chain không
            // còn xuất hiện vẫn giữ id nhưng counts về 0.
            for e in &mut registry.entries {
                if let Some(p) = mined.iter().find(|p| p.pattern_id == e.pattern_id) {
                    *e = p.clone();
                } else {
                    e.node_count = 0;
                    e.doc_count = 0;
                    e.doc_freq = 0.0;
                }
            }
            for p in &mined {
                if !registry.entries.iter().any(|e| e.pattern_id == p.pattern_id) {
                    registry.entries.push(p.clone());
                }
            }
            registry.entries.sort_by_key(|e| e.pattern_id);
            // Index mined chains vào pattern_trie (leaf = PATTERN base + id).
            let chains = mined
                .iter()
                .map(|p| {
                    (
                        p.tokens.iter().map(|t| parse_kind_label(t)).collect::<Vec<_>>(),
                        p.pattern_id,
                    )
                })
                .collect();
            (mined, chains)
        };
        for (tokens, id) in chains {
            Self::insert_chain_allow_dup(
                &mut self.pattern_trie,
                (PATTERN_RECORD_BASE + id) as usize,
                &tokens,
            )
            .await?;
        }
        self.persist_patterns().await?;
        Ok(mined)
    }

    /// Registry hiện tại (counts từ lần mine gần nhất).
    pub fn list_patterns(&self) -> Vec<PatternEntry> {
        self.patterns.lock().unwrap().entries.clone()
    }

    /// Search pattern theo kind chain đã mine — leaf lưu pattern id.
    pub async fn search_patterns(
        &self,
        pattern: &[DocToken],
    ) -> Result<Vec<PatternEntry>> {
        let pages = self.pattern_trie.search(pattern, None).await?;
        let registry = self.patterns.lock().unwrap();
        let mut out = Vec::new();
        for (record, _) in pages {
            let r = record as u64;
            if r >= PATTERN_RECORD_BASE
                && let Some(e) = registry
                    .entries
                    .iter()
                    .find(|e| e.pattern_id == r - PATTERN_RECORD_BASE)
            {
                out.push(e.clone());
            }
        }
        out.sort_by_key(|e| e.pattern_id);
        Ok(out)
    }

    /// Search node theo kind chain (kind token payload 0) trên type trie.
    /// Search node theo kind chain (kind token payload 0) — quét cache so
    /// khớp suffix window. Không dùng trie ở đây vì radix leaf chỉ giữ MỘT
    /// record per chain: các node trùng shape (cùng pattern ở nhiều doc) sẽ
    /// bị collapse còn node đầu tiên. Trie giữ vai trò index của pattern P#
    /// (`pattern_trie` — mỗi pattern một chain), node retrieval quét cache.
    pub fn search_kind_chain(&self, pattern: &[DocToken], _depth: Option<usize>) -> Vec<u64> {
        if pattern.is_empty() {
            return Vec::new();
        }
        let cache = self.nodes.lock().unwrap();
        let mut ids: Vec<u64> = cache
            .values()
            .filter(|node| {
                let chain = self.kind_chain_of_unchecked(node.id, &cache);
                chain.len() >= pattern.len() && chain[chain.len() - pattern.len()..] == *pattern
            })
            .map(|node| node.id)
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Như `kind_chain_of` nhưng nhận cache đã lock bên ngoài.
    fn kind_chain_of_unchecked(&self, node_id: u64, cache: &HashMap<u64, Node>) -> Vec<DocToken> {
        let mut tokens = Vec::new();
        let mut cur = Some(node_id);
        while let Some(id) = cur {
            let Some(n) = cache.get(&id) else { break };
            tokens.push(kind_token(&n.kind));
            cur = n.parent;
        }
        tokens.reverse();
        tokens
    }

    /// Bổ sung cho `search_path`: quét cache trả ĐỦ node có key-chain khớp
    /// pattern (radix leaf chỉ giữ 1 record/chain nên trie chỉ đại diện node
    /// đầu tiên — với repo nhiều doc trùng path thì thiếu). `pattern[0]` là
    /// `DocToken::root()`, các segment sau là `DocToken::field(id)`.
    pub fn search_path_scan(&self, pattern: &[DocToken], limit: usize) -> Vec<u64> {
        if pattern.len() < 2 {
            return Vec::new();
        }
        let fields: Vec<u64> = pattern[1..].iter().map(|t| t.field_key_id()).collect();
        let last = *fields.last().unwrap();
        let cache = self.nodes.lock().unwrap();
        let mut ids: Vec<u64> = Vec::new();
        for node in cache.values() {
            // Lọc thô: node phải mang key cuối của pattern.
            let Some(key) = &node.key else { continue };
            if self.intern.get(key) != Some(last) {
                continue;
            }
            // Xác minh tổ tiên: chuỗi key id từ node lên phải khớp reversed.
            let mut up: Vec<u64> = Vec::with_capacity(fields.len());
            let mut cur = Some(node.id);
            while let Some(id) = cur {
                let Some(n) = cache.get(&id) else { break };
                if let Some(k) = &n.key {
                    up.push(self.intern.get(k).unwrap_or(0));
                }
                cur = n.parent;
                if up.len() == fields.len() {
                    break;
                }
            }
            up.reverse();
            if up == fields {
                ids.push(node.id);
                if ids.len() >= limit {
                    break;
                }
            }
        }
        ids.sort_unstable();
        ids
    }

    /// Uniqueness score của node theo pattern registry — pattern càng hiếm
    /// (ít document chứa) càng điểm: IDF = log2(total_docs / doc_count).
    /// Chain chưa từng mine coi như hiếm nhất (điểm +1).
    pub fn pattern_uniqueness(&self, node_id: u64, total_docs: usize) -> f64 {
        let cache = self.nodes.lock().unwrap();
        if !cache.contains_key(&node_id) {
            return 0.0;
        }
        let chain = self.kind_chain_of(node_id, &cache);
        drop(cache);
        let registry = self.patterns.lock().unwrap();
        match registry.by_chain.get(&kind_labels_key(&chain)) {
            Some(&id) => registry
                .entries
                .iter()
                .find(|e| e.pattern_id == id)
                .map(|e| {
                    if e.doc_count == 0 {
                        (total_docs.max(1) as f64).log2() + 1.0
                    } else {
                        (total_docs.max(1) as f64 / e.doc_count as f64).log2().max(0.0)
                    }
                })
                .unwrap_or(0.0),
            None => (total_docs.max(1) as f64).log2() + 1.0,
        }
    }

    /// Kind chain root → node (FIELD/IDX payload wildcard) từ cache.
    fn kind_chain_of(&self, node_id: u64, cache: &HashMap<u64, Node>) -> Vec<DocToken> {
        let mut tokens = Vec::new();
        let mut cur = Some(node_id);
        while let Some(id) = cur {
            let Some(n) = cache.get(&id) else { break };
            tokens.push(kind_token(&n.kind));
            cur = n.parent;
        }
        tokens.reverse();
        tokens
    }

    // ── Pattern registry persist ─────────────────────────────────────

    async fn persist_patterns(&self) -> Result<()> {
        // Guard phải đóng trước .await (non-Send) — scope block.
        let blob = {
            let registry = self.patterns.lock().unwrap();
            serde_json::to_vec(&registry.entries).map_err(|e| anyhow::anyhow!("{e}"))?
        };
        self.storage
            .write()
            .await
            .set_node_meta(DOC_PATTERNS_RECORD as usize, &blob)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }

    /// Restore registry từ storage. Trả `false` nếu chưa có (graph mới).
    async fn restore_patterns(&mut self) -> Result<bool> {
        let bytes = {
            let guard = self.storage.read().await;
            guard.get_node_meta(DOC_PATTERNS_RECORD as usize).await?
        };
        let Some(bytes) = bytes else {
            return Ok(false);
        };
        if bytes.is_empty() {
            return Ok(false);
        }
        let entries: Vec<PatternEntry> = serde_json::from_slice(&bytes)
            .map_err(|e| anyhow::anyhow!("corrupt pattern registry: {e}"))?;
        let mut reg = PatternRegistry::default();
        for e in entries {
            reg.by_chain.insert(e.tokens.join("\u{1}"), e.pattern_id);
            reg.next_id = reg.next_id.max(e.pattern_id + 1);
            reg.entries.push(e);
        }
        self.patterns = std::sync::Mutex::new(reg);
        Ok(true)
    }

    // ── Stats ─────────────────────────────────────────────────────────

    pub async fn stats(&self) -> Result<DocStats> {
        // nodes đếm từ chain node id (1 lần đọc chain, không đọc từng meta).
        let nodes = {
            let guard = self.storage.read().await;
            guard
                .get_chain(DOC_NODE_LIST_RECORD as usize)
                .await?
                .map(|c| c.len())
                .unwrap_or(0)
        };
        Ok(DocStats {
            docs: self.docs.len(),
            nodes,
        })
    }

    // ── Internal helpers ──────────────────────────────────────

    async fn add_doc_id(&self, doc_id: u64) -> Result<()> {
        let mut list = self.load_doc_list().await?;
        if !list.contains(&doc_id) {
            list.push(doc_id);
            self.storage
                .write()
                .await
                .set_chain(DOC_LIST_RECORD as usize, &list.to_vec())
                .await?;
        }
        Ok(())
    }
    async fn load_doc_list(&self) -> Result<Vec<u64>> {
        let chain = {
            let guard = self.storage.read().await;
            guard.get_chain(DOC_LIST_RECORD as usize).await?
        };
        if let Some(chain) = chain {
            Ok(chain.to_vec())
        } else {
            Ok(Vec::new())
        }
    }
    /// Ghi danh sách node id vào chain sentinel — `rebuild()` đọc từ đây để
    /// khôi phục `nodes` map khi mở lại graph từ storage.
    async fn add_node_ids(&self, node_ids: &[u64]) -> Result<()> {
        let mut list = {
            let chain = {
                let guard = self.storage.read().await;
                guard.get_chain(DOC_NODE_LIST_RECORD as usize).await?
            };
            chain.map(|c| c.to_vec()).unwrap_or_default()
        };
        list.extend_from_slice(node_ids);
        self.storage
            .write()
            .await
            .set_chain(DOC_NODE_LIST_RECORD as usize, &list)
            .await?;
        Ok(())
    }
    fn assign_node_ids(&mut self, mut doc: Document) -> Document {
        // Parser sinh id local (1..N) — remap toàn bộ (kèm parent/children/root)
        // sang dải global (≥ `doc_base`) để nhiều doc trong cùng graph không
        // đè node của nhau. Doc đã có id global (rebuild/re-upsert) giữ nguyên.
        let is_local = doc
            .nodes
            .first()
            .map(|n| n.id < self.doc_base)
            .unwrap_or(false);
        if is_local {
            let offset = self.next_node_id.saturating_sub(1);
            if offset > 0 {
                for node in &mut doc.nodes {
                    node.id += offset;
                    if let Some(p) = node.parent.as_mut() {
                        *p += offset;
                    }
                    for c in &mut node.children {
                        *c += offset;
                    }
                }
                doc.root += offset;
            }
            self.next_node_id += doc.nodes.len() as u64;
        }
        for node in &mut doc.nodes {
            if node.id == 0 {
                node.id = self.next_node_id;
                self.next_node_id += 1;
            }
            node.doc = doc.id;
        }
        doc.root = doc
            .nodes
            .iter()
            .find(|n| n.kind == Kind::Root)
            .map(|n| n.id)
            .unwrap_or(doc.nodes[0].id);
        doc
    }

    async fn remove_document_nodes(&self, doc: &Document) -> Result<()> {
        let removed: Vec<u64> = doc.nodes.iter().map(|n| n.id).collect();
        for node in &doc.nodes {
            self.storage
                .write()
                .await
                .set_node_meta(node.id as usize, &[])
                .await?;
        }
        // Bỏ node id khỏi chain sentinel — stats đếm từ chain nên id cũ
        // (doc bị replace/remove) phải ra khỏi danh sách.
        let mut list = {
            let guard = self.storage.read().await;
            guard
                .get_chain(DOC_NODE_LIST_RECORD as usize)
                .await?
                .map(|c| c.to_vec())
                .unwrap_or_default()
        };
        list.retain(|id| !removed.contains(id));
        self.storage
            .write()
            .await
            .set_chain(DOC_NODE_LIST_RECORD as usize, &list)
            .await?;
        // Cache in-memory cũng bỏ theo.
        {
            let mut cache = self.nodes.lock().unwrap();
            for id in removed {
                cache.remove(&id);
            }
        }
        Ok(())
    }

    async fn collect_path(&self, mut node_id: u64) -> Vec<String> {
        let mut path = Vec::new();
        while let Some(node) = self.node(node_id).await {
            if let Some(key) = &node.key {
                path.push(key.clone());
            }
            node_id = node.parent.unwrap_or(0);
        }
        path.reverse();
        path
    }
    /// Insert một token chain vào trie — token rỗng bỏ qua (`insert_chain` với
    /// key rỗng là lỗi NotFound), key trùng coi như OK (node trùng path/token
    /// với node khác, hoặc re-ingest cùng path — record cũ giữ nguyên).
    async fn insert_chain_allow_dup(
        trie: &mut Search<DocToken>,
        record: usize,
        tokens: &[DocToken],
    ) -> Result<()> {
        if tokens.is_empty() {
            return Ok(());
        }
        let metas: Vec<Option<&[u8]>> = vec![None; tokens.len()];
        if let Err(e) = trie.insert_chain(record, tokens, &metas).await
            && !matches!(e, codegraph_graph::SearchError::Duplicated)
        {
            return Err(anyhow::anyhow!(e.to_string()));
        }
        Ok(())
    }

    async fn insert_node_into_tries(&mut self, node: &Node) -> Result<()> {
        let path_tokens = self.path_tokens(node);
        let type_tokens = self.type_tokens(node);
        let value_tokens = self.value_tokens(node);
        let struct_tokens = self.struct_tokens(node);
        let node_id = node.id;
        Self::insert_chain_allow_dup(
            &mut self.path_trie,
            (PATH_RECORD_BASE + node_id) as usize,
            &path_tokens,
        )
        .await?;
        Self::insert_chain_allow_dup(
            &mut self.type_trie,
            (TYPE_RECORD_BASE + node_id) as usize,
            &type_tokens,
        )
        .await?;
        Self::insert_chain_allow_dup(
            &mut self.value_trie,
            (VALUE_RECORD_BASE + node_id) as usize,
            &value_tokens,
        )
        .await?;
        Self::insert_chain_allow_dup(
            &mut self.struct_trie,
            (STRUCT_RECORD_BASE + node_id) as usize,
            &struct_tokens,
        )
        .await?;
        Ok(())
    }

    fn decode_record(&self, record: usize) -> Option<u64> {
        let r = record as u64;
        for base in [
            PATH_RECORD_BASE,
            TYPE_RECORD_BASE,
            VALUE_RECORD_BASE,
            STRUCT_RECORD_BASE,
            PATTERN_RECORD_BASE,
        ] {
            if r >= base {
                return Some(r - base);
            }
        }
        None
    }

    fn path_tokens(&mut self, node: &Node) -> Vec<DocToken> {
        // Thu keys từ node đi lên (node → ancestor), đảo lại thành
        // ancestor → node rồi MỚI gắn root ở đầu: chain phải là
        // [root, field(top), ..., field(node)] để khớp query full path.
        let mut keys = Vec::new();
        let mut cur = node.id;
        let cache = self.nodes.lock().unwrap();
        while let Some(n) = cache.get(&cur) {
            if let Some(key) = &n.key {
                let key_id = self.intern.intern(key.clone());
                keys.push(DocToken::field(key_id));
            }
            cur = n.parent.unwrap_or(0);
        }
        keys.reverse();
        let mut tokens = vec![DocToken::root()];
        tokens.extend(keys);
        tokens
    }
    /// Token hóa loại node theo chuỗi tổ tiên: MAP → FIELD → NUMBER ...
    /// Payload để 0 — đây là token "kind", không mang id.
    fn type_tokens(&mut self, node: &Node) -> Vec<DocToken> {
        let mut tokens = Vec::new();
        let mut cur = Some(node.clone());
        let cache = self.nodes.lock().unwrap();
        while let Some(n) = cur {
            tokens.push(kind_token(&n.kind));
            cur = n.parent.and_then(|p| cache.get(&p).cloned());
        }
        tokens.reverse();
        tokens
    }
    /// Token hóa giá trị scalar: Str/Num/Bool intern theo giá trị.
    fn value_tokens(&mut self, node: &Node) -> Vec<DocToken> {
        match &node.value {
            Some(Scalar::String(s)) => vec![DocToken::str(self.intern.intern(s.clone()))],
            Some(Scalar::Number(n)) => vec![DocToken::num(self.intern.intern(format!("{n}")))],
            Some(Scalar::Bool(b)) => {
                vec![DocToken::bool(self.intern.intern(b.to_string()))]
            }
            Some(Scalar::Null) => vec![DocToken::null()],
            None => vec![],
        }
    }
    /// Token hóa cấu trúc: cửa sổ 2 tầng [kind cha, kind node] — ví
    /// "FIELD NUMBER" là hình dạng điển hình của một field mang scalar.
    fn struct_tokens(&mut self, node: &Node) -> Vec<DocToken> {
        let parent_kind = node
            .parent
            .and_then(|p| self.nodes.lock().unwrap().get(&p).cloned())
            .map(|p| kind_token(&p.kind));
        let mut tokens = Vec::new();
        if let Some(t) = parent_kind {
            tokens.push(t);
        }
        tokens.push(kind_token(&node.kind));
        tokens
    }
}

fn kind_token(kind: &Kind) -> DocToken {
    match kind {
        Kind::Root => DocToken::root(),
        Kind::Map => DocToken::map(),
        Kind::Array => DocToken::arr(),
        Kind::Index => DocToken::idx(0),
        Kind::Field => DocToken::field(0),
        Kind::String => DocToken::str(0),
        Kind::Number => DocToken::num(0),
        Kind::Bool => DocToken::bool(0),
        Kind::Null | Kind::Reference => DocToken::null(),
    }
}

/// Nhãn hiển thị của một structural token ("MAP", "FIELD", ...).
fn token_label(tag: DocTag) -> &'static str {
    match tag {
        DocTag::Root => "ROOT",
        DocTag::Map => "MAP",
        DocTag::Arr => "ARRAY",
        DocTag::Field => "FIELD",
        DocTag::Idx => "INDEX",
        DocTag::Str => "STRING",
        DocTag::Num => "NUMBER",
        DocTag::Bool => "BOOL",
        DocTag::Null => "NULL",
    }
}

/// Parse nhãn kind ("MAP", "FIELD", ...) về kind token payload 0 — dùng cho
/// query `doc_search_struct`. Nhãn lạ → Map token.
pub fn parse_kind_label(label: &str) -> DocToken {
    match label.trim().to_ascii_uppercase().as_str() {
        "ROOT" => DocToken::root(),
        "ARRAY" | "ARR" => DocToken::arr(),
        "FIELD" => DocToken::field(0),
        "INDEX" | "IDX" => DocToken::idx(0),
        "STRING" | "STR" => DocToken::str(0),
        "NUMBER" | "NUM" => DocToken::num(0),
        "BOOL" => DocToken::bool(0),
        "NULL" => DocToken::null(),
        _ => DocToken::map(),
    }
}

fn kind_chain_labels(chain: &[DocToken]) -> Vec<String> {
    chain
        .iter()
        .map(|t| token_label(t.tag()).to_string())
        .collect()
}

fn kind_labels_key(chain: &[DocToken]) -> String {
    kind_chain_labels(chain).join("\u{1}")
}

/// Similarity = 1 - d(a,b)/max(len) — 1.0 khi trùng khớp hoàn toàn.
fn levenshtein_similarity(a: &str, b: &str) -> f64 {
    let max = a.chars().count().max(b.chars().count());
    if max == 0 {
        return 1.0;
    }
    let d = levenshtein(a, b);
    1.0 - d as f64 / max as f64
}

/// Levenshtein chuẩn (một hàng, O(min·max)).
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Small payload returned to LLM after `hydrate`.
#[derive(Debug, Clone, Serialize)]
pub struct NodePayload {
    pub id: u64,
    pub path: Vec<String>,
    pub kind: Kind,
    pub value: Option<Scalar>,
    pub key: Option<String>,
    pub index: Option<u32>,
    pub doc: u64,
    pub children: Vec<NodePayload>,
}

/// Thông tin tóm tắt một document — trả về cho `doc list`.
#[derive(Debug, Clone, Serialize)]
pub struct DocInfo {
    pub doc_id: u64,
    pub path: String,
    pub format: String,
    pub root_node_id: u64,
    pub nodes: usize,
}

/// Một structural pattern đã mine — kind chain (FIELD/IDX wildcard payload)
/// từ một cửa sổ tổ tiên đến node lá scalar. `doc_freq` = tỷ lệ số document
/// chứa pattern (pattern ~1.0 là noise nền, nhỏ là đặc trưng).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternEntry {
    pub pattern_id: u64,
    /// Nhãn hiển thị, vd ["MAP", "FIELD", "NUMBER"].
    pub tokens: Vec<String>,
    pub node_count: usize,
    pub doc_count: usize,
    pub doc_freq: f64,
}

/// Registry pattern — id (P#) ổn định: chain đã đăng ký giữ nguyên id giữa
/// các lần mine; counts refresh mỗi lần mine.
#[derive(Debug, Default)]
pub struct PatternRegistry {
    entries: Vec<PatternEntry>,
    by_chain: HashMap<String, u64>,
    next_id: u64,
}

/// Kết quả fuzzy match một key.
#[derive(Debug, Clone, Serialize)]
pub struct KeyHit {
    pub node: Node,
    pub matched_key: String,
    /// Điểm similarity (0..1] cộng bonus IDF của key (key hiếm +điểm).
    pub score: f64,
}

/// Summary returned by `codegraph doc stats`.
#[derive(Debug, Default, Serialize)]
pub struct DocStats {
    pub docs: usize,
    pub nodes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_graph::InMemoryStorage;

    #[tokio::test]
    async fn new_graph() {
        let storage = Arc::new(TokioRwLock::new(InMemoryStorage::default()));
        let config = DocConfig::default();
        let graph = DocumentGraph::new(storage, config);
        assert_eq!(graph.stats().await.unwrap_or_default().docs, 0);
    }

    /// `ingest_file` hai file khác nhau → doc id khác nhau, node không đè nhau;
    /// re-ingest cùng path → cùng doc id (thay thế tại chỗ); `open()` lại từ
    /// storage → docs còn nguyên và counter id tiếp tục sau max id cũ.
    #[tokio::test]
    async fn ingest_file_resume_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("a.yaml");
        let p2 = dir.path().join("b.toml");
        let p3 = dir.path().join("c.json");
        std::fs::write(&p1, "service:\n  name: api\n  replicas: 3\n").unwrap();
        std::fs::write(&p2, "[service]\nname = \"db\"\n").unwrap();
        std::fs::write(&p3, r#"{"service": {"name": "web"}}"#).unwrap();

        let storage = Arc::new(TokioRwLock::new(InMemoryStorage::default()));
        let mut graph = DocumentGraph::new(storage.clone(), DocConfig::default());
        let d1 = graph.ingest_file(p1.to_str().unwrap(), None).await.unwrap();
        let d2 = graph.ingest_file(p2.to_str().unwrap(), None).await.unwrap();
        assert_ne!(d1, d2);
        assert_eq!(graph.stats().await.unwrap_or_default().docs, 2);
        // a.yaml: root+service+name+replicas = 4; b.toml: root+service+name = 3.
        // Nếu remap local-id sai thì 2 doc đè node nhau → tổng < 7.
        assert_eq!(graph.stats().await.unwrap_or_default().nodes, 7);

        // Re-ingest cùng path → id giữ nguyên.
        assert_eq!(
            graph.ingest_file(p1.to_str().unwrap(), None).await.unwrap(),
            d1
        );

        // Reopen từ storage — docs phục hồi, ingest tiếp có id mới (không đè).
        let mut reopened = DocumentGraph::open(storage, DocConfig::default())
            .await
            .unwrap();
        assert_eq!(reopened.stats().await.unwrap_or_default().docs, 2);
        // Node list được persist — mở lại phải khôi phục đủ node.
        assert_eq!(reopened.stats().await.unwrap_or_default().nodes, 7);
        let d3 = reopened
            .ingest_file(p3.to_str().unwrap(), None)
            .await
            .unwrap();
        assert!(d3 > d1 && d3 > d2, "d3={d3} phải sau d1={d1}, d2={d2}");
    }

    /// Children phải được persist: hydrate root đi xuống được, và
    /// `hydrate_depth` giới hạn số tầng trả về.
    #[tokio::test]
    async fn hydrate_descends_children_with_depth_limit() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.yaml");
        std::fs::write(&p, "service:\n  name: api\n  replicas: 3\n").unwrap();
        let storage = Arc::new(TokioRwLock::new(InMemoryStorage::default()));
        let mut graph = DocumentGraph::new(storage, DocConfig::default());
        let _doc_id = graph.ingest_file(p.to_str().unwrap(), None).await.unwrap();
        let root = graph.list_docs()[0].root_node_id;

        let full = graph.hydrate(root).await.unwrap();
        // root → service → {name, replicas}
        assert_eq!(full.children.len(), 1, "root phải có 1 con `service`");
        let service = &full.children[0];
        assert_eq!(service.key.as_deref(), Some("service"));
        assert_eq!(service.children.len(), 2, "service phải có name + replicas");

        let shallow = graph.hydrate_depth(root, Some(1)).await.unwrap();
        assert_eq!(shallow.children.len(), 1);
        assert!(
            shallow.children[0].children.is_empty(),
            "max_depth=1 không đi xuống tầng service"
        );
    }

    /// Sau reopen, interner phải khớp token đã ghi trong tries — search theo
    /// key name vẫn trả kết quả.
    #[tokio::test]
    async fn reopen_preserves_interner_and_search() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.yaml");
        std::fs::write(&p, "service:\n  replicas: 3\n").unwrap();
        let storage = Arc::new(TokioRwLock::new(InMemoryStorage::default()));
        let mut graph = DocumentGraph::new(storage.clone(), DocConfig::default());
        graph.ingest_file(p.to_str().unwrap(), None).await.unwrap();
        drop(graph);

        let reopened = DocumentGraph::open(storage, DocConfig::default())
            .await
            .unwrap();
        let key_id = reopened
            .intern_id("replicas")
            .expect("key `replicas` phải còn trong interner sau reopen");
        let tokens = vec![
            DocToken::root(),
            DocToken::field(reopened.intern_id("service").unwrap()),
            DocToken::field(key_id),
        ];
        let ids = reopened.search_path(&tokens, None).await.unwrap();
        assert!(!ids.is_empty(), "search `replicas` sau reopen phải match");
    }

    /// Fuzzy key match: exact/prefix/contains và Levenshtein (sai chính tả)
    /// phải tìm được `replicas`; key vô nghĩa thì không.
    #[tokio::test]
    async fn fuzzy_key_match_ranks_and_finds() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.yaml");
        std::fs::write(&p, "service:\n  replicas: 3\n  name: api\n").unwrap();
        let storage = Arc::new(TokioRwLock::new(InMemoryStorage::default()));
        let mut graph = DocumentGraph::new(storage, DocConfig::default());
        graph.ingest_file(p.to_str().unwrap(), None).await.unwrap();

        // exact (viết hoa vẫn match — case-insensitive).
        let hits = graph.search_key_fuzzy("REPLICAS", 10);
        assert!(hits.iter().any(|h| h.matched_key == "replicas"));
        // sai chính tả 1 ký tự → Levenshtein.
        let hits = graph.search_key_fuzzy("replcas", 10);
        assert!(
            hits.iter().any(|h| h.matched_key == "replicas"),
            "fuzzy phải bắt được lỗi chính tả"
        );
        // key không liên quan → rỗng.
        assert!(graph.search_key_fuzzy("zzzzzz", 10).is_empty());
    }

    /// Pattern mining: hai doc cùng shape → pattern lặp với count/doc đúng;
    /// id (P#) ổn định sau reopen + mine lại; kết quả sort theo doc_freq.
    #[tokio::test]
    async fn mine_patterns_stable_ids_and_frequency() {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        for i in 0..2 {
            let p = dir.path().join(format!("d{i}.yaml"));
            std::fs::write(&p, format!("svc{i}:\n  name: a{i}\n  replicas: {i}\n")).unwrap();
            paths.push(p);
        }
        let storage = Arc::new(TokioRwLock::new(InMemoryStorage::default()));
        let mut graph = DocumentGraph::new(storage.clone(), DocConfig::default());
        for p in &paths {
            graph.ingest_file(p.to_str().unwrap(), None).await.unwrap();
        }
        let mined = graph.mine_patterns(10, 2, 4).await.unwrap();
        // Mỗi scalar lá (name x2, replicas x2) tạo chain [MAP, MAP, STRING|NUMBER].
        let string_pat = mined.iter().find(|p| p.tokens.last() == Some(&"STRING".to_string()));
        let number_pat = mined.iter().find(|p| p.tokens.last() == Some(&"NUMBER".to_string()));
        let string_pat = string_pat.expect("pattern STRING");
        assert_eq!(number_pat.expect("pattern NUMBER").node_count, 2);
        assert_eq!(string_pat.node_count, 2);
        assert_eq!(string_pat.doc_count, 2);
        assert!((string_pat.doc_freq - 1.0).abs() < 1e-9);
        let s_id = string_pat.pattern_id;

        // Reopen + mine lại — id giữ nguyên.
        drop(graph);
        let mut reopened = DocumentGraph::open(storage, DocConfig::default()).await.unwrap();
        let mined2 = reopened.mine_patterns(10, 2, 4).await.unwrap();
        let string_pat2 = mined2
            .iter()
            .find(|p| p.tokens.last() == Some(&"STRING".to_string()))
            .expect("pattern STRING sau reopen");
        assert_eq!(string_pat2.pattern_id, s_id, "pattern id phải ổn định");
    }

    /// Search cấu trúc theo nhãn kind + ranking IDF: node thuộc pattern hiếm
    /// (ít doc) phải đứng trước node pattern nền.
    #[tokio::test]
    async fn struct_search_and_idf_ranking() {
        let dir = tempfile::tempdir().unwrap();
        // d0, d1: shape phổ biến (MAP MAP NUMBER); d2: thêm nhánh hiếm hơn.
        for (i, body) in [
            "svc:\n  replicas: 1\n".to_string(),
            "svc:\n  replicas: 2\n".to_string(),
            "svc:\n  replicas: 3\n  metrics:\n    unique_metric: 9\n".to_string(),
        ]
        .into_iter()
        .enumerate()
        {
            let p = dir.path().join(format!("d{i}.yaml"));
            std::fs::write(&p, body).unwrap();
        }
        let storage = Arc::new(TokioRwLock::new(InMemoryStorage::default()));
        let mut graph = DocumentGraph::new(storage, DocConfig::default());
        for i in 0..3 {
            let p = dir.path().join(format!("d{i}.yaml"));
            graph.ingest_file(p.to_str().unwrap(), None).await.unwrap();
        }
        graph.mine_patterns(10, 2, 4).await.unwrap();

        // Chain [MAP, MAP, NUMBER] match cả 3 node replicas.
        let tokens: Vec<DocToken> = ["MAP", "MAP", "NUMBER"]
            .iter()
            .map(|l| parse_kind_label(l))
            .collect();
        let ids = graph.search_kind_chain(&tokens, None);
        // Window [MAP, MAP, NUMBER] match 3 node replicas + unique_metric
        // (chain [MAP, MAP, MAP, NUMBER] có tail window trùng — đúng ngữ nghĩa
        // cửa sổ của mining).
        assert_eq!(ids.len(), 4);
        // uniqueness: replicas ở 3/3 docs → IDF thấp; unique_metric 1/3 → cao.
        let uniq_replicas = graph.pattern_uniqueness(ids[0], 3);
        let metric_id = graph
            .search_key_fuzzy("unique", 10)
            .first()
            .expect("unique_metric")
            .node
            .id;
        let uniq_metric = graph.pattern_uniqueness(metric_id, 3);
        assert!(
            uniq_metric > uniq_replicas,
            "cấu trúc hiếm phải có IDF cao hơn nền: {uniq_metric} vs {uniq_replicas}"
        );
    }

    #[tokio::test]
    async fn doc_ids_do_not_collide_with_node_ids() {
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<_> = (0..3)
            .map(|i| {
                let p = dir.path().join(format!("d{i}.yaml"));
                std::fs::write(&p, format!("svc{i}:\n  name: a{i}\n")).unwrap();
                p
            })
            .collect();
        let storage = Arc::new(TokioRwLock::new(InMemoryStorage::default()));
        let mut graph = DocumentGraph::new(storage, DocConfig::default());
        let mut doc_ids = Vec::new();
        for f in &files {
            doc_ids.push(graph.ingest_file(f.to_str().unwrap(), None).await.unwrap());
        }
        let node_ids: Vec<u64> = graph.list_docs().iter().map(|i| i.root_node_id).collect();
        for d in &doc_ids {
            assert!(
                !node_ids.contains(d),
                "doc id {d} không được trùng node id nào"
            );
            assert!(
                *d >= DOC_ID_BASE,
                "doc id {d} phải nằm trong dải riêng ≥ DOC_ID_BASE"
            );
        }
    }
}
