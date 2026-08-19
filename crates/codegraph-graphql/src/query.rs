//! Query resolvers — expose toàn bộ năng lực đọc của `GraphApi` dưới dạng
//! GraphQL có field-selection. Mọi type domain là `codegraph_core` (đã derive
//! GraphQL gated), nên resolver trả trực tiếp core type, không mirror.

use async_graphql::{Context, Object, Result as GqlResult, ID};
use codegraph_api::GraphApi;
use codegraph_core::{
    ClassInfo, DependenciesReport, FileInfo, FlowResult, FunctionScope, SearchFlowResult,
    SemgraphStats, Symbol, SymbolKind, SymbolMatch,
};
use std::sync::Arc;

use crate::types::*;
use crate::AppState;

/// Parse GraphQL `ID` (string) thành `u64` symbol id.
fn parse_id(id: &ID) -> GqlResult<u64> {
    id.parse::<u64>()
        .map_err(|_| async_graphql::Error::new(format!("invalid id: {id:?}")))
}

/// Build một `GraphApi` trên snapshot index mới nhất của session hiện tại.
async fn api_for(ctx: &Context<'_>) -> GqlResult<GraphApi> {
    let state = ctx.data::<Arc<AppState>>()?;
    let sgi = state
        .session
        .ensure_ready()
        .await
        .map_err(|e| async_graphql::Error::new(e.to_string()))?;
    Ok(GraphApi::new_with_sessions(
        sgi,
        state.search_sessions.clone(),
    ))
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
        api.context_markdown(&core_req)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))
    }

    // ── Class / scope / files ──

    /// Files trong graph, filter theo prefix đường dẫn.
    async fn files(&self, ctx: &Context<'_>, prefix: Option<String>) -> GqlResult<Vec<FileInfo>> {
        let prefix = prefix.unwrap_or_default();
        Ok(api_for(ctx).await?.files(&prefix).await)
    }

    /// Thông số index (symbols/chains/edges/files/next_id) — health check.
    async fn status(&self, ctx: &Context<'_>) -> GqlResult<SemgraphStats> {
        Ok(api_for(ctx).await?.stats_cached().await)
    }

    /// Class info: symbol + fields + methods.
    async fn class(&self, ctx: &Context<'_>, id: ID) -> GqlResult<Option<ClassInfo>> {
        let id = parse_id(&id)?;
        Ok(api_for(ctx).await?.class_info(id).await)
    }

    /// Liệt kê symbol theo kind (CLASS / INTERFACE / ENUM), phân trang. Gộp cũ
    /// `list_classes` / `list_interfaces` / `list_enums` thành 1 resolver.
    async fn types(
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

    /// Scope của function (parameters + locals).
    async fn function_scope(&self, ctx: &Context<'_>, id: ID) -> GqlResult<Option<FunctionScope>> {
        let id = parse_id(&id)?;
        Ok(api_for(ctx).await?.function_scope(id).await)
    }

    // ── Annotations / dependencies ──

    /// Tìm symbol theo annotation (vd `@Override`, `@Cacheable`).
    async fn search_by_annotation(
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
    async fn dependencies(&self, ctx: &Context<'_>) -> GqlResult<DependenciesReport> {
        Ok(api_for(ctx).await?.dependencies().await)
    }
}
