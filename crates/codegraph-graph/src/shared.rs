//! SharedGraphIndex — index dùng chung cho production (GraphApi/MCP/viz).
//!
//! Mọi request dùng chung 1 snapshot `Arc<GraphIndex>`. Index sống trong một
//! backend persistent mà `StorageRoute` chỉ rõ (`sqlite://...` / `lmdb://...` /
//! `redis://...` / `Sharded{dsns, repo_id, ...}` cho Postgres/MySQL):
//! `GraphIndex::ingest` (CLI/watcher, tiến trình riêng) bump `index_version`
//! trong store; `ensure_fresh` probe version (đọc thẳng store — không cần
//! sidecar) và rebuild snapshot khi stale dưới `rebuild_lock` (N request stale
//! đồng thời chỉ 1 lần rebuild), đổi snapshot dưới `RwLock`. `route = None`:
//! in-memory — không có writer ngoài, snapshot coi như luôn fresh sau lần
//! build đầu.
//!
//! `StorageRoute` là **source duy nhất** cho cả `rebuild` (mở backend) lẫn
//! `current_version` (probe) — nên khi nhiều backend cùng được bật, backend
//! được chọn theo scheme trong route, không phải theo thứ tự feature.

use crate::GraphIndex;
use crate::storage::Storage;
use codegraph_core::{Result, SemgraphStats, StorageRoute};
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
    /// Route persist index (`None` = in-memory, không có writer ngoài).
    route: Option<StorageRoute>,
    state: RwLock<IndexState>,
    /// Serialize rebuild — N request stale đồng thời chỉ 1 lần rebuild.
    rebuild_lock: Arc<Mutex<()>>,
    /// Storage read-only cache để `stats_cached` đọc counts O(1) không rebuild.
    stats_storage: RwLock<Option<Arc<dyn Storage>>>,
}

impl SharedGraphIndex {
    /// Mở index dùng chung từ một DSN string (sqlite/lmdb/redis). Tiện ích bọc
    /// `open_route` với `StorageRoute::Local`.
    pub async fn open(dsn: Option<String>) -> Result<Self> {
        Self::open_route(dsn.map(StorageRoute::Local)).await
    }

    /// Mở index dùng chung theo `StorageRoute` (hỗ trợ multi-tenant + sharding
    /// RDBMS). `route = None` → in-memory.
    pub async fn open_route(route: Option<StorageRoute>) -> Result<Self> {
        Ok(Self {
            route,
            state: RwLock::new(IndexState {
                index: Arc::new(GraphIndex::in_memory()),
                version: 0,
                ready: false,
            }),
            rebuild_lock: Arc::new(Mutex::new(())),
            stats_storage: RwLock::new(None),
        })
    }

