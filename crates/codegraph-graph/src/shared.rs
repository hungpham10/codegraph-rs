//! SharedGraphIndex — index dùng chung cho production (GraphApi/MCP/viz).
//!
//! Mọi request dùng chung 1 snapshot `Arc<GraphIndex>`. Index sống trong chính
//! file `.codegraph/db.sqlite` (entity store `sg_*` + radix chain engine `rt_*`):
//! `GraphIndex::ingest` (CLI/watcher, tiến trình riêng) bump `index_version`
//! trong file; `ensure_fresh` probe version (đọc thẳng file — không cần sidecar)
//! và rebuild snapshot khi stale dưới `rebuild_lock` (N request stale đồng thời
//! chỉ 1 lần rebuild), đổi snapshot dưới `RwLock`. `path = None`: in-memory —
//! không có writer ngoài, snapshot coi như luôn fresh sau lần build đầu.

use crate::GraphIndex;
use codegraph_core::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

/// Snapshot hiện tại của index + version mà nó được build từ đó.
struct IndexState {
    /// Index snapshot — swap nguyên cái này khi rebuild xong.
    index: Arc<GraphIndex>,
    /// `GraphIndex::version()` lúc build (0 = chưa build).
    version: u64,
    /// Index đã build từ dữ liệu (không phải snapshot rỗng khởi tạo).
    ready: bool,
}

/// Index dùng chung (production): GraphApi, MCP server, viz CLI cùng tham
/// chiếu 1 instance. Rebuild đồng bộ theo version file — request đầu sau khi
/// re-index xong chờ rebuild, các request sau thấy đã fresh.
pub struct SharedGraphIndex {
    /// Nơi persist index (`None` = in-memory, chạy không feature `sqlite`).
    /// Chỉ đọc trong nhánh `sqlite` (open/rebuild) — build không feature này
    /// giữ `None` nên field không được dùng.
    #[cfg_attr(not(feature = "sqlite"), allow(dead_code))]
    path: Option<PathBuf>,
    state: RwLock<IndexState>,
    /// Serialize rebuild — N request stale đồng thời chỉ 1 lần rebuild.
    rebuild_lock: Arc<Mutex<()>>,
}

impl SharedGraphIndex {
    /// Mở index dùng chung.
    ///
    /// `path = Some(p)` (feature `sqlite`): chưa build — `ensure_fresh` sẽ
    /// reopen + rebuild index từ file lần đầu. `path = None`: in-memory.
    pub async fn open(path: Option<PathBuf>) -> Result<Self> {
        Ok(Self {
            path,
            state: RwLock::new(IndexState {
                index: Arc::new(GraphIndex::in_memory()),
                version: 0,
                ready: false,
            }),
            rebuild_lock: Arc::new(Mutex::new(())),
        })
    }

    /// Version index trên đĩa hiện tại — `None` nếu probe thất bại (file chưa
    /// có hoặc đang bị re-index). Chỉ gọi khi `path.is_some()`.
    #[cfg(feature = "sqlite")]
    async fn current_version(&self) -> Option<u64> {
        let p = self.path.as_ref()?;
        crate::storage::sqlite::SqliteStorage::probe_version(&p.display().to_string())
            .await
            .ok()
    }

    /// Snapshot hiện tại có khớp version trên đĩa không. In-memory (không file)
    /// → không có writer ngoài → luôn fresh.
    async fn is_fresh(&self, version: u64) -> bool {
        #[cfg(feature = "sqlite")]
        {
            if self.path.is_none() {
                return true;
            }
            matches!(self.current_version().await, Some(v) if v == version)
        }
        #[cfg(not(feature = "sqlite"))]
        {
            let _ = version;
            true
        }
    }

    /// Đảm bảo index mới nhất, trả snapshot dùng được.
    ///
    /// Fresh (ready + đúng version) → trả ngay. Stale hoặc chưa build → rebuild
    /// đồng bộ dưới `rebuild_lock` rồi trả snapshot mới.
    pub async fn ensure_fresh(self: &Arc<Self>) -> Arc<GraphIndex> {
        // Fast path: snapshot mới nhất sẵn sàng.
        {
            let state = self.state.read().await;
            if state.ready && self.is_fresh(state.version).await {
                return state.index.clone();
            }
        }

        // Slow path: rebuild đồng bộ. N request đồng thời chỉ 1 rebuild; request
        // chờ lock xong sẽ thấy đã fresh (re-check).
        let _guard = self.rebuild_lock.lock().await;
        {
            let state = self.state.read().await;
            if state.ready && self.is_fresh(state.version).await {
                return state.index.clone();
            }
        }
        if let Err(e) = self.rebuild_inner().await {
            eprintln!("[codegraph] shared index rebuild failed: {e}");
        }
        self.state.read().await.index.clone()
    }

