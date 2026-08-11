//! Context builder: search symbol → callers + callees → markdown/json.
//!
//! Chạy trên `SharedGraphIndex` (snapshot mới nhất qua `ensure_fresh`), không
//! còn `Db`/`Traversal` cũ — query surface mới của `GraphIndex`:
//! `search_symbol` → `callers`/`callees` (BFS trên chain engine).

use codegraph_core::{Result, Symbol};
use codegraph_graph::SharedGraphIndex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    #[default]
    Markdown,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextRequest {
    pub query: String,
    pub depth: u32,
    pub include_source: bool,
    pub limit: u32,
    pub format: Format,
    /// Workspace root — strip khỏi `file` trong markdown (path tương đối tiết
    /// kiệm token cho LLM). `None` = giữ absolute (CLI/HTTP không biết root).
    #[serde(default)]
    pub strip_prefix: Option<String>,
}

impl Default for ContextRequest {
    fn default() -> Self {
        Self {
            query: String::new(),
            depth: 1,
            include_source: false,
            limit: 5,
            format: Format::Markdown,
            strip_prefix: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextHit {
    pub symbol: Symbol,
    pub callers: Vec<Symbol>,
    pub callees: Vec<Symbol>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextResponse {
    pub query: String,
    pub hits: Vec<ContextHit>,
}

/// Build context markdown/json trên shared index (snapshot fresh).
pub async fn build(index: &Arc<SharedGraphIndex>, req: &ContextRequest) -> Result<String> {
    let response = build_response(index, req).await?;
    match req.format {
        Format::Json => Ok(serde_json::to_string_pretty(&response).unwrap_or_default()),
        Format::Markdown => Ok(render_markdown(&response, req.strip_prefix.as_deref())),
    }
}

pub async fn build_response(
    index: &Arc<SharedGraphIndex>,
    req: &ContextRequest,
) -> Result<ContextResponse> {
    let idx = index.ensure_fresh().await;
    let candidates = idx
        .search_symbol(&req.query, None, req.limit as usize)
        .await?;

    // Pre-load mỗi file một lần khi cần source.
    let file_cache: HashMap<String, Vec<String>> = if req.include_source {
        let mut cache = HashMap::new();
        for s in &candidates {
            if let std::collections::hash_map::Entry::Vacant(e) = cache.entry(s.file.clone()) {
                if let Ok(text) = std::fs::read_to_string(&s.file) {
                    e.insert(text.lines().map(str::to_owned).collect());
                }
            }
        }
        cache
    } else {
        HashMap::new()
    };

    let mut hits = Vec::new();
    for s in candidates {
        let callers = idx.callers(s.id, req.depth as usize).await?;
        let callees = idx.callees(s.id).await?;
        let source = if req.include_source {
            file_cache.get(&s.file).map(|lines| {
                let start = s.line.saturating_sub(1) as usize;
                let end = (s.end_line as usize).min(lines.len());
                lines[start..end].join("\n")
            })
        } else {
            None
        };
        hits.push(ContextHit {
            symbol: s,
            callers,
            callees,
            source,
        });
    }
    Ok(ContextResponse {
        query: req.query.clone(),
        hits,
    })
}

/// Strip `root/` prefix khỏi path (boundary-aware) — `None` giữ nguyên.
fn rel_path<'a>(p: &'a str, strip: Option<&str>) -> &'a str {
    if let Some(root) = strip {
        if let Some(rest) = p.strip_prefix(root) {
            if let Some(rest) = rest.strip_prefix('/') {
                return rest;
            }
        }
    }
    p
}

fn render_markdown(resp: &ContextResponse, strip: Option<&str>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Context: `{}`", resp.query);
    if resp.hits.is_empty() {
        let _ = writeln!(out, "\n_No matches._");
        return out;
    }
    for h in &resp.hits {
        let _ = writeln!(
            out,
            "\n## `{}` — {} — `{}:{}`",
            h.symbol.name,
            h.symbol.kind.as_str(),
            rel_path(&h.symbol.file, strip),
            h.symbol.line
        );
        if let Some(sig) = &h.symbol.signature {
            let _ = writeln!(out, "\n```{}\n{}\n```", h.symbol.language, sig);
        }
        if let Some(src) = &h.source {
            let _ = writeln!(out, "\n```{}\n{}\n```", h.symbol.language, src);
        }
        if !h.callers.is_empty() {
            let _ = writeln!(out, "\n**Callers** ({}):", h.callers.len());
            for c in &h.callers {
                let _ = writeln!(out, "- `{}` — `{}:{}`", c.name, rel_path(&c.file, strip), c.line);
            }
        }
        if !h.callees.is_empty() {
            let _ = writeln!(out, "\n**Callees** ({}):", h.callees.len());
            for c in &h.callees {
                let _ = writeln!(out, "- `{}` — `{}:{}`", c.name, rel_path(&c.file, strip), c.line);
            }
        }
    }
    out
}
