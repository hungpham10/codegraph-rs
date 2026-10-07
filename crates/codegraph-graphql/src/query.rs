//! Query resolvers — expose toàn bộ năng lực đọc của `GraphApi` dưới dạng
//! GraphQL có field-selection. Mọi type domain là `codegraph_core` (đã derive
//! GraphQL gated), nên resolver trả trực tiếp core type, không mirror.

use async_graphql::{Context, Object, Result as GqlResult, ID};
use codegraph_api::GraphApi;
use codegraph_core::{
    ClassInfo, DependenciesReport, FileInfo, FlowResult, FunctionScope, SearchFlowResult,
    SemgraphStats, Symbol, SymbolKind, SymbolMatch,
};
use codegraph_docs::tokenize::DocToken;
use codegraph_source::GitRepo;
use std::sync::Arc;

use crate::types::*;
use crate::AppState;

/// Parse GraphQL `ID` (string) thành `u64` symbol id.
fn parse_id(id: &ID) -> GqlResult<u64> {
    id.parse::<u64>()
        .map_err(|_| async_graphql::Error::new(format!("invalid id: {id:?}")))
}

/// Session đã bind root + index đã refresh xong.
///
/// `GraphIndex::rebuild` mất hàng chục giây với repo lớn. Server warm index
/// ố task nến lúc khởi động, nên Ồ đây ta **không chờ** rebuild
/// mà trả lỗi rõ ràng — request treo chờ rebuild làm UI trông như treo máy.
pub(crate) async fn fresh_index(
    ctx: &Context<'_>,
) -> GqlResult<Arc<codegraph_graph::SharedGraphIndex>> {
    let state = ctx.data::<Arc<AppState>>()?;
    let sgi = state
        .session
        .ensure_ready()
        .await
        .map_err(|e| async_graphql::Error::new(e.to_string()))?;
    if sgi.fresh_snapshot().await.is_none() {
        return Err(async_graphql::Error::new(
            "index chưa refresh xong (đang rebuild từ storage) — thử lại sau vài giây",
        ));
    }
    Ok(sgi)
}

/// Build một `GraphApi` trên snapshot index mới nhất của session hiện tại.
async fn api_for(ctx: &Context<'_>) -> GqlResult<GraphApi> {
    let sgi = fresh_index(ctx).await?;
    Ok(GraphApi::new_with_sessions(
        sgi,
        ctx.data::<Arc<AppState>>()?.search_sessions.clone(),
    ))
}

/// Workspace root của session (nếu đã bind) — làm root cho `DiskSource` ở
/// đường query `include_source`.
async fn state_root(ctx: &Context<'_>) -> Option<camino::Utf8PathBuf> {
    let state = ctx.data::<Arc<AppState>>().ok()?;
    state.session.root().await
}

/// Clamp + default paging args.
fn paging(limit: Option<i32>, offset: Option<i32>) -> (u32, u32) {
    let limit = limit.unwrap_or(50).clamp(1, 500) as u32;
    let offset = offset.unwrap_or(0).max(0) as u32;
    (limit, offset)
}

pub struct Query;

#[Object]
impl Query {
    // ── Symbol lookup ──

    /// Symbol theo `id`, hoặc resolve theo `name` nếu chỉ truyền `name`. Gộp cũ
    /// `symbol` (id) + `resolve` (name) thành 1 entry.
    async fn symbol(
        &self,
        ctx: &Context<'_>,
        id: Option<ID>,
        name: Option<String>,
    ) -> GqlResult<Option<Symbol>> {
        let api = api_for(ctx).await?;
        match id {
            Some(i) => {
                let i = parse_id(&i)?;
                Ok(api.symbol_by_id(i).await)
            }
            None => match name {
                Some(n) => Ok(api
                    .resolve(&n, 0)
                    .await
                    .map_err(|e| async_graphql::Error::new(e.to_string()))?
                    .symbol),
                None => Err(async_graphql::Error::new("provide `id` or `name`")),
            },
        }
    }

