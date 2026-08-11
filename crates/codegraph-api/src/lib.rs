//! Shared graph query API cho MCP + visualize HTTP server.
//!
//! `GraphApi` wrap `Arc<SharedGraphIndex>` — mọi query chạy trên snapshot index
//! mới nhất (`ensure_fresh`: rebuild khi version file đổi). Query surface mới
//! của semgraph: search/symbol/flow/search_flow/callers/callees/references.

use codegraph_context::ContextRequest;
use codegraph_core::{
    CallSiteResult, ClassInfo, DependenciesReport, Error, FileInfo, FlowResult, FunctionScope,
    MemberInfo, ResolveResult, Result, SearchFlowResult, Symbol, SymbolKind, SymbolMatch,
};
use codegraph_graph::{GraphIndex, SearchCursor, SharedGraphIndex};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub struct GraphApi {
    shared_index: Arc<SharedGraphIndex>,
    /// Session store cho search resumable (resume id → cursor).
    sessions: Arc<SearchSessionStore>,
}

// ==================== Search session store ====================

/// Cursor session lưu **phía server** — LLM chỉ cầm một id ngắn (hex) và echo
/// lại khi retry. Id vô nghĩa ngoài tiến trình này: index version đổi (re-ingest)
/// hoặc server restart → session stale, báo LLM retry không có `resume`.
struct StoredResume {
    created: Instant,
    /// Version index lúc tạo — đổi (re-ingest) → cursor mất giá trị.
    index_version: u64,
    cursor: SearchCursor,
}

/// Store in-process cho resume id → cursor. Không persist; purge theo TTL khi
/// `put` (đủ cho use-case retry trong vài phút).
pub struct SearchSessionStore {
    inner: Mutex<HashMap<String, StoredResume>>,
    ttl: Duration,
    max_sessions: usize,
    next_id: AtomicU64,
}

impl Default for SearchSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchSessionStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            ttl: Duration::from_secs(600),
            max_sessions: 512,
            next_id: AtomicU64::new(0),
        }
    }

    /// Lưu cursor, trả id hex ngắn. Trước khi thêm: purge session quá TTL, chặn
    /// số session tối đa (evict session già nhất).
    pub fn put(&self, cursor: SearchCursor, index_version: u64) -> String {
        let mut map = self.inner.lock().unwrap();
        let now = Instant::now();
        map.retain(|_, s| now.duration_since(s.created) < self.ttl);
        while map.len() >= self.max_sessions {
            let oldest = map
                .iter()
                .min_by_key(|(_, s)| s.created)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                map.remove(&k);
            } else {
                break;
            }
        }
        let id = Self::gen_id(&self.next_id);
        map.insert(
            id.clone(),
            StoredResume {
                created: now,
                index_version,
                cursor,
            },
        );
        id
    }

    /// Đọc cursor theo id — `None` nếu không có / quá TTL.
    pub fn get(&self, id: &str) -> Option<(u64, SearchCursor)> {
        let map = self.inner.lock().unwrap();
        map.get(id).map(|s| (s.index_version, s.cursor.clone()))
    }

    /// Xoá session (khi search hoàn tất, không còn page nào).
    pub fn remove(&self, id: &str) {
        self.inner.lock().unwrap().remove(id);
    }

    /// Id hex 16 ký tự: epoch-nanos + counter tiến trình — đủ unique trong
    /// tiến trình, không cần crate random.
    fn gen_id(counter: &AtomicU64) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = counter.fetch_add(1, Ordering::Relaxed);
        let v = (nanos as u64) ^ (n.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        format!("{:016x}", v)
    }
}

/// Kết quả search resumable từ `GraphApi` — tầng MCP dựng message từ đây.
#[derive(Debug)]
pub struct ResumeSearchOutcome {
    pub page: Vec<Symbol>,
    pub total: usize,
    pub timed_out: bool,
    /// Số đơn vị đã xử lý lúc ngắt (names khi đang phase A, symbols khi phase B).
    pub progress: usize,
    /// Resume id để retry: `Some` khi timed_out HOẶC còn page sau. `None` =
    /// xong và hết page (session đã xoá).
    pub resume: Option<String>,
    /// Version index mà search chạy trên — đổi giữa các lần retry → resume
    /// không còn giá trị.
    pub index_version: u64,
}

impl GraphApi {
    pub fn new_with_index(index: Arc<SharedGraphIndex>) -> Self {
        Self {
            shared_index: index,
            sessions: Arc::new(SearchSessionStore::new()),
        }
    }

