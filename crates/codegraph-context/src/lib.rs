//! Context builder: search symbol → callers + callees → markdown/json.
//!
//! Chạy trên `SharedGraphIndex` (snapshot mới nhất qua `ensure_fresh`), không
//! còn `Db`/`Traversal` cũ — query surface mới của `GraphIndex`:
//! `search_symbol` → `callers`/`callees` (BFS trên chain engine).

use codegraph_core::{Result, Symbol, SymbolMatch};
use codegraph_graph::{Pagination, SharedGraphIndex};
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
    // Try symbol-name search first, then fallback to file-path search.
    let mut candidates = idx
        .search_symbol_paged_resumable(
            &req.query,
            None,
            SymbolMatch::Contains,
            Pagination {
                limit: req.limit as usize,
                offset: 0,
            },
            None,
            None,
        )
        .await?
        .page;
    if candidates.is_empty() {
        // Fallback: query as filename (strip extension for symbol-name search).
        let query_stripped = req.query.split('/').last()
            .and_then(|f| {
                let without_ext = f.rsplitn(2, '.').nth(1)?;
                if without_ext.is_empty() { None } else { Some(without_ext.to_string()) }
            })
            .unwrap_or_else(|| req.query.clone());
        candidates = idx
            .search_symbol_paged_resumable(
                &query_stripped,
                None,
                SymbolMatch::Contains,
                Pagination {
                    limit: req.limit as usize,
                    offset: 0,
                },
                None,
                None,
            )
            .await?
            .page;
    }

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
                let _ = writeln!(
                    out,
                    "- `{}` — `{}:{}`",
                    c.name,
                    rel_path(&c.file, strip),
                    c.line
                );
            }
        }
        if !h.callees.is_empty() {
            let _ = writeln!(out, "\n**Callees** ({}):", h.callees.len());
            for c in &h.callees {
                let _ = writeln!(
                    out,
                    "- `{}` — `{}:{}`",
                    c.name,
                    rel_path(&c.file, strip),
                    c.line
                );
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_graph::SharedGraphIndex;
    use std::sync::Arc;

    fn sym(name: &str, id: u64) -> codegraph_core::Symbol {
        codegraph_core::Symbol {
            id,
            name: name.to_string(),
            kind: codegraph_core::SymbolKind::Function,
            scope: codegraph_core::ScopeLevel::Global,
            scope_id: 0,
            type_ref: 0,
            type_name: None,
            file: "RestEndpoint.java".into(),
            line: 1,
            end_line: 2,
            signature: None,
            doc: None,
            annotations: Vec::new(),
            language: "java".into(),
        }
    }

    #[tokio::test]
    async fn context_fallback_matches_filename() {
        // Tạo index với symbol "RestEndpoint" trong file "RestEndpoint.java" dùng sqlite temp.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db_str = format!("sqlite://{}", db_path.to_string_lossy());

        {
            let mut idx = codegraph_graph::GraphIndex::open(&db_str).await.unwrap();
            let r = codegraph_graph::ParseResult {
                path: "RestEndpoint.java".into(),
                language: "java".into(),
                bytes: 0,
                lines: 0,
                symbols: vec![sym("RestEndpoint", 100)],
                chains: std::collections::HashMap::new(),
                calls: vec![],
            };
            idx.ingest(&[r]).await.unwrap();
        }

        let sgi = SharedGraphIndex::open(Some(db_str.clone())).await.unwrap();

        // Query "RestEndpoint.java" → không match theo tên symbol → fallback tìm "RestEndpoint".
        let req = ContextRequest {
            query: "RestEndpoint.java".into(),
            depth: 1,
            include_source: false,
            limit: 5,
            format: Format::Markdown,
            strip_prefix: None,
        };
        let sgi_arc: Arc<SharedGraphIndex> = Arc::new(sgi);
        let resp = build_response(&sgi_arc, &req).await.unwrap();
        assert!(!resp.hits.is_empty(), "phải match qua fallback filename");
        assert_eq!(resp.hits[0].symbol.name, "RestEndpoint");

        // Query "RestEndpoint" (không có extension) → match trực tiếp.
        let req2 = ContextRequest {
            query: "RestEndpoint".into(),
            ..req
        };
        let resp2 = build_response(&sgi_arc, &req2).await.unwrap();
        assert!(!resp2.hits.is_empty());
        assert_eq!(resp2.hits[0].symbol.name, "RestEndpoint");
    }
}
