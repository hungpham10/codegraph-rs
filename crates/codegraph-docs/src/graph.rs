use crate::config::DocConfig;
use crate::ir::{Document, Kind, Node, Scalar};
use crate::intern::Interner;
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
    nodes: HashMap<u64, Node>,
    intern: Interner,
    path_trie: Search<DocToken>,
    type_trie: Search<DocToken>,
    value_trie: Search<DocToken>,
    struct_trie: Search<DocToken>,
    pattern_trie: Search<DocToken>,
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
            nodes: HashMap::new(),
            intern: Interner::new(),
            path_trie: Search::new(sharding, storage.clone()),
            type_trie: Search::new(sharding, storage.clone()),
            value_trie: Search::new(sharding, storage.clone()),
            struct_trie: Search::new(sharding, storage.clone()),
            pattern_trie: Search::new(sharding, storage.clone()),
            next_doc_id: doc_base,
            next_node_id: doc_base,
        }
    }

    /// Open an existing graph from persistent storage and rebuild the tries.
    pub async fn open(storage: Arc<TokioRwLock<dyn Storage>>, config: DocConfig) -> Result<Self> {
        let mut graph = Self::new(storage, config);
        graph.rebuild().await?;
        Ok(graph)
    }

    /// Rebuild all materialized tries from persisted node/doc metadata.
    pub async fn rebuild(&mut self) -> Result<()> {
        // Load node list.
        let node_ids = {
            let guard = self.storage.read().await;
            if let Some(chain) = guard.get_chain(DOC_NODE_LIST_RECORD as usize).await? {
                chain.iter().map(|&x| x as u64).collect()
            } else {
                Vec::new()
            }
        };
        // Load docs list.
        let doc_ids = {
            let guard = self.storage.read().await;
            if let Some(chain) = guard.get_chain(DOC_LIST_RECORD as usize).await? {
                chain.iter().map(|&x| x as u64).collect()
            } else {
                Vec::new()
            }
        };
        // Load nodes.
        for id in &node_ids {
            let bytes = {
                let guard = self.storage.read().await;
                guard.get_node_meta(*id as usize).await?
            };
            if let Some(bytes) = bytes {
                if let Ok(node) = serde_json::from_slice::<Node>(&bytes) {
                    self.nodes.insert(node.id, node);
                }
            }
        }
        // Load docs.
        for id in &doc_ids {
            let meta_id = DOC_META_BASE + id;
            let bytes = {
                let guard = self.storage.read().await;
                guard.get_node_meta(meta_id as usize).await?
            };
            if let Some(bytes) = bytes {
                if let Ok(doc) = serde_json::from_slice::<Document>(&bytes) {
                    self.docs.insert(doc.id, doc);
                }
            }
        }
        // Rebuild tries.
        self.path_trie.clear().await?;
        self.type_trie.clear().await?;
        self.value_trie.clear().await?;
        self.struct_trie.clear().await?;
        self.pattern_trie.clear().await?;
        let nodes: Vec<Node> = self.nodes.values().cloned().collect();
        for node in nodes {
            self.insert_node_into_tries(&node).await?;
        }
        Ok(())
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
        // Insert into tries.
        for node in &doc.nodes {
            self.insert_node_into_tries(node).await?;
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

    /// Return the document owning `node_id`, if any.
    pub fn doc_of(&self, node_id: u64) -> Option<&Document> {
        self.nodes.get(&node_id).and_then(|n| self.docs.get(&n.doc))
    }

    /// Hydrate a node into a small payload suitable for LLM reasoning.
    pub fn hydrate(&self, node_id: u64) -> Option<NodePayload> {
        let node = self.nodes.get(&node_id)?;
        let path = self.collect_path(node_id);
        Some(NodePayload {
            id: node.id,
            path,
            kind: node.kind,
            value: node.value.clone(),
            key: node.key.clone(),
            doc: node.doc,
            children: node.children.iter().filter_map(|c| self.hydrate(*c)).collect(),
        })
    }

    // ── Query pipeline (reuses Search::search_resumable) ──────────────

    pub async fn search_path(&self, pattern: &[DocToken], depth: Option<usize>) -> Result<Vec<u64>> {
        self.search_trie(&self.path_trie, pattern, depth).await
    }
    pub async fn search_type(&self, pattern: &[DocToken], depth: Option<usize>) -> Result<Vec<u64>> {
        self.search_trie(&self.type_trie, pattern, depth).await
    }
    pub async fn search_value(&self, pattern: &[DocToken], depth: Option<usize>) -> Result<Vec<u64>> {
        self.search_trie(&self.value_trie, pattern, depth).await
    }
    pub async fn search_struct(&self, pattern: &[DocToken], depth: Option<usize>) -> Result<Vec<u64>> {
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

    pub fn stats(&self) -> DocStats {
        DocStats {
            docs: self.docs.len(),
            nodes: self.nodes.len(),
        }
    }

    // ── Internal helpers ──────────────────────────────────────

    async fn add_doc_id(&self, doc_id: u64) -> Result<()> {
        let mut list = self.load_doc_list().await?;
        if !list.contains(&doc_id) {
            list.push(doc_id);
            self.storage
                .write()
                .await
                .set_chain(DOC_LIST_RECORD as usize, &list.iter().map(|&x| x as u64).collect::<Vec<_>>())
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
            Ok(chain.iter().map(|&x| x as u64).collect())
        } else {
            Ok(Vec::new())
        }
    }
    fn assign_node_ids(&mut self, mut doc: Document) -> Document {
        for node in &mut doc.nodes {
            if node.id == 0 {
                node.id = self.next_node_id;
                self.next_node_id += 1;
            }
            node.doc = doc.id;
        }
        doc.root = doc.nodes.iter().find(|n| n.kind == Kind::Root).map(|n| n.id).unwrap_or(doc.nodes[0].id);
        doc
    }

    async fn remove_document_nodes(&self, doc: &Document) -> Result<()> {
        for node in &doc.nodes {
            self.storage
                .write()
                .await
                .set_node_meta(node.id as usize, &[])
                .await?;
        }
        Ok(())
    }

    fn collect_path(&self, mut node_id: u64) -> Vec<String> {
        let mut path = Vec::new();
        while let Some(node) = self.nodes.get(&node_id) {
            if let Some(key) = &node.key {
                path.push(key.clone());
            }
            node_id = node.parent.unwrap_or(0);
        }
        path.reverse();
        path
    }
    async fn insert_node_into_tries(&mut self, node: &Node) -> Result<()> {
        let path_tokens = self.path_tokens(node);
        let type_tokens = self.type_tokens(node);
        let value_tokens = self.value_tokens(node);
        let struct_tokens = self.struct_tokens(node);
        let node_id = node.id;
        {
            let trie = &mut self.path_trie;
            let record = (PATH_RECORD_BASE + node_id) as usize;
            let metas: Vec<Option<&[u8]>> = vec![None; path_tokens.len()];
            trie.insert_chain(record, &path_tokens, &metas).await?;
        }
        {
            let trie = &mut self.type_trie;
            let record = (TYPE_RECORD_BASE + node_id) as usize;
            let metas: Vec<Option<&[u8]>> = vec![None; type_tokens.len()];
            trie.insert_chain(record, &type_tokens, &metas).await?;
        }
        {
            let trie = &mut self.value_trie;
            let record = (VALUE_RECORD_BASE + node_id) as usize;
            let metas: Vec<Option<&[u8]>> = vec![None; value_tokens.len()];
            trie.insert_chain(record, &value_tokens, &metas).await?;
        }
        {
            let trie = &mut self.struct_trie;
            let record = (STRUCT_RECORD_BASE + node_id) as usize;
            let metas: Vec<Option<&[u8]>> = vec![None; struct_tokens.len()];
            trie.insert_chain(record, &struct_tokens, &metas).await?;
        }
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
        while let Some(n) = self.nodes.get(&cur) {
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
    use codegraph_graph::storage::InMemoryStorage;

    #[test]
    fn new_graph() {
        let storage = Arc::new(RwLock::new(InMemoryStorage::default()));
        let config = DocConfig::default();
        let graph = DocumentGraph::new(storage, config);
        assert_eq!(graph.stats().docs, 0);
    }
}