    /// Dùng chung session store (resume id) — server MCP giữ store ở vòng đời
    /// server để resume id sống qua nhiều tool call.
    pub fn new_with_sessions(index: Arc<SharedGraphIndex>, sessions: Arc<SearchSessionStore>) -> Self {
        Self {
            shared_index: index,
            sessions,
        }
    }

    /// Snapshot index mới nhất (rebuild nếu stale) — mọi query chạy trên đây.
    pub async fn index(&self) -> Arc<GraphIndex> {
        self.shared_index.ensure_fresh().await
    }

    /// Search symbol theo tên (substring, case-insensitive).
    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<Symbol>> {
        self.index()
            .await
            .search_symbol(query, None, limit as usize)
            .await
    }

    /// Resumable + deadline-aware của [`Self::search`] — nền cho
    /// `codegraph_search`. `timeout_ms = 0` = không giới hạn thời gian.
    /// `resume` = id trả về từ lần timeout trước (phải cùng query).
    pub async fn search_resumable(
        &self,
        query: &str,
        limit: u32,
        resume: Option<String>,
        timeout_ms: u64,
    ) -> Result<ResumeSearchOutcome> {
        self.search_symbol_paged_resumable(
            query,
            None,
            SymbolMatch::Contains,
            limit,
            0,
            resume,
            timeout_ms,
        )
        .await
    }

    /// Search symbol nâng cao — kind filter + match mode + phân trang.
    /// Trả về (page, total).
    pub async fn search_symbol_paged(
        &self,
        query: &str,
        kind: Option<SymbolKind>,
        mode: SymbolMatch,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<Symbol>, usize)> {
        self.index()
            .await
            .search_symbol_paged(query, kind, mode, limit as usize, offset as usize)
            .await
    }

    /// Resumable + deadline-aware của [`Self::search_symbol_paged`] — nền cho
    /// `codegraph_search_symbol`. `timeout_ms = 0` = không giới hạn.
    ///
    /// `resume` được validate (index version + query/mode/kind phải khớp) —
    /// sai → lỗi báo LLM retry không có `resume`.
    pub async fn search_symbol_paged_resumable(
        &self,
        query: &str,
        kind: Option<SymbolKind>,
        mode: SymbolMatch,
        limit: u32,
        offset: u32,
        resume: Option<String>,
        timeout_ms: u64,
    ) -> Result<ResumeSearchOutcome> {
        let idx = self.index().await;
        let version = idx.version();
        let q = query.to_lowercase();

        // ── Validate resume id (nếu có) ──
        let cursor = match &resume {
            Some(id) => {
                let (stored_version, stored) = self
                    .sessions
                    .get(id)
                    .ok_or_else(|| Error::Invalid("resume id expired or unknown — retry without resume".into()))?;
                if stored_version != version {
                    return Err(Error::Invalid(
                        "index was re-built since this resume was created — retry without resume"
                            .into(),
                    ));
                }
                if stored.query != q || stored.mode != mode || stored.kind != kind {
                    return Err(Error::Invalid(
                        "resume id was created for a different query — retry without resume".into(),
                    ));
                }
                Some(stored)
            }
            None => None,
        };

        // ── Deadline ──
        let deadline = if timeout_ms == 0 {
            None
        } else {
            Some(Instant::now() + Duration::from_millis(timeout_ms))
        };

        let out = idx
            .search_symbol_paged_resumable(
                query,
                kind,
                mode,
                limit as usize,
                offset as usize,
                cursor,
                deadline,
            )
            .await?;

        // ── Quản lý session: lưu khi còn tiếp tục (timeout / còn page), xoá
        // khi xong hẳn. ──
        let resume_id = match &out.cursor {
            Some(c) => Some(self.sessions.put(c.clone(), version)),
            None => {
                if let Some(id) = &resume {
                    self.sessions.remove(id);
                }
                None
            }
        };

        Ok(ResumeSearchOutcome {
            page: out.page,
            total: out.total,
            timed_out: out.timed_out,
            progress: out.progress,
            resume: resume_id,
            index_version: version,
        })
    }

    /// Methods của class (compact projection).
    pub async fn class_methods(&self, id: u64) -> Vec<MemberInfo> {
        self.index().await.list_methods_of_class(id)
    }

    /// Class info: symbol + fields + methods.
    pub async fn class_info(&self, id: u64) -> Option<ClassInfo> {
        self.index().await.get_class_info(id)
    }

