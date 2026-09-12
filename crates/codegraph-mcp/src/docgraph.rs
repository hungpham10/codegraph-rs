//! SharedDocGraph — document graph dùng chung cho MCP server, mở **lazily**.
//!
//! Mở `DocumentGraph` từ storage (`DocumentGraph::open` → rebuild toàn bộ
//! tries từ node/doc JSON) tốn thời gian tuyến tính với số node — với repo
//! document lớn có thể vượt startup timeout của MCP client. Nên server chỉ
//! giữ root lúc khởi động; lần doc tool đầu tiên mới trigger open + rebuild
//! (dưới rebuild_lock — N call đồng thời chỉ 1 lần open), các call sau dùng
//! handle đã cache.

use camino::Utf8PathBuf;
use codegraph_docs::DocumentGraph;
use std::sync::{Arc, RwLock};
use tokio::sync::{Mutex, RwLock as TokioRwLock};

/// Doc graph dùng chung: sẵn sàng (in-memory seed / đã open) hoặc lazy theo root.
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
    /// Bọc handle đã sẵn sàng (in-memory seed của `CodegraphServer::new`).
    pub fn ready(graph: Arc<TokioRwLock<DocumentGraph>>) -> Self {
        Self {
            state: RwLock::new(SharedDocGraphState::Ready(graph)),
            rebuild_lock: Mutex::new(()),
        }
    }

    /// Lazy theo workspace root — chưa open gì. Lỗi open khi `graph()` được
    /// gọi → fallback in-memory (doc tools vẫn dùng được per-session), giữ
    /// nguyên hành vi của đường eager cũ.
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
        let graph = match codegraph_extract::open_doc_graph(&root).await {
            Ok(g) => Arc::new(TokioRwLock::new(g)),
            Err(e) => {
                tracing::warn!("doc graph open failed ({e}) — fallback in-memory");
                Arc::new(TokioRwLock::new(DocumentGraph::new(
                    Arc::new(TokioRwLock::new(codegraph_graph::InMemoryStorage::default())),
                    codegraph_docs::DocConfig::default(),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_docs::DocConfig;

    /// Lazy instance chưa open gì — `is_ready` false, init không chạm storage.
    #[test]
    fn lazy_starts_not_ready() {
        let shared = SharedDocGraph::lazy("/nonexistent-root-xyz".into());
        assert!(!shared.is_ready());
    }

    /// Ready instance trả đúng handle, `graph()` không đổi instance.
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

    /// N call `graph()` đồng thời trên lazy instance chỉ open 1 lần — call sau
    /// dùng chung handle đã cache (root không tồn tại → fallback in-memory).
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
    /// rebuild từ storage và thấy đúng dữ liệu (đường của MCP sau khi
    /// `with_root_and_format` không chạm storage, doc tool đầu mới open).
    #[tokio::test]
    async fn lazy_graph_rebuilds_from_persisted_docs() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();

        // "CLI process": ingest 1 doc JSON vào dataset mặc định của root.
        let doc_file = dir.path().join("sample.json");
        std::fs::write(&doc_file, r#"{"name": "app", "replicas": 2}"#).unwrap();
        {
            let mut graph = codegraph_extract::open_doc_graph(&root).await.unwrap();
            graph
                .ingest_file(doc_file.to_string_lossy().as_ref(), None)
                .await
                .unwrap();
        }

        // "Server process": lazy open thấy lại doc đã persist.
        let shared = SharedDocGraph::lazy(root);
        assert!(!shared.is_ready());
        let graph = shared.graph().await;
        let stats = graph.read().await.stats().await.unwrap();
        assert_eq!(stats.docs, 1);
        assert!(stats.nodes > 0);
        assert!(shared.is_ready());
    }
}
