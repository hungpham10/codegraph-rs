use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use codegraph_api::GraphApi;
use codegraph_core::Symbol;
use codegraph_graph::SharedGraphIndex;
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
    match api.search(&params.q, params.limit).await {
        Ok(hits) => Json(hits).into_response(),
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
        idx.search_symbol(q, None, 1)
            .await
            .ok()
            .and_then(|mut v| v.pop())
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

    let nodes: Vec<Symbol> = nodes.into_values().collect();
    Json(json!({
        "nodes": nodes,
        "edges": edges,
        "seed": seed,
        "truncated": truncated,
    }))
    .into_response()
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
