//! SharedGraphIndex — index dùng chung cho production (GraphApi/MCP/viz).
//!
//! Mọi request dùng chung 1 snapshot `Arc<GraphIndex>`. Index sống trong một
//! backend persistent mà DSN chỉ rõ (`sqlite://...` / `lmdb://...` / `redis://...`):
//! `GraphIndex::ingest` (CLI/watcher, tiến trình riêng) bump `index_version`
//! trong store; `ensure_fresh` probe version (đọc thẳng store — không cần
//! sidecar) và rebuild snapshot khi stale dưới `rebuild_lock` (N request stale
//! đồng thời chỉ 1 lần rebuild), đổi snapshot dưới `RwLock`. `dsn = None`:
//! in-memory — không có writer ngoài, snapshot coi như luôn fresh sau lần
//! build đầu.
//!
//! DSN là **source duy nhất** cho cả `rebuild` (mở backend) lẫn `current_version`
//! (probe) — nên khi nhiều backend cùng được bật (vd `sqlite` + `lmdb`), backend
//! được chọn theo scheme trong DSN, không phải theo thứ tự feature.

use crate::GraphIndex;
use codegraph_core::Result;
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
    /// DSN nơi persist index (`None` = in-memory, không có writer ngoài).
    dsn: Option<String>,
    state: RwLock<IndexState>,
    /// Serialize rebuild — N request stale đồng thời chỉ 1 lần rebuild.
    rebuild_lock: Arc<Mutex<()>>,
}

impl SharedGraphIndex {
    /// Mở index dùng chung.
    ///
    /// `dsn = Some(d)`: chưa build — `ensure_fresh` sẽ mở đúng backend theo
    /// scheme rồi rebuild index từ store lần đầu. `dsn = None`: in-memory.
    ///
    /// `dsn` phải là DSN đầy đủ scheme (vd `sqlite:///path/db.sqlite`,
    /// `lmdb:///path/db`) — không phải plain path, để nhiều backend cùng bật
    /// vẫn chọn đúng backend.
    pub async fn open(dsn: Option<String>) -> Result<Self> {
        Ok(Self {
            dsn,
            state: RwLock::new(IndexState {
                index: Arc::new(GraphIndex::in_memory()),
                version: 0,
                ready: false,
            }),
            rebuild_lock: Arc::new(Mutex::new(())),
        })
    }

    /// Scheme của DSN (`"sqlite"`, `"lmdb"`, `"redis"`) — `None` nếu in-memory.
    fn scheme(&self) -> Option<&'static str> {
        let dsn = self.dsn.as_ref()?;
        if dsn.starts_with("sqlite://") {
            return Some("sqlite");
        }
        if dsn.starts_with("lmdb://") {
            return Some("lmdb");
        }
        if dsn.starts_with("redis://") {
            return Some("redis");
        }
        // Các scheme/DSN khác (chưa biết) — không đo được version độc lập.
        None
    }

    /// Version index trên đĩa hiện tại — `None` nếu probe thất bại (store chưa
    /// có hoặc đang bị re-index), hay backend không probe độc lập được (redis).
    /// Chỉ gọi khi `dsn.is_some()`.
    async fn current_version(&self) -> Option<u64> {
        let dsn = self.dsn.as_ref()?;
        let path = trim_scheme(dsn);
        match self.scheme() {
            #[cfg(feature = "sqlite")]
            Some("sqlite") => crate::storage::sqlite::SqliteStorage::probe_version(path)
                .await
                .ok(),
            #[cfg(feature = "lmdb")]
            Some("lmdb") => crate::storage::lmdb::probe_version(path).await.ok(),
            // redis không có probe file ngoài — không đo được → stale.
            _ => None,
        }
    }

    /// Snapshot hiện tại có khớp version trên đĩa không. In-memory (không file)
    /// → không có writer ngoài → luôn fresh. Backend không probe được (redis/
    /// unknown scheme) → coi là stale để rebuilt lại.
    async fn is_fresh(&self, version: u64) -> bool {
        if self.dsn.is_none() {
            return true;
        }
        matches!(self.current_version().await, Some(v) if v == version)
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

    /// Build index từ DSN hiện tại rồi swap snapshot (gọi trong `rebuild_lock`).
    /// `GraphIndex::open` tự route theo scheme — không cần nhánh cfg.
    async fn rebuild_inner(&self) -> Result<()> {
        #[cfg(any(feature = "sqlite", feature = "lmdb", feature = "redis"))]
        let index = match &self.dsn {
            Some(d) => GraphIndex::open(d).await?,
            None => GraphIndex::in_memory(),
        };
        #[cfg(not(any(feature = "sqlite", feature = "lmdb", feature = "redis")))]
        let index = GraphIndex::in_memory();

        let version = index.version();
        let mut state = self.state.write().await;
        state.index = Arc::new(index);
        state.version = version;
        state.ready = true;
        Ok(())
    }
}

/// Bỏ `scheme://` khỏi DSN — trả phần còn lại (path cho probe file).
fn trim_scheme(dsn: &str) -> &str {
    dsn.strip_prefix("sqlite://")
        .or_else(|| dsn.strip_prefix("lmdb://"))
        .or_else(|| dsn.strip_prefix("redis://"))
        .unwrap_or(dsn)
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

    // Chỉ test sqlite dùng — build không có feature này vẫn compile.
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
        let db_str = format!("sqlite://{}", db_path.to_string_lossy());

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
        let sgi = Arc::new(SharedGraphIndex::open(Some(db_str.clone())).await.unwrap());
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
