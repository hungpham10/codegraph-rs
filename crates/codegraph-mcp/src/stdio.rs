//! Transport stdio cho MCP server.
//!
//! Session đi theo process: một tiến trình = một kết nối = **một session slot**
//! cố định. Server không tự chọn đường dẫn — agent bind session bằng
//! `codegraph_init {"path": ...}` / nhả bằng `codegraph_deinit`.
//!
//! `serve_stdio` mount bất kỳ `ServerHandler` lên stdin/stdout qua
//! `rmcp::transport::io::stdio()` (transport-async-rw). Mọi JSON-RPC framing
//! đều do rmcp xử lý.

use rmcp::ServiceExt;

/// Serve `service` qua stdio tới khi kết nối kết thúc (client đóng stdin /
/// gửi shutdown). Lỗi transport (IO/handshake) trả về qua `anyhow`.
pub async fn serve_stdio<S>(service: S) -> anyhow::Result<()>
where
    S: rmcp::ServerHandler,
{
    service.serve(rmcp::transport::io::stdio())
        .await?
        .waiting()
        .await?;
    Ok(())
}
