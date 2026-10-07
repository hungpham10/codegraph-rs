//! GraphQL HTTP API server cho codegraph — on-prem, không qua MCP.
//!
//! Expose toàn bộ năng lực đọc của `GraphApi` (query) + lifecycle session + 4
//! heavy tools (mutation) dưới dạng GraphQL có field-selection. Domain types
//! đến từ `codegraph_core` (đã derive GraphQL gated), nên không mirror.
//!
//! Privacy: response graph mặc định **không chứa raw source**; chỉ
//! `context(includeSource: true)` trả source (do UI/người dùng tự quyết định).

mod mutation;
mod query;
mod types;

use async_graphql::{EmptySubscription, Schema};
use async_graphql_axum::GraphQL;
use axum::{
    body::Body,
    http::{header::AUTHORIZATION, HeaderValue, Request, StatusCode},
    middleware::{from_fn, Next},
    response::IntoResponse,
    routing::get,
    Router,
};
use camino::Utf8PathBuf;
use codegraph_api::session::{OutputStyle, Session};
use codegraph_api::SearchSessionStore;
use codegraph_extract::SharedDocGraph;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

pub use types::*;

/// State chia sẻ giữa các resolver (lưu trong `Schema::data`).
pub struct AppState {
    /// Session quản lý vòng đời index của workspace root.
    pub session: Arc<Session>,
    /// Store resume id cho search phân trang (sống qua nhiều request).
    pub search_sessions: Arc<SearchSessionStore>,
    /// Bật output Mermaid cho các query diagram (`*_meraid`). Tắt → những
    /// resolver này trả lỗi rõ ràng. Đây là config mức server (`--mermaid`).
    pub mermaid: bool,
    /// Document graph cho structured document operations (HCL, YAML, JSON, TOML).
    /// Lazy theo root (persist qua `[docgraph]`); in-memory khi chưa bind root.
    pub doc_graph: Arc<SharedDocGraph>,
    /// Module UI đang bật (từ config) — frontend lọc sidebar theo danh sách.
    pub ui_modules: Vec<String>,
}

/// Cấu hình cho [`serve`].
pub struct ServeConfig {
    /// Địa chỉ bind (vd `127.0.0.1:8080`).
    pub addr: SocketAddr,
    /// API key bắt buộc (`Authorization: Bearer <key>` hoặc `?api_key=`).
    /// `None` → không giới hạn (chỉ dùng nội bộ / sau reverse-proxy).
    pub api_key: Option<String>,
    /// Pre-bind workspace root (`--path`). Có `.codegraph/` → load sẵn index.
    /// `None` → chờ `init` mutation từ UI.
    pub root: Option<Utf8PathBuf>,
    /// Output style seed từ CLI.
    pub format: OutputStyle,
    /// Origins CORS được phép. Rỗng → permissive (dev).
    pub allow_hosts: Vec<String>,
    /// Bật Mermaid diagram output (tương ứng flag `--mermaid` ở CLI).
    pub mermaid: bool,
    /// Phục vụ web UI nhúng tại `/` (tương ứng cờ tắt `--no-web`).
    pub web: bool,
    /// Module UI được bật (mặc định = tất cả). Tắt module để thu gọn sidebar.
    pub ui_modules: Vec<String>,
}

/// Module UI mặc định (khi `--ui-modules` không truyền).
pub const DEFAULT_UI_MODULES: [&str; 3] = ["explore", "review", "documents"];

