use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use codegraph_api::GraphApi;
use codegraph_core::{Symbol, SymbolKind, SymbolMatch};
use codegraph_graph::{GraphIndex, SharedGraphIndex};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub shared_index: Arc<SharedGraphIndex>,
    pub boot_json: String,
}

#[derive(Deserialize)]
pub struct SearchParams {
    pub q: String,
    #[serde(default = "default_search_limit")]
    pub limit: u32,
    /// Lọc theo loại symbol (VD `function`) — `None` = tất cả (hành vi cũ).
    pub kind: Option<String>,
    /// Match mode: `contains` | `prefix` | `suffix` | `exact` (mặc định contains).
    #[serde(rename = "match")]
    pub mode: Option<String>,
}

fn default_search_limit() -> u32 {
    20
}

#[derive(Deserialize)]
pub struct SubgraphParams {
    pub seed: Option<u64>,
    pub query: Option<String>,
    #[serde(default = "default_depth")]
    pub depth: u32,
    pub limit: Option<u32>,
}

fn default_depth() -> u32 {
    2
}

#[derive(Deserialize)]
pub struct DepthParams {
    #[serde(default = "default_depth")]
    pub depth: u32,
}

#[derive(Deserialize)]
pub struct SearchFlowParams {
    pub pattern: String,
}

#[derive(Deserialize)]
pub struct FilesParams {
    pub prefix: Option<String>,
}

pub async fn status(State(state): State<AppState>) -> impl IntoResponse {
    let api = GraphApi::new_with_index(state.shared_index.clone());
    Json(api.stats().await)
}

pub async fn search(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> impl IntoResponse {
    let api = GraphApi::new_with_index(state.shared_index.clone());
    let kind = params.kind.as_deref().and_then(SymbolKind::parse);
    let mode = params
        .mode
        .as_deref()
        .and_then(SymbolMatch::parse)
        .unwrap_or(SymbolMatch::Contains);
    match api.search_symbol_paged(&params.q, kind, mode, params.limit, 0).await {
        Ok((hits, _total)) => Json(hits).into_response(),
        Err(e) => api_error(e),
    }
}

pub async fn symbol(State(state): State<AppState>, Path(id): Path<u64>) -> impl IntoResponse {
    let api = GraphApi::new_with_index(state.shared_index.clone());
    match api.symbol_by_id(id).await {
        Some(s) => Json(s).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "symbol not found" })),
        )
            .into_response(),
    }
}

pub async fn flow(State(state): State<AppState>, Path(id): Path<u64>) -> impl IntoResponse {
    let api = GraphApi::new_with_index(state.shared_index.clone());
    match api.flow(id).await {
        Ok(f) => Json(f).into_response(),
        Err(e) => api_error(e),
    }
}

pub async fn search_flow(
    State(state): State<AppState>,
    Query(params): Query<SearchFlowParams>,
) -> impl IntoResponse {
    let api = GraphApi::new_with_index(state.shared_index.clone());
    match api.search_flow_pattern(&params.pattern).await {
        Ok(hits) => Json(hits).into_response(),
        Err(e) => api_error(e),
    }
}

pub async fn callers(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Query(params): Query<DepthParams>,
) -> impl IntoResponse {
    let api = GraphApi::new_with_index(state.shared_index.clone());
    match api.callers(id, params.depth).await {
        Ok(hits) => Json(hits).into_response(),
        Err(e) => api_error(e),
    }
}

pub async fn callees(State(state): State<AppState>, Path(id): Path<u64>) -> impl IntoResponse {
    let api = GraphApi::new_with_index(state.shared_index.clone());
    match api.callees(id).await {
        Ok(hits) => Json(hits).into_response(),
        Err(e) => api_error(e),
    }
}

pub async fn files(
    State(state): State<AppState>,
    Query(params): Query<FilesParams>,
) -> impl IntoResponse {
    let api = GraphApi::new_with_index(state.shared_index.clone());
    Json(api.files(params.prefix.as_deref().unwrap_or("")).await)
}