    /// Build index từ file hiện tại rồi swap snapshot (gọi trong `rebuild_lock`).
    async fn rebuild_inner(&self) -> Result<()> {
        #[cfg(feature = "sqlite")]
        let index = match &self.path {
            Some(p) => GraphIndex::open(&p.display().to_string()).await?,
            None => GraphIndex::in_memory(),
        };
        #[cfg(all(feature = "redis", not(feature = "sqlite")))]
        let index = match &self.path {
            Some(p) => GraphIndex::open(&p.display().to_string()).await?,
            None => GraphIndex::in_memory(),
        };
        #[cfg(not(any(feature = "sqlite", feature = "redis")))]
        let index = GraphIndex::in_memory();

        let version = index.version();
        let mut state = self.state.write().await;
        state.index = Arc::new(index);
        state.version = version;
        state.ready = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ParseResult;
    use codegraph_core::{CallRecord, SYMBOL_BASE, Symbol, SymbolKind};

    // Chỉ test sqlite dùng — build không feature này vẫn compile.
    #[cfg_attr(not(feature = "sqlite"), allow(dead_code))]
    fn sym(name: &str, id: u64) -> Symbol {
        Symbol {
            id,
            name: name.to_string(),
            kind: SymbolKind::Function,
            scope: codegraph_core::ScopeLevel::Global,
            scope_id: 0,
            type_ref: 0,
            type_name: None,
            file: "a.ts".into(),
            line: 1,
            end_line: 1,
            signature: None,
            doc: None,
            annotations: Vec::new(),
            language: "ts".into(),
        }
    }

    // Chỉ test sqlite dùng — build không feature này vẫn compile.
    #[cfg_attr(not(feature = "sqlite"), allow(dead_code))]
    fn mk_result(path: &str, symbols: Vec<Symbol>, chain: Vec<u64>) -> ParseResult {
        ParseResult {
            path: path.into(),
            language: "ts".into(),
            bytes: 0,
            lines: 0,
            symbols,
            chains: std::collections::HashMap::from([(SYMBOL_BASE, chain)]),
            calls: Vec::<CallRecord>::new(),
        }
    }

    #[tokio::test]
    async fn in_memory_ensure_fresh_returns_ready_snapshot() {
        let sgi = Arc::new(SharedGraphIndex::open(None).await.unwrap());
        let idx1 = sgi.ensure_fresh().await;
        assert_eq!(idx1.version(), 0);
        // Fresh sau lần build đầu — cùng snapshot, không rebuild.
        let idx2 = sgi.ensure_fresh().await;
        assert!(Arc::ptr_eq(&idx1, &idx2));
    }

    /// Re-index ngoài (bump version) → ensure_fresh phát hiện stale → rebuild.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn sqlite_stale_version_rebuilds() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.sqlite");
        let db_str = db_path.to_string_lossy().into_owned();

        // "CLI process": index dữ liệu vào file.
        {
            let mut idx = GraphIndex::open(&db_str).await.unwrap();
            let r = mk_result(
                "a.ts",
                vec![sym("a", SYMBOL_BASE), sym("b", SYMBOL_BASE + 1)],
                vec![SYMBOL_BASE, SYMBOL_BASE + 1],
            );
            idx.ingest(&[r]).await.unwrap();
        }

        // "Server process": shared index trên cùng file.
        let sgi = Arc::new(SharedGraphIndex::open(Some(db_path.clone())).await.unwrap());
        let idx = sgi.ensure_fresh().await;
        assert_eq!(idx.version(), 1);
        assert_eq!(idx.stats().symbols, 2);
        assert_eq!(idx.symbol_by_id(SYMBOL_BASE).unwrap().name, "a");

        // Re-index lại (full re-index → version bump, dữ liệu đổi).
        {
            let mut idx = GraphIndex::open(&db_str).await.unwrap();
            let r = mk_result("b.ts", vec![sym("x", SYMBOL_BASE)], vec![SYMBOL_BASE]);
            idx.ingest(&[r]).await.unwrap();
        }
        let idx2 = sgi.ensure_fresh().await;
        assert_eq!(idx2.version(), 2);
        assert_eq!(idx2.stats().symbols, 1);
        assert_eq!(idx2.symbol_by_id(SYMBOL_BASE).unwrap().name, "x");
    }
}
