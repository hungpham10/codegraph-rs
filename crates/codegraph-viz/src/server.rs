use crate::api::{self, AppState};
use crate::assets::{content_type, Asset};
use crate::VizConfig;
use axum::{
    body::Body,
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use codegraph_graph::SharedGraphIndex;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::compression::CompressionLayer;

pub async fn serve(db_path: PathBuf, config: VizConfig) -> anyhow::Result<()> {
    let boot_json = serde_json::to_string(&config.boot)?;
    // Index sống trong chính file db (`.codegraph/db.sqlite`) — không sidecar.
    let shared_index = Arc::new(SharedGraphIndex::open(Some(db_path)).await?);
    let state = AppState {
        shared_index,
        boot_json,
    };

    let app = Router::new()
        .route("/api/status", get(api::status))
        .route("/api/search", get(api::search))
        .route("/api/symbol/{id}", get(api::symbol))
        .route("/api/flow/{id}", get(api::flow))
        .route("/api/search_flow", get(api::search_flow))
        .route("/api/subgraph", get(api::subgraph))
        .route("/api/files", get(api::files))
        .route("/api/callers/{id}", get(api::callers))
        .route("/api/callees/{id}", get(api::callees))
        .route("/api/boot", get(api::boot))
        .fallback(static_handler)
        .layer(CompressionLayer::new())
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], config.port));
    let url = format!("http://{addr}");
    tracing::info!("codegraph visualize at {url}");

    if config.open_browser {
        if let Err(e) = open::that(&url) {
            tracing::warn!("failed to open browser: {e}");
        }
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn static_handler(uri: Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match Asset::get(path) {
        Some(content) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, content_type(path))
            .body(Body::from(content.data.into_owned()))
            .unwrap(),
        None if !path.contains('.') => match Asset::get("index.html") {
            Some(content) => Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                .body(Body::from(content.data.into_owned()))
                .unwrap(),
            None => not_found(),
        },
        None => not_found(),
    }
}

fn not_found() -> Response<Body> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from("not found"))
        .unwrap()
}
