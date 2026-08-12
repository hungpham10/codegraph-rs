//! Telemetry cho MCP tool usage (tương ứng `query_usage_report` của Walle).
//!
//! Ghi mỗi tool call: tool name, answer_bytes (JSON trả về cho LLM) và
//! source_bytes (ước lượng tổng bytes của các file mà answer "thay thế" — lấy
//! từ `file` fields trong answer, map sang `FileInfo.bytes`). `savings_pct` đo
//! mức độ tránh đọc source khi dùng query thay vì đọc file.

use serde_json::{json, Value};
use std::collections::HashMap;

/// Thống kê một tool.
#[derive(Default)]
pub struct ToolStat {
    pub calls: u64,
    pub errors: u64,
    pub answer_bytes: u64,
    pub source_bytes: u64,
}

/// Bộ đếm usage toàn server (thread-safe qua Mutex ngoài).
#[derive(Default)]
pub struct UsageStats {
    pub calls: u64,
    pub errors: u64,
    pub answer_bytes: u64,
    pub source_bytes: u64,
    pub per_tool: HashMap<String, ToolStat>,
}

impl UsageStats {
    /// Ghi một tool call (is_error = lỗi dispatch → không tính answer bytes).
    pub fn record(&mut self, tool: &str, answer_bytes: u64, source_bytes: u64, is_error: bool) {
        self.calls += 1;
        self.answer_bytes += answer_bytes;
        self.source_bytes += source_bytes;
        if is_error {
            self.errors += 1;
        }
        let t = self.per_tool.entry(tool.to_string()).or_default();
        t.calls += 1;
        t.answer_bytes += answer_bytes;
        t.source_bytes += source_bytes;
        if is_error {
            t.errors += 1;
        }
    }

    /// Reset toàn bộ thống kê (dùng khi `reset=true`).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Báo cáo tổng hợp + per-tool (sort theo calls giảm dần).
    pub fn report(&self, limit: usize) -> Value {
        let mut per_tool: Vec<Value> = self
            .per_tool
            .iter()
            .map(|(name, s)| {
                json!({
                    "tool": name,
                    "calls": s.calls,
                    "errors": s.errors,
                    "answer_bytes": s.answer_bytes,
                    "source_bytes": s.source_bytes,
                })
            })
            .collect();
        per_tool.sort_by(|a, b| {
            b["calls"]
                .as_u64()
                .cmp(&a["calls"].as_u64())
                .then(b["answer_bytes"].as_u64().cmp(&a["answer_bytes"].as_u64()))
        });
        if limit > 0 && per_tool.len() > limit {
            per_tool.truncate(limit);
        }
        let savings_pct = if self.answer_bytes + self.source_bytes > 0 {
            (self.source_bytes as f64 / (self.answer_bytes + self.source_bytes) as f64) * 100.0
        } else {
            0.0
        };
        json!({
            "total_calls": self.calls,
            "total_errors": self.errors,
            "answer_bytes": self.answer_bytes,
            "source_bytes": self.source_bytes,
            "savings_pct": (savings_pct * 10.0).round() / 10.0,
            "per_tool": per_tool,
        })
    }
}

/// Ước lượng source bytes mà một answer JSON "thay thế": gom mọi giá trị của
/// key `file` (path của symbol trả về), map sang `FileInfo.bytes` trong index.
/// Duyệt toàn bộ cây JSON — an toàn với mọi shape của answer. `root` là
/// workspace root: answer relativize `file` theo root (xem `tools::emit_value`),
/// nên lookup key cũng strip root để khớp.
pub async fn estimate_source_bytes(
    api: &codegraph_api::GraphApi,
    answer_json: &Value,
    root: &str,
) -> u64 {
    let mut paths = Vec::new();
    collect_file_paths(answer_json, &mut paths);
    if paths.is_empty() {
        return 0;
    }
    // FileInfo.bytes của từng file (lazy — chỉ build khi cần). Key theo path
    // tương đối với root — cùng dạng với `file` trong answer đã relativize.
    let files = api.files("").await;
    let bytes_by_path: std::collections::HashMap<&str, u64> = files
        .iter()
        .map(|f| {
            (
                crate::tools::strip_root_prefix(f.path.as_str(), root),
                f.bytes,
            )
        })
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut total = 0u64;
    for p in paths {
        if let Some(b) = bytes_by_path.get(crate::tools::strip_root_prefix(&p, root)) {
            if seen.insert(p) {
                total += b;
            }
        }
    }
    total
}

fn collect_file_paths(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            if let Some(p) = map.get("file").and_then(|f| f.as_str()) {
                out.push(p.to_string());
            }
            for (_, val) in map {
                collect_file_paths(val, out);
            }
        }
        Value::Array(arr) => {
            for val in arr {
                collect_file_paths(val, out);
            }
        }
        _ => {}
    }
}
