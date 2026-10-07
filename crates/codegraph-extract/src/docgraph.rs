//! Document graph runtime — mở `DocumentGraph` từ `[docgraph]`/`[storage]` của
//! `.codegraph/config.toml` (dùng chung cho CLI `init`/`doc` và MCP server).

use crate::config::ExtractConfig;
use camino::{Utf8Path, Utf8PathBuf};
use codegraph_core::{Error, Result};
use codegraph_docs::{DocConfig, DocumentGraph, StorageConfig};
use std::sync::{Arc, RwLock};
use tokio::sync::{Mutex, RwLock as TokioRwLock};

/// Mở document graph theo config: dataset riêng cho docs (mặc định
/// `.codegraph/docs.sqlite` với sqlite — tries của docs đụng namespace với code
/// index nên KHÔNG dùng chung dataset), rebuild tries từ storage. Không có DSN
/// hợp lệ (memory/RDBMS không override) → in-memory.
pub async fn open_doc_graph(root: &Utf8Path) -> Result<DocumentGraph> {
    let cfg = ExtractConfig::load(root);
    let (config, _) = match cfg.doc_config(root) {
        Some(pair) => pair,
        // Không khai báo `[docgraph]` — vẫn mở dataset mặc định để CLI `doc`
        // và MCP persist đúng (dsn mặc định theo backend kind của `[storage]`).
        None => {
            let dsn = cfg.doc_storage_dsn(root);
            let config = DocConfig {
                storage: dsn.map(|dsn| StorageConfig {
                    r#type: Some(dsn.split("://").next().unwrap_or("sqlite").to_string()),
                    dsn: Some(dsn),
                }),
                ..Default::default()
            };
            (config, Vec::new())
        }
    };
    let storage: Arc<TokioRwLock<dyn codegraph_graph::Storage>> =
        match config.storage.as_ref().and_then(|s| s.dsn.as_deref()) {
            Some(dsn) => codegraph_graph::open_doc_storage(dsn).await?,
            None => Arc::new(TokioRwLock::new(codegraph_graph::InMemoryStorage::default())),
        };
    DocumentGraph::open(storage, config)
        .await
        .map_err(|e| Error::Db(format!("open document graph: {e}")))
}

/// Doc graph dùng chung: sẵn sàng (in-memory seed / đã open) hoặc lazy theo root.
///
/// Mở `DocumentGraph` từ storage tốn thời gian tuyến tính với số node — repo
/// document lớn có thể vượt startup timeout. Nên server giữ root lúc khởi
/// động; lần doc op đầu tiên mới trigger open + rebuild (dưới `rebuild_lock`,
/// N call đồng thời chỉ 1 lần open), các call sau dùng handle đã cache.
pub struct SharedDocGraph {
    state: RwLock<SharedDocGraphState>,
    /// Serialize open+rebuild — N doc call đồng thời chỉ 1 lần open.
    rebuild_lock: Mutex<()>,
}

enum SharedDocGraphState {
    /// Handle sẵn sàng — trả ngay, không chờ.
    Ready(Arc<TokioRwLock<DocumentGraph>>),
    /// Chưa open — lần `graph()` đầu mở storage + rebuild từ root này.
    Lazy(Utf8PathBuf),
}

impl SharedDocGraph {
    /// Bọc handle đã sẵn sàng (in-memory seed).
    pub fn ready(graph: Arc<TokioRwLock<DocumentGraph>>) -> Self {
        Self {
            state: RwLock::new(SharedDocGraphState::Ready(graph)),
            rebuild_lock: Mutex::new(()),
        }
    }

    /// Doc graph in-memory rỗng (khi session chưa bind root).
    pub fn in_memory() -> Self {
        let storage: Arc<TokioRwLock<dyn codegraph_graph::Storage>> =
            Arc::new(TokioRwLock::new(codegraph_graph::InMemoryStorage::default()));
        Self::ready(Arc::new(TokioRwLock::new(DocumentGraph::new(
            storage,
            DocConfig::default(),
        ))))
    }

    /// Lazy theo workspace root — chưa open gì. Lỗi open khi `graph()` được
    /// gọi → fallback in-memory (doc tools vẫn dùng được per-session).
    pub fn lazy(root: Utf8PathBuf) -> Self {
        Self {
            state: RwLock::new(SharedDocGraphState::Lazy(root)),
            rebuild_lock: Mutex::new(()),
        }
    }

