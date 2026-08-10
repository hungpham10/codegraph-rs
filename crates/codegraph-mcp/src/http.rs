//! Transport HTTP cho MCP server — **luồng riêng, chưa implement** (stub).
//!
//! Với HTTP session KHÔNG đi theo process: mỗi kết nối được xác định bằng
//! `mcp-session-id` header và session store quản lý MỘT session PER KẾT NỐI
//! (cùng lúc nhiều phiên khác nhau, khác root, không chia sẻ gì ngoài process).
//!
//! Khi làm sẽ dùng rmcp feature `transport-streamable-http-server` (tower/
//! axum) + một `SessionStore` map `session_id -> Session`, và cần chỉnh
//! `codegraph serve --mcp --http` để mount server này thay vì stdio. Cấu trúc
//! module đã tách sẵn ở đây để không nhiễu vòng đời process-bound của stdio.

/// Entry điểm cho luồng HTTP (tương lai). Không bật mặc định — cần feature
/// `http` + `transport-streamable-http-server`; hiện tại chỉ báo chưa làm.
///
/// # Panics
/// Không có — trả `Err` rõ ràng để `codegraph serve --mcp --http` fail với
/// message giải thích thay vì chạy nhầm sang stdio.
#[cfg(feature = "http")]
pub async fn serve_http<S>(_service: S) -> anyhow::Result<()> {
    anyhow::bail!(
        "codegraph MCP http transport chưa được implement — đây là luồng riêng \
         (session theo mcp-session-id). Dùng `--mcp` (stdio) trước."
    )
}