    /// Liệt kê symbol theo kind (class/interface/enum) — phân trang.
    pub async fn list_by_kind(
        &self,
        kind: SymbolKind,
        limit: u32,
        offset: u32,
    ) -> (Vec<Symbol>, usize) {
        self.index()
            .await
            .list_symbols_by_kind(kind, limit as usize, offset as usize)
    }

    /// Scope của function (parameters + locals).
    pub async fn function_scope(&self, id: u64) -> Option<FunctionScope> {
        self.index().await.function_scope(id)
    }

    /// Tìm symbol theo annotation — (page, total, truncated).
    pub async fn search_by_annotation(
        &self,
        annotation: &str,
        kind: Option<SymbolKind>,
        offset: u32,
        limit: u32,
    ) -> (Vec<Symbol>, usize, bool) {
        self.index()
            .await
            .search_by_annotation(annotation, kind, offset as usize, limit as usize)
    }

    /// Dependencies ước lượng từ call names.
    pub async fn dependencies(&self) -> DependenciesReport {
        self.index().await.dependencies_report()
    }

    /// Symbol theo id.
    pub async fn symbol_by_id(&self, id: u64) -> Option<Symbol> {
        self.index().await.symbol_by_id(id)
    }

    /// Resolve theo id hoặc tên chính xác — trùng tên → `ambiguous` + `matches`.
    pub async fn resolve(&self, name: &str, symbol_id: u64) -> Result<ResolveResult> {
        self.index().await.resolve_by_name_or_id(name, symbol_id)
    }

    /// Callers (transitive BFS) — `depth` = số hop tối đa (1 = direct).
    pub async fn callers(&self, id: u64, depth: u32) -> Result<Vec<Symbol>> {
        self.index().await.callers(id, depth as usize).await
    }

    /// Callees trực tiếp (đọc chain, skip marker/self).
    pub async fn callees(&self, id: u64) -> Result<Vec<Symbol>> {
        self.index().await.callees(id).await
    }

    /// Impact: ai phụ thuộc (transitive callers) tới `max_depth`.
    pub async fn impact(&self, id: u64, max_depth: u32) -> Result<Vec<Symbol>> {
        self.index().await.callers(id, max_depth as usize).await
    }

    /// Flow của symbol — chain render (marker + callee) + call edges.
    pub async fn flow(&self, id: u64) -> Result<FlowResult> {
        self.index().await.flow(id).await
    }

    /// Tìm function có chain chứa pattern. Pattern là chuỗi token cách nhau bởi
    /// dấu phẩy; mỗi token là id số, tên marker (`LOOP`, `IF_TRUE`, ...) hoặc tên
    /// symbol (resolve exact — trùng tên lấy ứng viên đầu).
    pub async fn search_flow_pattern(&self, pattern: &str) -> Result<Vec<SearchFlowResult>> {
        let idx = self.index().await;
        let mut ids = Vec::new();
        for tok in pattern.split(',') {
            let t = tok.trim();
            if t.is_empty() {
                continue;
            }
            if let Ok(n) = t.parse::<u64>() {
                ids.push(n);
                continue;
            }
            if let Some(m) = codegraph_core::marker_id(t) {
                ids.push(m);
                continue;
            }
            let r = idx.resolve_by_name_or_id(t, 0)?;
            let sid = r
                .symbol
                .map(|s| s.id)
                .or_else(|| r.matches.first().map(|s| s.id))
                .ok_or_else(|| Error::Invalid(format!("unknown flow token: {t}")))?;
            ids.push(sid);
        }
        if ids.is_empty() {
            return Err(Error::Invalid("empty flow pattern".into()));
        }
        idx.search_flow(&ids).await
    }

    /// Functions gọi một library call có tên chứa `query` (kể cả call unresolved).
    pub async fn references(&self, query: &str, limit: u32) -> Result<Vec<CallSiteResult>> {
        self.index()
            .await
            .callers_by_call_name(query, limit as usize)
            .await
    }

    pub async fn context_markdown(&self, req: &ContextRequest) -> Result<String> {
        codegraph_context::build(&self.shared_index, req).await
    }

    /// Files trong graph (filter theo prefix đường dẫn).
    pub async fn files(&self, prefix: &str) -> Vec<FileInfo> {
        let files = self.index().await.files();
        if prefix.is_empty() {
            files
        } else {
            files
                .into_iter()
                .filter(|f| f.path.starts_with(prefix))
                .collect()
        }
    }

    pub async fn stats(&self) -> codegraph_core::SemgraphStats {
        self.index().await.stats()
    }
}