    /// Scheme của backend (`"sqlite"`, `"lmdb"`, `"redis"`, `"postgres"`,
    /// `"mysql"`) — `None` nếu in-memory hoặc không đo được version độc lập.
    fn backend_scheme(&self) -> Option<&'static str> {
        let route = self.route.as_ref()?;
        match route {
            StorageRoute::Memory => None,
            StorageRoute::Local(dsn) => {
                if dsn.starts_with("sqlite://") {
                    Some("sqlite")
                } else if dsn.starts_with("lmdb://") {
                    Some("lmdb")
                } else if dsn.starts_with("redis://") {
                    Some("redis")
                } else {
                    None
                }
            }
            StorageRoute::Sharded { dsns, .. } => {
                let dsn = dsns.first()?;
                if dsn.starts_with("postgres://") {
                    Some("postgres")
                } else if dsn.starts_with("mysql://") {
                    Some("mysql")
                } else {
                    None
                }
            }
        }
    }

    /// Với route `Sharded`, giải shard → `(dsn, repo_id)` để probe/open.
    #[cfg(any(feature = "postgres", feature = "mysql"))]
    fn sharded_target(&self) -> Option<(String, u64)> {
        let route = self.route.as_ref()?;
        match route {
            StorageRoute::Sharded { dsns, repo_id, .. } => {
                let repo_id = (*repo_id)?;
                let shard = route.shard_of(repo_id)?;
                let dsn = dsns.get(shard)?;
                Some((dsn.clone(), repo_id))
            }
            _ => None,
        }
    }

    /// Version index trên đĩa hiện tại — `None` nếu probe thất bại (store chưa
    /// có hoặc đang bị re-index), hay backend không probe độc lập được (redis/
    /// in-memory/unknown scheme).
    async fn current_version(&self) -> Option<u64> {
        match self.backend_scheme()? {
            #[cfg(feature = "sqlite")]
            "sqlite" => {
                let dsn = match &self.route {
                    Some(StorageRoute::Local(d)) => d.as_str(),
                    _ => return None,
                };
                crate::storage::sqlite::SqliteStorage::probe_version(trim_scheme(dsn))
                    .await
                    .ok()
            }
            #[cfg(feature = "lmdb")]
            "lmdb" => {
                let dsn = match &self.route {
                    Some(StorageRoute::Local(d)) => d.as_str(),
                    _ => return None,
                };
                crate::storage::lmdb::probe_version(trim_scheme(dsn))
                    .await
                    .ok()
            }
            #[cfg(feature = "postgres")]
            "postgres" => {
                let (dsn, repo_id) = self.sharded_target()?;
                crate::storage::postgres::PostgresStorage::probe_version(&dsn, repo_id)
                    .await
                    .ok()
            }
            #[cfg(feature = "mysql")]
            "mysql" => {
                let (dsn, repo_id) = self.sharded_target()?;
                crate::storage::mysql::MySqlStorage::probe_version(&dsn, repo_id)
                    .await
                    .ok()
            }
            _ => None,
        }
    }

    /// Snapshot hiện tại có khớp version trên đĩa không. In-memory (không file)
    /// → không có writer ngoài → luôn fresh. Backend không probe được (redis/
    /// unknown scheme) → coi là stale để rebuilt lại.
    async fn is_fresh(&self, version: u64) -> bool {
        if self.route.is_none() {
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

    /// Build index từ route hiện tại rồi swap snapshot (gọi trong `rebuild_lock`).
    /// `GraphIndex::open_route` tự route theo scheme — không cần nhánh cfg.
    async fn rebuild_inner(&self) -> Result<()> {
        let index = match &self.route {
            Some(route) => GraphIndex::open_route(route).await?,
            None => GraphIndex::in_memory(),
        };
        let version = index.version();
        let mut state = self.state.write().await;
        state.index = Arc::new(index);
        state.version = version;
        state.ready = true;
        Ok(())
    }

    /// Đọc counts tổng hợp từ đĩa (`sg_stats`) mà KHÔNG rebuild in-memory
    /// `GraphIndex` — O(1) với repo lớn. Hỗ trợ sqlite/lmdb/postgres/mysql;
    /// backend khác / index cũ thiếu bảng → trả `None` để caller fallback rebuild.
    ///
    /// Trả `None` cả khi counts toàn 0 (index cũ chưa ghi `sg_stats`) để không
    /// trình ra số 0 sai lệch.
    pub async fn stats_cached(&self) -> Option<SemgraphStats> {
        let storage = self.stats_storage_handle().await?;
        let counts = storage.stats().await.ok()?;
        if counts.symbols == 0 && counts.chains == 0 && counts.edges == 0 && counts.files == 0 {
            return None;
        }
        Some(SemgraphStats {
            symbols: counts.symbols,
            chains: counts.chains,
            edges: counts.edges,
            files: counts.files,
            next_id: counts.next_id,
        })
    }

    /// Lấy (và cache) storage read-only từ route để đọc `sg_stats` không rebuild.
    /// Hỗ trợ: sqlite (Local), lmdb (Local, read-only để không tranh lock),
    /// postgres/mysql (Sharded — resolve dsn đầu + repo_id). Backend khác
    /// (redis/unknown) → `None` → caller fallback rebuild.
    async fn stats_storage_handle(&self) -> Option<Arc<dyn Storage>> {
        {
            let g = self.stats_storage.read().await;
            if let Some(s) = g.as_ref() {
                return Some(s.clone());
            }
        }
        let st: Arc<dyn Storage> = match &self.route {
            #[cfg(feature = "sqlite")]
            Some(StorageRoute::Local(d)) if d.starts_with("sqlite://") => {
                let s = crate::storage::sqlite::SqliteStorage::open(trim_scheme(d))
                    .await
                    .ok()?;
                Arc::new(s)
            }
            #[cfg(feature = "lmdb")]
            Some(StorageRoute::Local(d)) if d.starts_with("lmdb://") => {
                let s = crate::storage::lmdb::LmdbStorage::open(trim_scheme(d))
                    .await
                    .ok()?;
                Arc::new(s)
            }
            #[cfg(any(feature = "postgres", feature = "mysql"))]
            Some(StorageRoute::Sharded { dsns, repo_id, .. }) => {
                let dsn = dsns.first()?;
                let rid = (*repo_id)?;
                if dsn.starts_with("postgres://") {
                    #[cfg(feature = "postgres")]
                    {
                        let s = crate::storage::postgres::PostgresStorage::open(dsn, rid)
                            .await
                            .ok()?;
                        Arc::new(s)
                    }
                    #[cfg(not(feature = "postgres"))]
                    {
                        return None;
                    }
                } else if dsn.starts_with("mysql://") {
                    #[cfg(feature = "mysql")]
                    {
                        let s = crate::storage::mysql::MySqlStorage::open(dsn, rid)
                            .await
                            .ok()?;
                        Arc::new(s)
                    }
                    #[cfg(not(feature = "mysql"))]
                    {
                        return None;
                    }
                } else {
                    return None;
                }
            }
            _ => return None,
        };
        *self.stats_storage.write().await = Some(st.clone());
        Some(st)
    }
}

/// Bỏ `scheme://` khỏi DSN — trả phần còn lại (path cho probe file).
#[cfg(any(feature = "sqlite", feature = "lmdb"))]
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

    /// `stats_cached` đọc counts từ đĩa (`sg_stats`) mà KHÔNG rebuild in-memory
    /// `GraphIndex` — xác nhận `codegraph_status` tức thì trên repo lớn.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn sqlite_stats_cached_reads_disk_without_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.sqlite");
        let db_str = format!("sqlite://{}", db_path.to_string_lossy());

        // Index qua process riêng — ghi `sg_stats` lúc ingest.
        {
            let mut idx = GraphIndex::open(&db_str).await.unwrap();
            let r = mk_result(
                "a.ts",
                vec![sym("a", SYMBOL_BASE), sym("b", SYMBOL_BASE + 1)],
                vec![SYMBOL_BASE, SYMBOL_BASE + 1],
            );
            idx.ingest(&[r]).await.unwrap();
        }

        let sgi = SharedGraphIndex::open(Some(db_str.clone())).await.unwrap();
        // Chưa gọi `ensure_fresh` — `stats_cached` mở storage riêng đọc `sg_stats`.
        let stats = sgi.stats_cached().await.expect("sg_stats đã populate");
        assert_eq!(stats.symbols, 2);
        assert_eq!(stats.chains, 1);
        assert_eq!(stats.edges, 1);
        assert_eq!(stats.files, 1);
        assert_eq!(stats.next_id, SYMBOL_BASE + 2);
    }
}
