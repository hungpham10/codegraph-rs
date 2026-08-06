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
use codegraph_graph::{GraphIndex, SharedGraphIndex};
use std::sync::Arc;

pub struct GraphApi {
    shared_index: Arc<SharedGraphIndex>,
}

impl GraphApi {
    pub fn new_with_index(index: Arc<SharedGraphIndex>) -> Self {
        Self {
            shared_index: index,
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
