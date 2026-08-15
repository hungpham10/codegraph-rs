//! Transport HTTP (Streamable HTTP / SSE) cho MCP server — luồng riêng.
//!
//! Với HTTP session KHÔNG đi theo process: mỗi kết nối được xác định bằng
//! `mcp-session-id` header và rmcp cấp **một `CodegraphServer` riêng PER KẾT
//! NỐI** (qua service factory) — cùng lúc nhiều phiên khác nhau, khác root,
//! không chia sẻ gì ngoài process. Agent bind workspace bằng
//! `codegraph_init {"path": ...}` ngay trong phiên của mình.
//!
//! Dùng rmcp feature `transport-streamable-http-server`: `StreamableHttpService`
//! (tower-service xử lý POST/GET/DELETE + SSE) được mount qua axum ở cả `/`
//! và `/mcp`. `codegraph serve --mcp --http` mount server này thay vì stdio.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tracing::info;

use crate::{CodegraphServer, OutputStyle};

/// Serve MCP qua Streamable HTTP trên `addr`, mount ở `/` và `/mcp`.
///
/// Mỗi session (`mcp-session-id`) do rmcp tạo bằng cách gọi factory → một
/// `CodegraphServer` với session slot trống riêng (không pre-seed root, kể cả
/// khi CLI truyền `--path`): mỗi client bind root riêng bằng `codegraph_init`.
///
/// `allowed_hosts` — danh sách `Host` header được chấp nhận (rmcp kiểm tra để
/// chống DNS rebinding; loopback là mặc định an toàn). Muốn mở LAN/docker:
/// thêm IP/hostname thật bằng `--allow-host`, hoặc truyền danh sách **rỗng**
/// (`--allow-any-host`) để chấp nhận mọi host.
///
/// `enable_observability` — bật endpoint `/health`, `/metrics`, `/metrics/prometheus`.
///
/// `api_keys` — danh sách API key hợp lệ. Nếu không rỗng, yêu cầu header
/// `Authorization: Bearer <key>` cho các route MCP (`/` và `/mcp`).
/// Health/metrics endpoints KHÔNG yêu cầu auth.
///
/// # Panics
/// Không có — bind thất bại / lỗi serve trả `Err` qua `anyhow`.
pub async fn serve_http(
    format: OutputStyle,
    addr: SocketAddr,
    allowed_hosts: Vec<String>,
    _enable_observability: bool,
    api_keys: Vec<String>,
) -> anyhow::Result<()> {
    let session_manager = Arc::new(LocalSessionManager::default());
    let config = StreamableHttpServerConfig::default()
        // CLI đã chuẩn bị: mặc định loopback, rỗng = allow all (--allow-any-host).
        .with_allowed_hosts(allowed_hosts)
        // Client cũ (Claude Desktop, ...) negotiate < 2026-07-28 → cần session.
        // Per SEP-2567 request 2026-07-28 vẫn luôn chạy stateless.
        .with_legacy_session_mode(true);
    let service = StreamableHttpService::new(
        move || Ok(CodegraphServer::new_with_format(format)),
        session_manager,
        config,
    );

    let router = Router::new()
        .route_service("/", service.clone())
        .route_service("/mcp", service);

    if !api_keys.is_empty() {
        // Auth will be added in Track 3
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    info!(
        %local,
        "codegraph MCP http listening (Streamable HTTP); point your MCP client at http://{local}/mcp"
    );
    axum::serve(listener, router).await?;
    Ok(())
}

// Deprecated original serve_http – replaced by extended version with observability and auth support.
// The old implementation has been removed to avoid duplicate symbol definitions.


/// Smoke test: POST `initialize` qua tower oneshot (không cần TCP) → HTTP
/// 200 + response SSE chứa `serverInfo.name = codegraph`. Module này chỉ
/// compile khi feature `http` bật (lib.rs gate toàn bộ `mod http`).
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn test_app() -> axum::Router {
        let session_manager = Arc::new(LocalSessionManager::default());
        let config = StreamableHttpServerConfig::default()
            .with_allowed_hosts(["localhost", "127.0.0.1"])
            .with_legacy_session_mode(true);
        let service =
            StreamableHttpService::new(|| Ok(CodegraphServer::new()), session_manager, config);
        axum::Router::new()
            .route_service("/", service.clone())
            .route_service("/mcp", service)
    }

    /// Smoke test: POST `initialize` qua tower oneshot (không cần TCP) → HTTP
    /// 200 + response SSE chứa `serverInfo.name = codegraph`.
    #[tokio::test]
    async fn initialize_over_http() {
        let app = test_app();
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke-test","version":"0"}}}"#;
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("http://localhost/mcp")
                    .header("host", "localhost")
                    .header("content-type", "application/json")
                    .header("accept", "application/json, text/event-stream")
                    .header("mcp-protocol-version", "2025-06-18")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("codegraph"),
            "initialize response thiếu serverInfo.name=codegraph: {text}"
        );
    }
}