    /// Handle dùng được: fast path trả handle cached; lazy thì open+rebuild
    /// đúng một lần dưới rebuild_lock rồi cache.
    pub async fn graph(&self) -> Arc<TokioRwLock<DocumentGraph>> {
        if let SharedDocGraphState::Ready(g) = &*self.state.read().unwrap() {
            return g.clone();
        }
        let _guard = self.rebuild_lock.lock().await;
        if let SharedDocGraphState::Ready(g) = &*self.state.read().unwrap() {
            return g.clone();
        }
        let root = match &*self.state.read().unwrap() {
            SharedDocGraphState::Lazy(root) => root.clone(),
            SharedDocGraphState::Ready(g) => return g.clone(),
        };
        let graph = match open_doc_graph(&root).await {
            Ok(g) => Arc::new(TokioRwLock::new(g)),
            Err(e) => {
                tracing::warn!("doc graph open failed ({e}) — fallback in-memory");
                Arc::new(TokioRwLock::new(DocumentGraph::new(
                    Arc::new(TokioRwLock::new(codegraph_graph::InMemoryStorage::default())),
                    DocConfig::default(),
                )))
            }
        };
        *self.state.write().unwrap() = SharedDocGraphState::Ready(graph.clone());
        graph
    }

    /// Doc graph đã open chưa (`false` trên lazy instance chưa `graph()` nào).
    pub fn is_ready(&self) -> bool {
        matches!(&*self.state.read().unwrap(), SharedDocGraphState::Ready(_))
    }

    /// Re-point sang root mới (gọi khi session `init` bind workspace khác).
    /// Reset về trạng thái Lazy — lần doc op tiếp theo mở lại từ đĩa.
    pub fn reinit(&self, root: Utf8PathBuf) {
        *self.state.write().unwrap() = SharedDocGraphState::Lazy(root);
    }
}

#[cfg(test)]
mod shared_tests {
    use super::*;

    /// Lazy instance chưa open gì — `is_ready` false.
    #[test]
    fn lazy_starts_not_ready() {
        let shared = SharedDocGraph::lazy("/nonexistent-root-xyz".into());
        assert!(!shared.is_ready());
    }

    /// Ready instance trả đúng handle.
    #[tokio::test]
    async fn ready_returns_cached_handle() {
        let storage: Arc<TokioRwLock<dyn codegraph_graph::Storage>> =
            Arc::new(TokioRwLock::new(codegraph_graph::InMemoryStorage::default()));
        let graph = Arc::new(TokioRwLock::new(DocumentGraph::new(
            storage,
            DocConfig::default(),
        )));
        let shared = SharedDocGraph::ready(graph.clone());
        assert!(shared.is_ready());
        let got = shared.graph().await;
        assert!(Arc::ptr_eq(&graph, &got));
    }

    /// N call `graph()` đồng thời trên lazy instance chỉ open 1 lần.
    #[tokio::test]
    async fn lazy_concurrent_calls_share_single_open() {
        let shared = Arc::new(SharedDocGraph::lazy("/nonexistent-root-xyz".into()));
        let (a, b) = {
            let (s1, s2) = (shared.clone(), shared.clone());
            tokio::join!(
                async move { s1.graph().await },
                async move { s2.graph().await }
            )
        };
        assert!(Arc::ptr_eq(&a, &b));
        assert!(shared.is_ready());
    }

    /// Lazy instance trên root có docs.sqlite đã seed: lần `graph()` đầu
    /// rebuild từ storage và thấy đúng dữ liệu.
    #[tokio::test]
    async fn lazy_graph_rebuilds_from_persisted_docs() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();

        let doc_file = dir.path().join("sample.json");
        std::fs::write(&doc_file, r#"{"name": "app", "replicas": 2}"#).unwrap();
        {
            let mut graph = open_doc_graph(&root).await.unwrap();
            graph
                .ingest_file(doc_file.to_string_lossy().as_ref(), None)
                .await
                .unwrap();
        }

        let shared = SharedDocGraph::lazy(root);
        assert!(!shared.is_ready());
        let graph = shared.graph().await;
        let stats = graph.read().await.stats().await.unwrap();
        assert_eq!(stats.docs, 1);
        assert!(stats.nodes > 0);
        assert!(shared.is_ready());
    }
}
