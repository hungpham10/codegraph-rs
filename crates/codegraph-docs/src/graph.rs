use crate::config::DocConfig;
use crate::intern::Interner;
use crate::ir::{Document, Kind, Node, Scalar};
use crate::tokenize::DocToken;
use anyhow::Result;
use codegraph_graph::Search;
use codegraph_graph::Storage;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock as TokioRwLock;

/// Record-id bases so the same `node_id` can appear in several tries
/// without colliding on the storage key.
const PATH_RECORD_BASE: u64 = 100_000_000_000;
const TYPE_RECORD_BASE: u64 = 200_000_000_000;
const VALUE_RECORD_BASE: u64 = 300_000_000_000;
const STRUCT_RECORD_BASE: u64 = 400_000_000_000;
const PATTERN_RECORD_BASE: u64 = 500_000_000_000;

/// Sentinel storage keys for persisted node/doc lists (node ids are u64
/// that never reach these small constants because real ids start at
/// `DOC_BASE` ≈ 1e9).
const DOC_NODE_LIST_RECORD: u64 = 0;
const DOC_LIST_RECORD: u64 = 1;
const DOC_META_BASE: u64 = 10_000_000_000;

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
    /// Pattern-mining trie — reserve cho tính năng mined patterns, chưa có
    /// reader (trước đây chỉ được clear trong rebuild).
    #[allow(dead_code)]
    pattern_trie: Search<DocToken>,
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
            path_trie: Search::new(sharding, storage.clone()),
            type_trie: Search::new(sharding, storage.clone()),
            value_trie: Search::new(sharding, storage.clone()),
            struct_trie: Search::new(sharding, storage.clone()),
            pattern_trie: Search::new(sharding, storage.clone()),
            doc_base,
            next_doc_id: doc_base,
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
        Ok(graph)
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
        // Insert into tries.
        for node in &doc.nodes {
            self.insert_node_into_tries(node).await?;
        }
        // Materialize nodes vào cache in-memory (hydrate/collect_path đọc từ
        // đây trước, thiếu thì mới xuống storage).
        {
            let mut cache = self.nodes.lock().unwrap();
            for node in &doc.nodes {
                cache.insert(node.id, node.clone());
            }
        }
        self.docs.insert(doc_id, doc.clone());
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
        let node = self.node(node_id).await?;
        let path = self.collect_path(node_id).await;
        let mut children = Vec::new();
        for c in &node.children {
            if let Some(payload) = Box::pin(self.hydrate(*c)).await {
                children.push(payload);
            }
        }
        Some(NodePayload {
            id: node.id,
            path,
            kind: node.kind,
            value: node.value.clone(),
            key: node.key.clone(),
            doc: node.doc,
            children,
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
        let mut tokens = vec![DocToken::root()];
        let mut cur = node.id;
        let cache = self.nodes.lock().unwrap();
        while let Some(n) = cache.get(&cur) {
            if let Some(key) = &n.key {
                let key_id = self.intern.intern(key.clone());
                tokens.push(DocToken::field(key_id));
            }
            cur = n.parent.unwrap_or(0);
        }
        tokens.reverse();
        tokens
    }
    fn type_tokens(&self, _node: &Node) -> Vec<DocToken> {
        vec![DocToken::map(), DocToken::field(0)] // simplified
    }
    fn value_tokens(&self, _node: &Node) -> Vec<DocToken> {
        vec![]
    }
    fn struct_tokens(&self, _node: &Node) -> Vec<DocToken> {
        vec![]
    }
}

/// Small payload returned to LLM after `hydrate`.
#[derive(Debug, Clone, Serialize)]
pub struct NodePayload {
    pub id: u64,
    pub path: Vec<String>,
    pub kind: Kind,
    pub value: Option<Scalar>,
    pub key: Option<String>,
    pub doc: u64,
    pub children: Vec<NodePayload>,
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
}
