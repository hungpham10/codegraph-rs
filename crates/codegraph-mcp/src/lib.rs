//! MCP server on the `rmcp` SDK.
//!
//! Server start lên rồi **quản lý theo session**: với transport stdio mỗi tiến
//! trình host đúng **1 session slot** — agent gọi `codegraph_init {"path": ...}`
//! để bind session vào workspace root, `codegraph_deinit` để nhả. Mọi tool khác
//! chạy qua session (chưa bind/init → refuse). `--path` lúc khởi động là
//! pre-seed, không bắt buộc.
//!
//! Hai transport module: [`stdio`] (luồng chính, 1 process = 1 session cố định)
//! và [`http`] (luồng riêng — stub, sẽ quản lý session theo session-id header).

pub mod http;
mod session;
pub mod stdio;
mod tools;
mod usage;

pub use session::{InitOutcome, Session};
pub use stdio::serve_stdio;

use std::future::Future;
use std::sync::{Arc, Mutex};

use codegraph_api::GraphApi;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo,
};
use rmcp::service::{MaybeSendFuture, RequestContext};
use rmcp::{ErrorData as McpError, RoleServer};
use serde_json::{json, Value};

/// Hướng dẫn sử dụng tools — client render trong instructions sau `initialize`.
pub const SERVER_INSTRUCTIONS: &str = include_str!("server-instructions.md");
pub const SERVER_NAME: &str = "codegraph";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Server MCP. Transport-agnostic: stdio (1 process = 1 session) mount trực
/// tiếp, http (tương lai) sẽ xoay vòng session store riêng.
pub struct CodegraphServer {
    session: Session,
    usage: Arc<Mutex<usage::UsageStats>>,
}

impl CodegraphServer {
    /// Server với session trống — `codegraph_init` sẽ bind root trong phiên.
    pub fn new() -> Self {
        Self {
            session: Session::new(),
            usage: Arc::new(Mutex::new(usage::UsageStats::default())),
        }
    }

    /// Pre-seed root từ `--path` lúc khởi động (tương đương đã `codegraph_init`
    /// với root đó, không index thêm). Giữ CLI/watcher flow không vỡ.
    pub async fn with_root(root: camino::Utf8PathBuf) -> anyhow::Result<Self> {
        Ok(Self {
            session: Session::with_root(root).await?,
            usage: Arc::new(Mutex::new(usage::UsageStats::default())),
        })
    }

    /// Dispatch một tool call đã verify tên. Trả [`ToolOutput::Text`] cho thành
    /// công, [`ToolOutput::Error`] cho lỗi tool (client thấy `is_error`),
    /// [`Err`] cho lỗi protocol (unknown tool đã bị chặn trước ở `call_tool`).
    async fn run_tool(&self, name: &str, args: Value) -> Result<ToolOutput, McpError> {
        // ── Telemetry — không cần session ──
        if name == "codegraph_query_usage_report" {
            let reset = args.get("reset").and_then(|v| v.as_bool()).unwrap_or(false);
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let mut u = self.usage.lock().unwrap();
            let report = u.report(limit);
            if reset {
                u.reset();
            }
            drop(u);
            let text = serde_json::to_string_pretty(&report).map_err(|e| {
                McpError::internal_error(
                    "usage report failed",
                    Some(json!({"reason": e.to_string()})),
                )
            })?;
            return Ok(ToolOutput::Text {
                text,
                source_bytes: 0,
            });
        }

        // ── Admin / session lifecycle ──
        match name {
            "codegraph_init" => {
                let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
                    return Ok(ToolOutput::Error(
                        "codegraph_init requires `path` — the workspace root to bind this session to, \
                         e.g. {\"path\": \"/abs/path/to/project\"}. \
                         To index immediately pass {\"path\": ..., \"index\": true}, \
                         otherwise call codegraph_index {} afterwards."
                            .into(),
                    ));
                };
                // Default = KHÔNG index — bind nhanh, không block user. Agent muốn
                // data thì chủ động gọi codegraph_index {} (hoặc truyền index=true).
                let do_index = args.get("index").and_then(|v| v.as_bool()).unwrap_or(false);
                return match self
                    .session
                    .init(camino::Utf8PathBuf::from(path), do_index)
                    .await
                {
                    Ok(out) => {
                        let mut v =
                            json!({ "root": out.root.as_str(), "initialized": out.dir.as_str() });
                        if let Some(stats) = &out.indexed {
                            v["indexed"] = session::stats_json(stats);
                        }
                        Ok(ToolOutput::json(&v))
                    }
                    Err(e) => Ok(ToolOutput::Error(e.to_string())),
                };
            }
            "codegraph_deinit" => {
                return match self.session.deinit().await {
                    Ok(prev) => {
                        let v = json!({
                            "deinitialized": true,
                            "root": prev.map(|p| Value::String(p.into_string())).unwrap_or(Value::Null),
                        });
                        Ok(ToolOutput::json(&v))
                    }
                    Err(e) => Ok(ToolOutput::Error(e.to_string())),
                };
            }
            "codegraph_index" => {
                return match self.session.reindex().await {
                    Ok(stats) => {
                        let v = session::stats_json(&stats);
                        Ok(ToolOutput::json(&v))
                    }
                    Err(e) => Ok(ToolOutput::Error(e.to_string())),
                };
            }
            _ => {}
        }

