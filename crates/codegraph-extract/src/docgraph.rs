//! Document graph runtime — mở `DocumentGraph` từ `[docgraph]`/`[storage]` của
//! `.codegraph/config.toml` (dùng chung cho CLI `init`/`doc` và MCP server).

use crate::config::ExtractConfig;
use camino::Utf8Path;
use codegraph_core::{Error, Result};
use codegraph_docs::{DocConfig, DocumentGraph, StorageConfig};
use std::sync::Arc;
use tokio::sync::RwLock as TokioRwLock;

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
