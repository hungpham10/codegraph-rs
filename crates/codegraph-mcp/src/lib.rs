//! MCP server (stdio JSON-RPC 2.0). Hand-rolled, no SDK.

mod protocol;
mod tools;
mod usage;

pub use protocol::{ErrorObj, JsonRpcMessage, Response};
pub use tools::tool_definitions;

use codegraph_graph::SharedGraphIndex;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub const SERVER_INSTRUCTIONS: &str = include_str!("server-instructions.md");
pub const PROTOCOL_VERSION: &str = "2024-11-05";
pub const SERVER_NAME: &str = "codegraph";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct McpServer {
    /// Workspace root — dùng cho admin tools (`codegraph_init` / `codegraph_index`).
    root: camino::Utf8PathBuf,
    shared_index: Arc<SharedGraphIndex>,
    /// Telemetry cho `codegraph_query_usage_report`.
    usage: Arc<Mutex<usage::UsageStats>>,
}

impl McpServer {
    pub async fn new(
        root: camino::Utf8PathBuf,
        index_path: Option<std::path::PathBuf>,
    ) -> anyhow::Result<Self> {
        let shared_index = Arc::new(SharedGraphIndex::open(index_path).await?);
        Ok(Self {
            root,
            shared_index,
            usage: Arc::new(Mutex::new(usage::UsageStats::default())),
        })
    }

    pub async fn run_stdio(self) -> anyhow::Result<()> {
        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin);
        let mut stdout = tokio::io::stdout();
        let mut line = String::new();

        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let msg: JsonRpcMessage = match serde_json::from_str(trimmed) {
                Ok(m) => m,
                Err(e) => {
                    write_response(
                        &mut stdout,
                        Response::error(Value::Null, -32700, &format!("parse error: {e}")),
                    )
                    .await?;
                    continue;
                }
            };
            if msg.id.is_none() {
                // notification — no response
                continue;
            }
            let id = msg.id.clone().unwrap_or(Value::Null);
            let resp = self.dispatch(msg).await;
            let final_resp = match resp {
                Ok(v) => Response::ok(id, v),
                Err(e) => Response::error(id, -32603, &e.to_string()),
            };
            write_response(&mut stdout, final_resp).await?;
        }
        Ok(())
    }

    async fn dispatch(&self, msg: JsonRpcMessage) -> anyhow::Result<Value> {
        match msg.method.as_deref() {
            Some("initialize") => Ok(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
                "instructions": SERVER_INSTRUCTIONS,
            })),
            Some("ping") => Ok(json!({})),
            Some("tools/list") => Ok(json!({ "tools": tool_definitions() })),
            Some("tools/call") => {
                self.handle_tool_call(msg.params.unwrap_or(Value::Null))
                    .await
            }
            Some(m) => Err(anyhow::anyhow!("method not found: {m}")),
            None => Err(anyhow::anyhow!("missing method")),
        }
    }

    async fn handle_tool_call(&self, params: Value) -> anyhow::Result<Value> {
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(Value::Null);

        // Telemetry tool — đọc/ghi trực tiếp từ usage stats, không qua GraphApi.
        if name == "codegraph_query_usage_report" {
            let reset = args.get("reset").and_then(|v| v.as_bool()).unwrap_or(false);
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let mut u = self.usage.lock().unwrap();
            let report = u.report(limit);
            if reset {
                u.reset();
            }
            let text = serde_json::to_string_pretty(&report)?;
            return Ok(json!({
                "content": [{ "type": "text", "text": text }],
                "isError": false,
            }));
        }

        let api = codegraph_api::GraphApi::new_with_index(self.shared_index.clone());
        // Admin tools (init/index) cần workspace root; sandbox cần root (config +
        // mock dirs) + snapshot index — dispatch riêng, không qua GraphApi.
        let dispatch = if name == "codegraph_init" || name == "codegraph_index" {
            tools::dispatch_admin(&self.root, name, args.clone()).await
        } else if name == "codegraph_sandbox" {
            tools::dispatch_sandbox(&self.root, self.shared_index.clone(), args.clone()).await
        } else if name == "codegraph_diff" {
            tools::dispatch_diff(&self.root, self.shared_index.clone(), args.clone()).await
        } else if name == "codegraph_diff_simulate" {
            tools::dispatch_diff_simulate(&self.root, self.shared_index.clone(), args.clone()).await
        } else if name == "codegraph_origin_simulate" {
            tools::dispatch_origin_simulate(&self.root, self.shared_index.clone(), args.clone())
                .await
        } else {
            tools::dispatch_with_api(&api, name, args).await
        };
        let text = match dispatch {
            Ok(t) => t,
            Err(e) => {
                self.usage
                    .lock()
                    .unwrap()
                    .record(name, e.to_string().len() as u64, 0, true);
                return Err(anyhow::Error::from(e));
            }
        };
        // Ước lượng source bytes mà answer "thay thế" (file refs trong answer).
        let source_bytes = match serde_json::from_str::<Value>(&text) {
            Ok(v) => usage::estimate_source_bytes(&api, &v).await,
            Err(_) => 0,
        };
        self.usage
            .lock()
            .unwrap()
            .record(name, text.len() as u64, source_bytes, false);
        Ok(json!({
            "content": [{ "type": "text", "text": text }],
            "isError": false,
        }))
    }
}

async fn write_response<W: tokio::io::AsyncWrite + Unpin>(
    w: &mut W,
    r: Response,
) -> anyhow::Result<()> {
    let s = serde_json::to_string(&r)?;
    w.write_all(s.as_bytes()).await?;
    w.write_all(b"\n").await?;
    w.flush().await?;
    Ok(())
}