/// Module UI đang bật (trả về cho frontend).
pub fn enabled_modules(cfg_modules: &[String]) -> Vec<String> {
    if cfg_modules.is_empty() {
        DEFAULT_UI_MODULES.iter().map(|s| s.to_string()).collect()
    } else {
        cfg_modules
            .iter()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// Chạy GraphQL server (blocking — bind + serve đến khi shutdown).
pub async fn serve(cfg: ServeConfig) -> anyhow::Result<()> {
    let session = match cfg.root {
        Some(ref r) => Session::with_root_and_format(r.clone(), cfg.format).await?,
        None => Session::new_with_format(cfg.format),
    };
    // Doc graph lazy theo root (nếu có) — persist qua `[docgraph]`/`[storage]`.
    let doc_graph = Arc::new(match cfg.root.clone() {
        Some(root) => SharedDocGraph::lazy(root),
        None => SharedDocGraph::in_memory(),
    });
    let state = Arc::new(AppState {
        session: Arc::new(session),
        search_sessions: Arc::new(SearchSessionStore::new()),
        mermaid: cfg.mermaid,
        doc_graph,
        ui_modules: crate::enabled_modules(&cfg.ui_modules),
    });

    // Warm index nền ngay lúc khởi động. `GraphIndex::rebuild` mất hàng chục
    // giây với repo lớn; để nó chạy ở task tách rời thay vì chặn request đầu
    // tiên của UI (request đó sẽ phải chờ `ensure_fresh`).
    {
        let warm = state.clone();
        tokio::spawn(async move {
            match warm.session.ensure_ready().await {
                Ok(sgi) => {
                    let _ = sgi.ensure_fresh().await;
                }
                Err(e) => {
                    tracing::debug!("index warm skipped: {e}");
                }
            }
        });
    }

    let app = build_app(&cfg, state);

    let listener = tokio::net::TcpListener::bind(cfg.addr).await?;
    tracing::info!("CodeGraph GraphQL listening on http://{}/graphql", cfg.addr);
    axum::serve(listener, app).await?;
    Ok(())
}

/// Build axum `Router` từ config + state (tách riêng để test không cần bind
/// port). Đây là nơi gắn CORS, api-key auth middleware và GraphQL handler.
pub(crate) fn build_app(cfg: &ServeConfig, state: Arc<AppState>) -> Router {
    let schema = Schema::build(query::Query, mutation::Mutation, EmptySubscription)
        .data(state.clone())
        .finish();

    // Clone các giá trị cần thiết vào owned data để middleware closure không
    // capture `&cfg` (phải là `'static`).
    let api_key = cfg.api_key.clone();

    let cors = if cfg.allow_hosts.is_empty() {
        CorsLayer::permissive()
    } else {
        let origins = cfg
            .allow_hosts
            .iter()
            .filter_map(|h| h.parse::<HeaderValue>().ok())
            .collect::<Vec<_>>();
        if origins.is_empty() {
            CorsLayer::permissive()
        } else {
            CorsLayer::new()
                .allow_origin(origins)
                .allow_methods(Any)
                .allow_headers(Any)
        }
    };

    let mut app = Router::new()
        .route("/health", get(health))
        .route_service("/graphql", GraphQL::new(schema))
        .with_state(state);

    // Web UI nhúng: mọi route không khớp (không phải /graphql, /health) trả
    // về SPA. Tắt bằng `--no-web` để chỉ còn API.
    if cfg.web {
        app = app.fallback_service(codegraph_web::web_router());
    }

    app.layer(cors)
        .layer(from_fn(move |req: Request<Body>, next: Next| {
            let api_key = api_key.clone();
            async move {
                if let Some(key) = api_key.as_ref() {
                    let header_ok = req
                        .headers()
                        .get(AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        .map(|v| v == format!("Bearer {key}") || v == key)
                        .unwrap_or(false);
                    let query_ok = req
                        .uri()
                        .query()
                        .map(|q| q.contains(&format!("api_key={key}")))
                        .unwrap_or(false);
                    if !header_ok && !query_ok {
                        return (StatusCode::UNAUTHORIZED, "missing or invalid api key")
                            .into_response();
                    }
                }
                next.run(req).await
            }
        }))
}

/// Health check — `200` khi index đã refresh xong, `503` khi đang rebuild.
///
/// Rebuild index mất hàng chục giây với repo lớn. Trong lúc đó server
/// chưa phục vục để query đước data — báo `503` để client/UI
/// retry thay vì treo chờ.
async fn health(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> impl IntoResponse {
    let sgi = match state.session.ensure_ready().await {
        Ok(sgi) => sgi,
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("no session bound: {e}"),
            )
        }
    };
    if sgi.fresh_snapshot().await.is_none() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "index chưa refresh xong (đang rebuild từ storage)".to_string(),
        );
    }
    (StatusCode::OK, "ok".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use codegraph_api::session::{OutputStyle, Session};
    use codegraph_api::SearchSessionStore;
    use codegraph_extract::SharedDocGraph;
    use tower::ServiceExt;

    fn make_state(mermaid: bool) -> Arc<AppState> {
        let session = Session::new_with_format(OutputStyle::Minimal);
        let doc_graph = Arc::new(SharedDocGraph::in_memory());
        Arc::new(AppState {
            session: Arc::new(session),
            search_sessions: Arc::new(SearchSessionStore::new()),
            mermaid,
            doc_graph,
            ui_modules: vec![],
        })
    }

    fn cfg(mermaid: bool) -> ServeConfig {
        ServeConfig {
            addr: "127.0.0.1:0".parse().unwrap(),
            api_key: None,
            root: None,
            format: OutputStyle::Minimal,
            allow_hosts: vec![],
            mermaid,
            // Test không phụ thuộc asset đã build — tắt web cho ổn định.
            web: false,
            ui_modules: vec![],
        }
    }

    async fn post_graphql(app: &Router, query: &str) -> (StatusCode, serde_json::Value) {
        let body = serde_json::json!({ "query": query }).to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/graphql")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let app = app.clone();
        let res = app.oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    /// Session chưa bind workspace → server chưa phục vục để query index → `503`.
    #[tokio::test]
    async fn health_503_without_session() {
        let app = build_app(&cfg(false), make_state(false));
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// Bind workspace + warm index xong — giửe `200`.
    #[tokio::test]
    async fn health_ok_after_warm() {
        use codegraph_api::session::DetailLevel;

        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let session = Session::new_with_format(OutputStyle::Minimal);
        session
            .init(root, false, DetailLevel::default(), None)
            .await
            .unwrap();
        // Đúng warm y nhết `serve()` lúc khởi động.
        let sgi = session.ensure_ready().await.unwrap();
        let _ = sgi.ensure_fresh().await;

        let state = Arc::new(AppState {
            session: Arc::new(session),
            search_sessions: Arc::new(SearchSessionStore::new()),
            mermaid: false,
            doc_graph: Arc::new(SharedDocGraph::in_memory()),
            ui_modules: vec![],
        });
        let app = build_app(&cfg(false), state);
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    /// Query cần index mà index đang rebuild → lỗi rõ ràng ngay, không treo request.
    #[tokio::test]
    async fn graphql_query_fails_fast_while_index_warming() {
        use codegraph_api::session::DetailLevel;

        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let session = Session::new_with_format(OutputStyle::Minimal);
        session
            .init(root, false, DetailLevel::default(), None)
            .await
            .unwrap();
        // KHÔNG warm — index chưa build, đúng trong trạng thái `serve()` lúc khởi động.
        let state = Arc::new(AppState {
            session: Arc::new(session),
            search_sessions: Arc::new(SearchSessionStore::new()),
            mermaid: false,
            doc_graph: Arc::new(SharedDocGraph::in_memory()),
            ui_modules: vec![],
        });
        let app = build_app(&cfg(false), state);
        let (_status, json) = post_graphql(
            &app,
            "{ graphcodeListSymbols(kind: FUNCTION, limit: 1) { total } }",
        )
        .await;
        let msg = json["errors"][0]["message"].as_str().unwrap();
        assert!(msg.contains("refresh"), "unexpected error: {msg}");
    }

    #[tokio::test]
    async fn graphql_endpoint_responds() {
        let app = build_app(&cfg(false), make_state(false));
        let (status, json) = post_graphql(&app, "{ __typename }").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["data"]["__typename"], "Query");
    }

    #[tokio::test]
    async fn mermaid_gate_enforced() {
        // Không bật --mermaid: mermaid phải báo lỗi gate (không gọi index).
        let app = build_app(&cfg(false), make_state(false));
        let (status, json) = post_graphql(&app, r#"{ mermaid(id: "1", kind: FLOW) }"#).await;
        assert_eq!(status, StatusCode::OK);
        let msg = json["errors"][0]["message"].as_str().unwrap();
        assert!(msg.contains("Mermaid"), "expected gate error, got: {msg}");

        // Bật --mermaid: vượt gate, sau đó lỗi do chưa có index (khác gate).
        let app2 = build_app(&cfg(true), make_state(true));
        let (_status, json2) = post_graphql(&app2, r#"{ mermaid(id: "1", kind: FLOW) }"#).await;
        let msg2 = json2["errors"][0]["message"].as_str().unwrap();
        assert!(
            !msg2.contains("Mermaid"),
            "gate should be off when --mermaid set, got: {msg2}"
        );
    }

    #[tokio::test]
    async fn api_key_required_when_set() {
        let mut c = cfg(false);
        c.api_key = Some("secret".to_string());
        let app = build_app(&c, make_state(false));

        // Thiếu key → 401.
        let req = Request::builder()
            .method("POST")
            .uri("/graphql")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({ "query": "{ __typename }" }).to_string(),
            ))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        // Có key (Bearer) → 200.
        let req = Request::builder()
            .method("POST")
            .uri("/graphql")
            .header("content-type", "application/json")
            .header("authorization", "Bearer secret")
            .body(Body::from(
                serde_json::json!({ "query": "{ __typename }" }).to_string(),
            ))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