        // ── Query tools — cần session ready ──
        let sgi = match self.session.ensure_ready().await {
            Ok(sgi) => sgi,
            Err(e) => return Ok(ToolOutput::Error(e.to_string())),
        };
        let api = GraphApi::new_with_index(sgi.clone());
        // ensure_ready chỉ Ok khi session có root — đây chỉ là phòng hờ.
        let Some(root) = self.session.root().await else {
            return Ok(ToolOutput::Error("session root unavailable".into()));
        };

        let dispatch = match name {
            "codegraph_sandbox" => tools::dispatch_sandbox(&root, sgi.clone(), args.clone()).await,
            "codegraph_diff" => tools::dispatch_diff(&root, sgi.clone(), args.clone()).await,
            "codegraph_diff_simulate" => {
                tools::dispatch_diff_simulate(&root, sgi.clone(), args.clone()).await
            }
            "codegraph_origin_simulate" => {
                tools::dispatch_origin_simulate(&root, sgi.clone(), args.clone()).await
            }
            _ => tools::dispatch_with_api(&api, name, args).await,
        };

        match dispatch {
            Ok(text) => {
                // Ước lượng source bytes mà answer "thay thế" (file refs trong answer).
                let source_bytes = match serde_json::from_str::<Value>(&text) {
                    Ok(v) => usage::estimate_source_bytes(&api, &v).await,
                    Err(_) => 0,
                };
                Ok(ToolOutput::Text { text, source_bytes })
            }
            Err(e) => Ok(ToolOutput::Error(e.to_string())),
        }
    }
}

impl Default for CodegraphServer {
    fn default() -> Self {
        Self::new()
    }
}

/// Kết quả `run_tool` — phân biệt thành công / lỗi tool (client-visible) /
/// lỗi protocol (không dùng ở đây, `call_tool` trả Err trực tiếp).
enum ToolOutput {
    Text { text: String, source_bytes: u64 },
    Error(String),
}

impl ToolOutput {
    fn json(v: &Value) -> Self {
        match serde_json::to_string_pretty(v) {
            Ok(text) => ToolOutput::Text {
                text,
                source_bytes: 0,
            },
            Err(e) => ToolOutput::Error(format!("serialize response: {e}")),
        }
    }
}

impl ServerHandler for CodegraphServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER_NAME, SERVER_VERSION))
            .with_instructions(SERVER_INSTRUCTIONS.to_string())
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + MaybeSendFuture + '_ {
        async move {
            Ok(ListToolsResult {
                tools: tools::rmcp_tools(),
                ..Default::default()
            })
        }
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, McpError>> + MaybeSendFuture + '_ {
        async move {
            let name = request.name.as_ref();
            let args = request.arguments.map(Value::Object).unwrap_or(Value::Null);

            // Tên tool không tồn tại → protocol error (client thấy lỗi JSON-RPC
            // method-not-found, không thấy một "tool ảo"). Chặn sớm trước khi
            // chạy vào run_tool để không cho nhầm tool lạ chạy nhánh `_`.
            if !tools::is_known_tool(name) {
                return Err(McpError::method_not_found::<
                    rmcp::model::CallToolRequestMethod,
                >());
            }

            match self.run_tool(name, args).await {
                Ok(ToolOutput::Text { text, source_bytes }) => {
                    self.usage
                        .lock()
                        .unwrap()
                        .record(name, text.len() as u64, source_bytes, false);
                    Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
                }
                Ok(ToolOutput::Error(msg)) => {
                    self.usage
                        .lock()
                        .unwrap()
                        .record(name, msg.len() as u64, 0, true);
                    Ok(CallToolResult::error(vec![ContentBlock::text(msg)]).into())
                }
                Err(e) => Err(e),
            }
        }
    }
}