/// Subgraph cho UI: BFS callers + callees quanh seed → nodes + call edges.
pub async fn subgraph(
    State(state): State<AppState>,
    Query(params): Query<SubgraphParams>,
) -> impl IntoResponse {
    let api = GraphApi::new_with_index(state.shared_index.clone());
    let idx = api.index().await;
    let depth = params.depth.max(1) as usize;
    let limit = params.limit.unwrap_or(300).max(1) as usize;

    let seed = if let Some(id) = params.seed {
        idx.symbol_by_id(id)
    } else if let Some(q) = params.query.as_deref().filter(|q| !q.is_empty()) {
        resolve_seed(&idx, q).await
    } else {
        None
    };
    let Some(seed) = seed else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no seed found" })),
        )
            .into_response();
    };

    let (nodes, edges, truncated) = bfs_neighbors(&idx, &seed, depth, limit).await;
    let nodes: Vec<Symbol> = nodes.into_values().collect();
    Json(json!({
        "nodes": nodes,
        "edges": edges,
        "seed": seed,
        "truncated": truncated,
    }))
    .into_response()
}

/// Neighbors quanh id (callers + callees) — shape giống subgraph để UI `mergeHits`
/// dùng chung (nút "⊕ Expand", `app.js` gọi `/api/neighbors/{id}`).
pub async fn neighbors(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Query(params): Query<DepthParams>,
) -> impl IntoResponse {
    let api = GraphApi::new_with_index(state.shared_index.clone());
    let idx = api.index().await;
    let Some(seed) = idx.symbol_by_id(id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "symbol not found" })),
        )
            .into_response();
    };
    let depth = params.depth.max(1) as usize;
    let (nodes, edges, truncated) = bfs_neighbors(&idx, &seed, depth, 300).await;
    let nodes: Vec<Symbol> = nodes.into_values().collect();
    Json(json!({
        "nodes": nodes,
        "edges": edges,
        "seed": seed,
        "truncated": truncated,
    }))
    .into_response()
}

/// BFS callers + callees quanh seed (tối đa `depth` hop) → nodes + call edges.
async fn bfs_neighbors(
    idx: &GraphIndex,
    seed: &Symbol,
    depth: usize,
    limit: usize,
) -> (HashMap<u64, Symbol>, Vec<serde_json::Value>, bool) {
    let mut nodes: HashMap<u64, Symbol> = HashMap::new();
    let mut edges: Vec<serde_json::Value> = Vec::new();
    nodes.insert(seed.id, seed.clone());
    let mut frontier = vec![seed.id];
    let mut truncated = false;
    for _ in 0..depth {
        let mut next = Vec::new();
        for &id in &frontier {
            let mut fresh = Vec::new();
            if let Ok(callees) = idx.callees(id).await {
                for c in callees {
                    edges.push(json!({ "from": id, "to": c.id, "kind": "calls" }));
                    if !nodes.contains_key(&c.id) {
                        fresh.push(c);
                    }
                }
            }
            if let Ok(callers) = idx.callers(id, 1).await {
                for c in callers {
                    edges.push(json!({ "from": c.id, "to": id, "kind": "calls" }));
                    if !nodes.contains_key(&c.id) {
                        fresh.push(c);
                    }
                }
            }
            for c in fresh {
                nodes.insert(c.id, c.clone());
                next.push(c.id);
            }
        }
        frontier = next;
        if frontier.is_empty() {
            break;
        }
        if nodes.len() >= limit {
            truncated = true;
            break;
        }
    }
    (nodes, edges, truncated)
}

/// Resolve seed từ query: ưu tiên hàm (Function → Method → Class) để view query
/// không rơi vào biến/param vô mối gọi (trước đây `search_symbol(q, None, 1)` lấy
/// match bừa). Với mỗi kind thử exact → prefix → contains; fallback mọi loại để
/// giữ hành vi substring cũ.
async fn resolve_seed(idx: &GraphIndex, q: &str) -> Option<Symbol> {
    for mode in [SymbolMatch::Exact, SymbolMatch::Prefix, SymbolMatch::Contains] {
        for kind in [
            Some(SymbolKind::Function),
            Some(SymbolKind::Method),
            Some(SymbolKind::Class),
            None,
        ] {
            if let Ok((v, _)) = idx.search_symbol_paged(q, kind, mode, 1, 0).await {
                if let Some(s) = v.into_iter().next() {
                    return Some(s);
                }
            }
        }
    }
    None
}

pub async fn boot(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        state.boot_json,
    )
}

fn api_error(e: codegraph_core::Error) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": e.to_string() })),
    )
        .into_response()
}