    /// Search symbol nâng cao (resumable + deadline-aware). `mode` mặc định
    /// CONTAINS; `resume` lấy từ query trước khi `timedOut`/`hasMore`.
    async fn search_symbol(
        &self,
        ctx: &Context<'_>,
        input: SearchSymbolInput,
    ) -> GqlResult<SearchSymbolResult> {
        let api = api_for(ctx).await?;
        let mode = input.mode.unwrap_or(SymbolMatch::Contains);
        let (limit, offset) = paging(input.limit, input.offset);
        let timeout = input.timeout_ms.unwrap_or(0).max(0) as u64;
        let out = api
            .search_symbol_paged_resumable(
                &input.query,
                input.kind,
                mode,
                codegraph_api::Pagination { limit, offset },
                input.resume,
                timeout,
            )
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        Ok(SearchSymbolResult {
            symbols: out.page,
            total: out.total as u64,
            timed_out: out.timed_out,
            resume: out.resume,
            index_version: out.index_version,
        })
    }

    // ── Call graph ──

    /// Callers (transitive BFS) của một symbol — `depth` hop tối đa (1 = direct).
    async fn callers(
        &self,
        ctx: &Context<'_>,
        id: ID,
        depth: Option<i32>,
    ) -> GqlResult<Vec<Symbol>> {
        let id = parse_id(&id)?;
        let depth = depth.unwrap_or(1).max(1) as u32;
        api_for(ctx)
            .await?
            .callers(id, depth)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))
    }

    /// Callees trực tiếp (đọc chain, skip marker/self).
    async fn callees(&self, ctx: &Context<'_>, id: ID) -> GqlResult<Vec<Symbol>> {
        let id = parse_id(&id)?;
        api_for(ctx)
            .await?
            .callees(id)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))
    }

    /// Impact: ai phụ thuộc (transitive callers) tới symbol này.
    async fn impact(
        &self,
        ctx: &Context<'_>,
        id: ID,
        max_depth: Option<i32>,
    ) -> GqlResult<Vec<Symbol>> {
        let id = parse_id(&id)?;
        let max_depth = max_depth.unwrap_or(3).max(1) as u32;
        api_for(ctx)
            .await?
            .impact(id, max_depth)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))
    }

    // ── Flow + Mermaid ──

    /// Flow của một symbol — chain render (marker + callee) + call edges.
    async fn flow(&self, ctx: &Context<'_>, id: ID) -> GqlResult<Option<FlowResult>> {
        let id = parse_id(&id)?;
        api_for(ctx)
            .await?
            .flow(id)
            .await
            .map(Some)
            .map_err(|e| async_graphql::Error::new(e.to_string()))
    }

    /// Diagram Mermaid cho một symbol — biến thể hình ảnh của `flow` /
    /// `callers` / `callees` / `impact`. `kind` chọn loại diagram; `depth` (mặc
    /// định 1) giới hạn BFS hop cho callers/callees/impact (bị bỏ qua với flow).
    /// Chỉ hoạt động khi server bật `--mermaid`; tắt → lỗi rõ ràng.
    async fn mermaid(
        &self,
        ctx: &Context<'_>,
        id: ID,
        kind: MermaidKind,
        depth: Option<i32>,
    ) -> GqlResult<String> {
        let state = ctx.data::<Arc<AppState>>()?;
        if !state.mermaid {
            return Err(async_graphql::Error::new(
                "Mermaid output is disabled. Start the GraphQL server with --mermaid to enable diagram rendering.",
            ));
        }
        let id = parse_id(&id)?;
        let depth = depth.unwrap_or(1).max(1) as u32;
        let api = api_for(ctx).await?;
        let diagram = match kind {
            MermaidKind::Flow => {
                let flow = api
                    .flow(id)
                    .await
                    .map_err(|e| async_graphql::Error::new(e.to_string()))?;
                codegraph_api::mermaid::control_flow(&flow)
            }
            MermaidKind::Callers => codegraph_api::mermaid::callers_mermaid(&api, id, depth)
                .await
                .map_err(|e| async_graphql::Error::new(e.to_string()))?,
            MermaidKind::Callees => codegraph_api::mermaid::callees_mermaid(&api, id, depth)
                .await
                .map_err(|e| async_graphql::Error::new(e.to_string()))?,
            MermaidKind::Impact => codegraph_api::mermaid::impact_mermaid(&api, id, depth)
                .await
                .map_err(|e| async_graphql::Error::new(e.to_string()))?,
        };
        Ok(diagram)
    }

    /// Functions có chain chứa pattern (id/marker/tên symbol, cách nhau bởi `,`).
    async fn search_flow(
        &self,
        ctx: &Context<'_>,
        pattern: String,
        limit: Option<i32>,
        offset: Option<i32>,
    ) -> GqlResult<FlowSearchResult> {
        let (limit, offset) = paging(limit, offset);
        let mut results = api_for(ctx)
            .await?
            .search_flow_pattern(&pattern)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        let total = results.len() as u64;
        // Slice thủ công (search_flow_pattern trả toàn bộ matches).
        let start = (offset as usize).min(results.len());
        let end = (start + limit as usize).min(results.len());
        let page: Vec<SearchFlowResult> = results.drain(start..end).collect();
        // has_more: còn phần tử sau trang này?
        let has_more = (offset as usize + page.len()) < total as usize;
        Ok(FlowSearchResult {
            results: page,
            total,
            has_more,
        })
    }

    /// Functions gọi một library call có tên chứa `query` (kể cả unresolved).
    async fn references(
        &self,
        ctx: &Context<'_>,
        query: String,
        limit: Option<i32>,
    ) -> GqlResult<ReferencesResult> {
        let (limit, _offset) = paging(limit, None);
        let results = api_for(ctx)
            .await?
            .references(&query, limit)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        let total = results.len() as u64;
        let has_more = limit as usize <= results.len();
        Ok(ReferencesResult {
            results,
            total,
            has_more,
        })
    }

    // ── Context (markdown/json) — chỉ field `include_source:true` trả raw source ──

    /// Context xung quanh một symbol/query — markdown hoặc json. **Mặc định
    /// không bao gồm raw source**; chỉ khi `req.includeSource = true` mới trả
    /// source (do UI/người dùng tự quyết định) — giữ data on-prem.
    async fn context(&self, ctx: &Context<'_>, req: ContextRequestInput) -> GqlResult<String> {
        let api = api_for(ctx).await?;
        let core_req: codegraph_context::ContextRequest = req.into();
        // Đường query đọc qua `Source` trait. HTTP/GraphQL không luôn có root
        // (session chưa bind) → `None` ⇒ hit vẫn trả về, `source` rỗng.
        let src = state_root(ctx)
            .await
            .map(codegraph_source::DiskSource::for_query);
        api.context_markdown(
            &core_req,
            src.as_ref().map(|s| s as &dyn codegraph_source::Source),
        )
        .await
        .map_err(|e| async_graphql::Error::new(e.to_string()))
    }

    // ── Class / scope / files ──

    /// Files trong graph, filter theo prefix đường dẫn.
    async fn graphcode_files(
        &self,
        ctx: &Context<'_>,
        prefix: Option<String>,
    ) -> GqlResult<Vec<FileInfo>> {
        let prefix = prefix.unwrap_or_default();
        Ok(api_for(ctx).await?.files(&prefix).await)
    }

    /// Thông số index (symbols/chains/edges/files/next_id) — health check.
    async fn status(&self, ctx: &Context<'_>) -> GqlResult<SemgraphStats> {
        let state = ctx.data::<Arc<AppState>>()?;
        let sgi = state
            .session
            .ensure_ready()
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        // Đọc counts từ `sg_stats` trên đĩa (O(1)) — không cần index in-memory,
        // nên vẫn trả lỗi đẵ server cóng đang rebuild. UI poll query này
        // để biết khi náo warm xong.
        Ok(sgi.stats_cached().await.unwrap_or(SemgraphStats {
            symbols: 0,
            chains: 0,
            edges: 0,
            files: 0,
            next_id: 0,
        }))
    }

    /// Class info: symbol + fields + methods.
    async fn graphcode_class(&self, ctx: &Context<'_>, id: ID) -> GqlResult<Option<ClassInfo>> {
        let id = parse_id(&id)?;
        Ok(api_for(ctx).await?.class_info(id).await)
    }

    /// Liệt kê symbol theo kind (CLASS / INTERFACE / ENUM), phân trang. Gộp cũ
    /// `list_classes` / `list_interfaces` / `list_enums` thành 1 resolver.
    async fn graphcode_list_types(
        &self,
        ctx: &Context<'_>,
        kind: TypeKind,
        limit: Option<i32>,
        offset: Option<i32>,
    ) -> GqlResult<ListResult> {
        let (limit, offset) = paging(limit, offset);
        let sk = match kind {
            TypeKind::Class => SymbolKind::Class,
            TypeKind::Interface => SymbolKind::Interface,
            TypeKind::Enum => SymbolKind::Enum,
        };
        let (items, total) = api_for(ctx).await?.list_by_kind(sk, limit, offset).await;
        let has_more = (offset as usize + items.len()) < total;
        Ok(ListResult {
            items,
            total: total as u64,
            has_more,
        })
    }

    /// Liệt kê symbol theo kind (mọi SymbolKind), phân trang. Dùng cho Browse.
    async fn graphcode_list_symbols(
        &self,
        ctx: &Context<'_>,
        kind: SymbolKind,
        limit: Option<i32>,
        offset: Option<i32>,
    ) -> GqlResult<ListResult> {
        let (limit, offset) = paging(limit, offset);
        let (items, total) = api_for(ctx).await?.list_by_kind(kind, limit, offset).await;
        let has_more = (offset as usize + items.len()) < total;
        Ok(ListResult {
            items,
            total: total as u64,
            has_more,
        })
    }

    /// Scope của function (parameters + locals).
    async fn graphcode_function_scope(
        &self,
        ctx: &Context<'_>,
        id: ID,
    ) -> GqlResult<Option<FunctionScope>> {
        let id = parse_id(&id)?;
        Ok(api_for(ctx).await?.function_scope(id).await)
    }

    // ── Annotations / dependencies ──

    /// Tìm symbol theo annotation (vd `@Override`, `@Cacheable`).
    async fn graphcode_search_by_annotation(
        &self,
        ctx: &Context<'_>,
        annotation: String,
        kind: Option<SymbolKind>,
        limit: Option<i32>,
        offset: Option<i32>,
    ) -> GqlResult<AnnotationSearchResult> {
        let (limit, offset) = paging(limit, offset);
        let (symbols, total, truncated) = api_for(ctx)
            .await?
            .search_by_annotation(&annotation, kind, offset, limit)
            .await;
        Ok(AnnotationSearchResult {
            symbols,
            total: total as u64,
            has_more: truncated,
        })
    }

    /// Dependencies ước lượng từ call names (internal/external/total).
    async fn graphcode_dependencies(&self, ctx: &Context<'_>) -> GqlResult<DependenciesReport> {
        Ok(api_for(ctx).await?.dependencies().await)
    }

    /// Liệt kê branch local của workspace (cho branch compare trong Review).
    async fn git_branches(&self, ctx: &Context<'_>) -> GqlResult<Vec<String>> {
        let state = ctx.data::<Arc<AppState>>()?;
        let root = state
            .session
            .root()
            .await
            .ok_or_else(|| async_graphql::Error::new("session root unavailable"))?;
        let git = codegraph_source::DiskGit::new(root);
        // Không phải repo (hoặc lỗi) → trả rỗng thay vì lỗi cứng.
        Ok(git.branches().await.unwrap_or_default())
    }

    // ── Document queries ──

    /// Module UI đang bật (từ config server) — sidebar lọc theo danh sách này.
    async fn ui_config(&self, ctx: &Context<'_>) -> GqlResult<Vec<String>> {
        let state = ctx.data::<Arc<AppState>>()?;
        Ok(state.ui_modules.clone())
    }

    /// Thống kê document graph (số doc + node).
    async fn graphdoc_stats(&self, ctx: &Context<'_>) -> GqlResult<DocStatsView> {
        let state = ctx.data::<Arc<AppState>>()?;
        let graph = state.doc_graph.graph().await;
        let graph = graph.read().await;
        let stats = graph
            .stats()
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        Ok(DocStatsView {
            docs: stats.docs,
            nodes: stats.nodes,
        })
    }

    /// Liệt kê mọi document (path, format, số node).
    async fn graphdoc_list(&self, ctx: &Context<'_>) -> GqlResult<Vec<DocInfoView>> {
        let state = ctx.data::<Arc<AppState>>()?;
        let graph = state.doc_graph.graph().await;
        let graph = graph.read().await;
        Ok(graph.list_docs().into_iter().map(Into::into).collect())
    }

    /// Search node theo pattern đường dẫn (vd `spec.replicas`) hoặc fuzzy key.
    /// Trả node đã hydrate (có path) — depth = số tầng con kế tiếp.
    async fn graphdoc_search(
        &self,
        ctx: &Context<'_>,
        pattern: String,
        depth: Option<i32>,
    ) -> GqlResult<Vec<DocNodePayload>> {
        let state = ctx.data::<Arc<AppState>>()?;
        let depth = depth.unwrap_or(1).max(1) as usize;
        let graph = state.doc_graph.graph().await;
        let graph = graph.read().await;

        // Pattern "spec.replicas" → [root, FIELD(spec), FIELD(replicas)].
        let mut tokens = vec![DocToken::root()];
        let mut unknown_seg = false;
        let mut fuzzy_seg: Option<String> = None;
        for seg in pattern.split('.') {
            if let Some(fz) = seg.strip_prefix('~') {
                fuzzy_seg = Some(fz.to_string());
                break;
            }
            match graph.intern_id(seg) {
                Some(id) => tokens.push(DocToken::field(id)),
                None => {
                    unknown_seg = true;
                    break;
                }
            }
        }

        let mut ids: Vec<u64> = Vec::new();
        if !unknown_seg && fuzzy_seg.is_none() {
            let mut found = graph
                .search_path(&tokens, Some(tokens.len() - 1 + depth))
                .await
                .unwrap_or_default();
            found.extend(graph.search_path_scan(&tokens, 100));
            found.sort_unstable();
            found.dedup();
            ids = found;
        }

        if ids.is_empty() {
            // Fallback: fuzzy key substring.
            let last = fuzzy_seg
                .clone()
                .unwrap_or_else(|| pattern.rsplit('.').next().unwrap_or(&pattern).to_string());
            let hits = graph.search_key_fuzzy(&last, 50);
            let mut results = Vec::new();
            for h in hits {
                if let Some(payload) = graph.hydrate_depth(h.node.id, Some(1)).await {
                    results.push(payload.into());
                }
            }
            return Ok(results);
        }

        let mut results = Vec::new();
        for id in ids.iter().take(100) {
            if let Some(payload) = graph.hydrate_depth(*id, Some(1)).await {
                results.push(payload.into());
            }
        }
        Ok(results)
    }

    /// Hydrate một node theo id (cây con theo max_depth).
    async fn graphdoc_hydrate(
        &self,
        ctx: &Context<'_>,
        id: ID,
        max_depth: Option<i32>,
    ) -> GqlResult<Option<DocNodePayload>> {
        let state = ctx.data::<Arc<AppState>>()?;
        let id = id
            .parse::<u64>()
            .map_err(|_| async_graphql::Error::new("invalid id"))?;
        let max_depth = max_depth.map(|d| d.max(0) as usize);
        let graph = state.doc_graph.graph().await;
        let graph = graph.read().await;
        Ok(graph.hydrate_depth(id, max_depth).await.map(Into::into))
    }

    /// Search theo giá trị scalar (substring).
    async fn graphdoc_search_value(
        &self,
        ctx: &Context<'_>,
        query: String,
        limit: Option<i32>,
    ) -> GqlResult<Vec<DocNodePayload>> {
        let state = ctx.data::<Arc<AppState>>()?;
        let limit = limit.unwrap_or(50).max(1) as usize;
        let graph = state.doc_graph.graph().await;
        let graph = graph.read().await;
        let hits = graph.search_value_substring(&query, limit);
        let mut results = Vec::new();
        for n in hits {
            if let Some(payload) = graph.hydrate_depth(n.id, Some(1)).await {
                results.push(payload.into());
            }
        }
        Ok(results)
    }

    /// Liệt kê structural patterns đã mine.
    async fn graphdoc_list_patterns(&self, ctx: &Context<'_>) -> GqlResult<Vec<DocPatternView>> {
        let state = ctx.data::<Arc<AppState>>()?;
        let graph = state.doc_graph.graph().await;
        let graph = graph.read().await;
        Ok(graph.list_patterns().into_iter().map(Into::into).collect())
    }
}
