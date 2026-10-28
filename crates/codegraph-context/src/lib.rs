//! Context builder: search symbol → callers + callees → markdown/json.
//!
//! Chạy trên `SharedGraphIndex` (snapshot mới nhất qua `ensure_fresh`), không
//! còn `Db`/`Traversal` cũ — query surface mới của `GraphIndex`:
//! `search_symbol` → `callers`/`callees` (BFS trên chain engine).
//!
//! Tuỳ chọn `rerank = "jev"`: trước khi lấy callers/callees, gom candidate
//! (token hoá query + `Contains` từng token) rồi để Jev chọn symbol gần nhất
//! kèm xác suất. Mặc định TẮT và **fail-open**: Jev lỗi → xếp hạng cũ.

mod jev;

pub use jev::{Candidate, JevJudge, Judge, Verdict};

use codegraph_core::{Error, Result, Symbol, SymbolKind, SymbolMatch};
use codegraph_graph::{Pagination, SharedGraphIndex};
use codegraph_source::{entry_from_symbol_file, read_entry, Source};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    #[default]
    Markdown,
    Json,
}

/// Cách chọn symbol gốc trước khi mở rộng callers/callees.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rerank {
    /// `Contains` trên tên symbol (hành vi cũ, không gọi mạng).
    #[default]
    None,
    /// Jev chọn trong pool candidate — cần `[jev] enabled = true` + env token.
    Jev,
}

impl Rerank {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" | "" => Some(Self::None),
            "jev" => Some(Self::Jev),
            _ => None,
        }
    }
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
    /// `Jev` = rerank bằng Jev (opt-in qua `[jev]` trong config).
    #[serde(default)]
    pub rerank: Rerank,
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
            rerank: Rerank::None,
        }
    }
}

/// Xác suất Jev gán cho symbol — `Some` chỉ khi `rerank = "jev"` chạy được.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MatchScore {
    pub probability: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

/// Trạng thái rerank của lần gọi này — để client biết vì sao kết quả trông cũ.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankInfo {
    /// Jev có thực sự quyết định thứ tự không.
    pub used: bool,
    /// Lý do khi `used = false` (tắt, thiếu token, timeout...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextHit {
    pub symbol: Symbol,
    pub callers: Vec<Symbol>,
    pub callees: Vec<Symbol>,
    pub source: Option<String>,
    /// Điểm khớp do Jev gán (chỉ khi rerank chạy được).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_score: Option<MatchScore>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextResponse {
    pub query: String,
    pub hits: Vec<ContextHit>,
    /// Không symbol nào đủ liên quan — `hits` rỗng vì lý do ngữ nghĩa, không
    /// phải vì index rỗng.
    #[serde(default)]
    pub no_match: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank: Option<RerankInfo>,
}

/// Build context markdown/json trên shared index (snapshot fresh).
pub async fn build(
    index: &Arc<SharedGraphIndex>,
    req: &ContextRequest,
    source: Option<&dyn Source>,
) -> Result<String> {
    let response = build_response(index, req, source).await?;
    match req.format {
        Format::Json => Ok(serde_json::to_string_pretty(&response).unwrap_or_default()),
        Format::Markdown => Ok(render_markdown(&response, req.strip_prefix.as_deref())),
    }
}

pub async fn build_response(
    index: &Arc<SharedGraphIndex>,
    req: &ContextRequest,
    source: Option<&dyn Source>,
) -> Result<ContextResponse> {
    build_response_with_judge(index, req, source, None).await
}

/// Như [`build_response`] nhưng nhận `judge` ngoài — test truyền fake, production
/// truyền `None` để tự dựng [`JevJudge`] từ config global.
pub async fn build_response_with_judge(
    index: &Arc<SharedGraphIndex>,
    req: &ContextRequest,
    source: Option<&dyn Source>,
    judge: Option<&dyn Judge>,
) -> Result<ContextResponse> {
    let idx = index.ensure_fresh().await;
    let (mut symbols, scores, no_match, rerank_info) = order_candidates(&idx, req, judge).await?;

    // Pre-load mỗi file một lần khi cần source. Đọc qua `Source` trait để
    // đường query không còn đọc `std::fs` trực tiếp — cùng abstraction với
    // đường ingest, nên đổi provider (disk → git/remote) là một chỗ.
    let file_cache: HashMap<String, Vec<String>> = if req.include_source {
        let mut cache = HashMap::new();
        for s in &symbols {
            // Đọc **trước**, xong mới insert — `cache.entry(..)` giữ borrow `&mut`
            // không qua được `.await`.
            if cache.contains_key(&s.file) {
                continue;
            }
            match load_source_lines(source, &s.file).await {
                Ok(lines) => {
                    cache.insert(s.file.clone(), lines);
                }
                Err(err) => {
                    // Trước đây lỗi ở đây bị `if let Ok` **nuốt im lặng**: file
                    // không đọc được → `source` trả None mà không ai biết vì
                    // sao. Giờ log để debug được.
                    tracing::warn!(
                        file = %s.file,
                        error = %err,
                        "include_source: bỏ qua file không đọc được"
                    );
                }
            }
        }
        cache
    } else {
        HashMap::new()
    };

    let mut hits = Vec::with_capacity(symbols.len());
    for s in symbols.drain(..) {
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
        let match_score = scores.get(&s.id).copied();
        hits.push(ContextHit {
            symbol: s,
            callers,
            callees,
            source,
            match_score,
        });
    }
    Ok(ContextResponse {
        query: req.query.clone(),
        hits,
        no_match,
        rerank: rerank_info,
    })
}

/// Chọn danh sách symbol gốc (đã cắt `limit`) + điểm khớp + trạng thái no-match.
type Ordered = (
    Vec<Symbol>,
    HashMap<u64, MatchScore>,
    bool,
    Option<RerankInfo>,
);

async fn order_candidates(
    idx: &codegraph_graph::GraphIndex,
    req: &ContextRequest,
    judge: Option<&dyn Judge>,
) -> Result<Ordered> {
    if req.rerank != Rerank::Jev {
        return Ok((
            legacy_candidates(idx, req).await?,
            HashMap::new(),
            false,
            None,
        ));
    }

    let cfg = codegraph_core::jev_config();
    let judge_owned;
    let judge: Option<&dyn Judge> = match judge {
        Some(j) => Some(j),
        None if cfg.is_ready() => {
            judge_owned = JevJudge::new(cfg.clone());
            Some(&judge_owned)
        }
        None => None,
    };
    let Some(judge) = judge else {
        // Tắt hoặc thiếu token: vẫn dùng pool (rộng hơn Contains 1 query) nhưng
        // không gọi mạng — hành vi gần nhất với "không có Jev".
        let pool = build_pool(idx, req).await?;
        let symbols: Vec<Symbol> = pool.into_iter().take(req.limit as usize).collect();
        return Ok((
            symbols,
            HashMap::new(),
            false,
            Some(RerankInfo {
                used: false,
                reason: Some(format!(
                    "jev không sẵn sàng (enabled={}, env `{}` {})",
                    cfg.enabled,
                    cfg.api_key_env,
                    if cfg.api_key().is_some() {
                        "có"
                    } else {
                        "thiếu"
                    }
                )),
            }),
        ));
    };

    let pool = build_pool(idx, req).await?;
    if pool.len() < MIN_POOL {
        return Ok((
            Vec::new(),
            HashMap::new(),
            true,
            Some(RerankInfo {
                used: false,
                reason: Some(format!(
                    "chỉ có {} candidate (< {MIN_POOL}) — không gọi Jev",
                    pool.len()
                )),
            }),
        ));
    }

    let repo = cfg.repo.as_deref().or_else(|| {
        req.strip_prefix
            .as_deref()
            .and_then(|p| p.trim_end_matches('/').rsplit('/').next())
            .filter(|s| !s.is_empty())
    });
    let candidates: Vec<Candidate> = pool
        .iter()
        .map(|s| Candidate::from_symbol(s, req.strip_prefix.as_deref()))
        .collect();

    match judge.rank(&req.query, repo, &candidates).await {
        Ok(verdict) => {
            let by_id: HashMap<u64, Symbol> = pool.into_iter().map(|s| (s.id, s)).collect();
            let mut scores = HashMap::new();
            let mut symbols = Vec::new();
            for (id, p) in verdict.ranking {
                scores.insert(
                    id,
                    MatchScore {
                        probability: p,
                        confidence: verdict.confidence,
                    },
                );
                if symbols.len() >= req.limit as usize {
                    continue;
                }
                if let Some(sym) = by_id.get(&id) {
                    symbols.push(sym.clone());
                }
            }
            let no_match = verdict.no_match;
            Ok((
                if no_match { Vec::new() } else { symbols },
                scores,
                no_match,
                Some(RerankInfo {
                    used: true,
                    reason: None,
                }),
            ))
        }
        Err(err) => {
            tracing::warn!(error = %err, "Jev rerank thất bại — fail-open về thứ tự pool");
            let symbols: Vec<Symbol> = pool.into_iter().take(req.limit as usize).collect();
            Ok((
                symbols,
                HashMap::new(),
                false,
                Some(RerankInfo {
                    used: false,
                    reason: Some(format!("jev lỗi: {err}")),
                }),
            ))
        }
    }
}

/// Pool tối thiểu để gọi Jev — dưới mức này coi như "không có gì để chọn".
const MIN_POOL: usize = 3;

/// Kind mặc định coi là "symbol code" — bỏ variable/parameter/field/constant.
const DEFAULT_KINDS: [SymbolKind; 7] = [
    SymbolKind::Function,
    SymbolKind::Method,
    SymbolKind::Class,
    SymbolKind::Interface,
    SymbolKind::Enum,
    SymbolKind::Module,
    SymbolKind::File,
];

/// Từ dừng khi token hoá query tự nhiên.
const STOPWORDS: [&str; 28] = [
    "the", "and", "for", "from", "with", "that", "this", "these", "those", "where", "when",
    "which", "what", "how", "why", "does", "did", "the", "are", "was", "were", "you", "our",
    "your", "please", "find", "show", "code",
];

/// Tách query thành token tìm kiếm: snake_case, camelCase, bỏ stopword/ngắn.
fn query_tokens(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in query.split(|c: char| !c.is_alphanumeric()) {
        if raw.is_empty() {
            continue;
        }
        // camelCase / PascalCase → nhiều token.
        let mut cur = String::new();
        let mut prev_lower = false;
        for ch in raw.chars() {
            if ch.is_uppercase() && prev_lower {
                if cur.len() >= 3 {
                    out.push(cur.to_ascii_lowercase());
                }
                cur.clear();
            }
            prev_lower = ch.is_lowercase() || ch.is_numeric();
            cur.push(ch);
        }
        if cur.len() >= 3 {
            out.push(cur.to_ascii_lowercase());
        }
    }
    out.retain(|t| !STOPWORDS.contains(&t.as_str()));
    out.dedup();
    out
}

/// Symbol test/bench — Jev từng chọn đúng file nhưng là `#[test]` fn, nên loại
/// trước khi đưa vào pool.
fn is_test_symbol(s: &Symbol) -> bool {
    let name = s.name.as_str();
    let file = s.file.as_str();
    name.starts_with("test_")
        || name.ends_with("_test")
        || name.ends_with("_tests")
        || file.contains("/tests/")
        || file.contains("/benches/")
        || file.starts_with("tests/")
}

/// Gom pool candidate cho Jev: query gốc + từng token, dedupe theo id.
async fn build_pool(
    idx: &codegraph_graph::GraphIndex,
    req: &ContextRequest,
) -> Result<Vec<Symbol>> {
    let max = codegraph_core::jev_config().max_candidates;
    let mut seen = HashMap::<u64, Symbol>::new();

    let push = |syms: Vec<Symbol>, seen: &mut HashMap<u64, Symbol>| {
        for s in syms {
            if seen.len() >= max {
                break;
            }
            if !DEFAULT_KINDS.contains(&s.kind) || is_test_symbol(&s) {
                continue;
            }
            seen.entry(s.id).or_insert(s);
        }
    };

    // Query gốc trước (giữ hành vi lexical cũ khi query là tên symbol).
    let mut queries = vec![req.query.clone()];
    queries.extend(query_tokens(&req.query));
    for q in queries {
        if seen.len() >= max {
            break;
        }
        let page = idx
            .search_symbol_paged_resumable(
                &q,
                None,
                SymbolMatch::Contains,
                Pagination {
                    limit: 8,
                    offset: 0,
                },
                None,
                None,
            )
            .await?;
        push(page.page, &mut seen);
    }
    Ok(seen.into_values().collect())
}

/// Hành vi cũ: `Contains` trên tên, fallback sang tên file bỏ extension.
async fn legacy_candidates(
    idx: &codegraph_graph::GraphIndex,
    req: &ContextRequest,
) -> Result<Vec<Symbol>> {
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
        let query_stripped = req
            .query
            .split('/')
            .next_back()
            .and_then(|f| {
                let without_ext = f.rsplit_once('.')?.0;
                if without_ext.is_empty() {
                    None
                } else {
                    Some(without_ext.to_string())
                }
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
    Ok(candidates)
}

/// Đọc nội dung 1 file qua `Source`, trả về dạng dòng.
///
/// `Symbol.file` lưu dạng `<root>/<rel>` — nhờ quy ước đó, tách `source.root()`
/// là ra `SourceEntry` mà không cần thêm cột `source_id` vào schema persist.
async fn load_source_lines(source: Option<&dyn Source>, file: &str) -> Result<Vec<String>> {
    let Some(src) = source else {
        return Err(Error::Other(
            "include_source=true nhưng không có Source".into(),
        ));
    };
    let entry = entry_from_symbol_file(file, src.root()).ok_or_else(|| {
        Error::Other(format!(
            "file `{file}` nằm ngoài root `{}` — index tạo từ nguồn khác?",
            src.root()
        ))
    })?;
    let bytes = read_entry(src, &entry, None).await?;
    // Provider từ xa có thể trả bytes thô (không phải source code) — báo lỗi
    // tường minh thay vì nuốt.
    let text = String::from_utf8(bytes)
        .map_err(|e| Error::Other(format!("`{file}` không phải UTF-8: {e}")))?;
    Ok(text.lines().map(str::to_owned).collect())
}

/// Strip `root/` prefix khỏi path (boundary-aware) — `None` giữ nguyên.
pub(crate) fn rel_path<'a>(p: &'a str, strip: Option<&str>) -> &'a str {
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
        if resp.no_match {
            let _ = writeln!(out, "\n_No symbol matches `{}`._", resp.query);
        } else {
            let _ = writeln!(out, "\n_No matches._");
        }
        return out;
    }
    for h in &resp.hits {
        let score = h
            .match_score
            .map(|s| format!(" (p={:.2})", s.probability))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "\n## `{}` — {} — `{}:{}`{score}",
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
            impl_trait: None,
            sig_key: None,
        }
    }

    /// `sym` nhưng cho phép chỉ định `file` — cần cho test đường query vì
    /// `Symbol.file` phải là `<root>/<rel>` để tách entry được.
    fn sym_at(name: &str, id: u64, file: &str) -> codegraph_core::Symbol {
        codegraph_core::Symbol {
            file: file.into(),
            ..sym(name, id)
        }
    }

    /// `include_source` phải đọc **qua trait** và dựng lại được entry từ
    /// `Symbol.file` (`<root>/<rel>`) — không cần cột `source_id`.
    #[tokio::test]
    async fn include_source_đọc_qua_source_trait() {
        let dir = tempfile::tempdir().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let abs = root.join("RestEndpoint.java");
        std::fs::write(
            abs.as_std_path(),
            "class RestEndpoint {}\nvoid handle() {}\n",
        )
        .unwrap();

        let db_str = format!("sqlite://{}", dir.path().join("t.db").to_string_lossy());
        {
            let mut idx = codegraph_graph::GraphIndex::open(&db_str).await.unwrap();
            let r = codegraph_graph::ParseResult {
                path: abs.as_str().to_string(),
                language: "java".into(),
                bytes: 0,
                lines: 2,
                symbols: vec![sym_at("RestEndpoint", 100, abs.as_str())],
                chains: std::collections::HashMap::new(),
                calls: vec![],
                fn_ptr_assigns: Vec::new(),
                file_deps: None,
            };
            idx.ingest(&[r], None).await.unwrap();
        }
        let sgi: Arc<SharedGraphIndex> =
            Arc::new(SharedGraphIndex::open(Some(db_str)).await.unwrap());

        let src = codegraph_source::DiskSource::for_query(root);
        let req = ContextRequest {
            query: "RestEndpoint".into(),
            include_source: true,
            ..ContextRequest::default()
        };
        let resp = build_response(&sgi, &req, Some(&src as &dyn Source))
            .await
            .unwrap();
        assert_eq!(
            resp.hits[0].source.as_deref(),
            Some("class RestEndpoint {}\nvoid handle() {}"),
            "sym có line=1 end_line=2 → lấy cả 2 dòng"
        );
    }

    /// `Source` không có (CLI/HTTP không truyền) → `source` là None, **không**
    /// panic — và không được âm thầm đọc `std::fs`.
    #[tokio::test]
    async fn include_source_không_có_source_thì_trả_none() {
        let dir = tempfile::tempdir().unwrap();
        let db_str = format!("sqlite://{}", dir.path().join("t.db").to_string_lossy());
        {
            let mut idx = codegraph_graph::GraphIndex::open(&db_str).await.unwrap();
            let r = codegraph_graph::ParseResult {
                path: "RestEndpoint.java".into(),
                language: "java".into(),
                bytes: 0,
                lines: 1,
                symbols: vec![sym("RestEndpoint", 100)],
                chains: std::collections::HashMap::new(),
                calls: vec![],
                fn_ptr_assigns: Vec::new(),
                file_deps: None,
            };
            idx.ingest(&[r], None).await.unwrap();
        }
        let sgi: Arc<SharedGraphIndex> =
            Arc::new(SharedGraphIndex::open(Some(db_str)).await.unwrap());

        let req = ContextRequest {
            query: "RestEndpoint".into(),
            include_source: true,
            ..ContextRequest::default()
        };
        let resp = build_response(&sgi, &req, None).await.unwrap();
        assert!(resp.hits[0].source.is_none());
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
                fn_ptr_assigns: Vec::new(),
                file_deps: None,
            };
            idx.ingest(&[r], None).await.unwrap();
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
            rerank: Rerank::None,
        };
        let sgi_arc: Arc<SharedGraphIndex> = Arc::new(sgi);
        let resp = build_response(&sgi_arc, &req, None).await.unwrap();
        assert!(!resp.hits.is_empty(), "phải match qua fallback filename");
        assert_eq!(resp.hits[0].symbol.name, "RestEndpoint");

        // Query "RestEndpoint" (không có extension) → match trực tiếp.
        let req2 = ContextRequest {
            query: "RestEndpoint".into(),
            ..req
        };
        let resp2 = build_response(&sgi_arc, &req2, None).await.unwrap();
        assert!(!resp2.hits.is_empty());
        assert_eq!(resp2.hits[0].symbol.name, "RestEndpoint");
    }

    #[test]
    fn query_tokens_tách_snake_camel_và_bỏ_stopword() {
        assert_eq!(
            query_tokens("where do we ValidateUserInput in the config"),
            vec!["validate", "user", "input", "config"]
        );
        assert_eq!(
            query_tokens("load_config_file"),
            vec!["load", "config", "file"],
            "snake_case cũng được tách để tìm theo từng phần"
        );
        assert_eq!(query_tokens("a b"), Vec::<String>::new());
    }

    #[test]
    fn is_test_symbol_nhận_diện_test_và_bench() {
        let mut s = sym("test_load", 1);
        assert!(is_test_symbol(&s));
        s = sym("load_tests", 2);
        assert!(is_test_symbol(&s));
        s = sym("load", 3);
        assert!(!is_test_symbol(&s));
        s = sym_at("load", 4, "/repo/tests/fixtures/load.rs");
        assert!(is_test_symbol(&s));
        s = sym_at("load", 5, "/repo/benches/bench.rs");
        assert!(is_test_symbol(&s));
    }

    #[test]
    fn markdown_hiện_xác_suất_và_no_match() {
        let resp = ContextResponse {
            query: "auth".into(),
            hits: vec![ContextHit {
                symbol: sym("auth", 1),
                callers: Vec::new(),
                callees: Vec::new(),
                source: None,
                match_score: Some(MatchScore {
                    probability: 0.62,
                    confidence: Some(0.9),
                }),
            }],
            no_match: false,
            rerank: None,
        };
        let md = render_markdown(&resp, None);
        assert!(md.contains("(p=0.62)"), "{md}");

        let empty = ContextResponse {
            query: "nonexistent".into(),
            hits: Vec::new(),
            no_match: true,
            rerank: None,
        };
        assert!(render_markdown(&empty, None).contains("_No symbol matches `nonexistent`._"));
    }

    // ── rerank = "jev" (judge giả, không gọi mạng) ──

    enum Fake {
        /// `(tên symbol, xác suất)` — map sang id thật của pool trong test.
        Rank(Vec<(&'static str, f32)>),
        NoMatch,
        Fail,
    }

    struct FakeJudge {
        fake: Fake,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl FakeJudge {
        fn new(fake: Fake) -> Self {
            Self {
                fake,
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl Judge for FakeJudge {
        async fn rank(
            &self,
            _query: &str,
            _repo: Option<&str>,
            _candidates: &[Candidate],
        ) -> Result<Verdict> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match &self.fake {
                Fake::Rank(r) => Ok(Verdict {
                    ranking: r
                        .iter()
                        .filter_map(|(name, p)| {
                            _candidates
                                .iter()
                                .find(|c| c.name == *name)
                                .map(|c| (c.id, *p))
                        })
                        .collect(),
                    no_match: false,
                    confidence: Some(0.8),
                    none_probability: 0.1,
                    fits: 0.9,
                }),
                Fake::NoMatch => Ok(Verdict::no_match(0.1)),
                Fake::Fail => Err(Error::Other("timeout".into())),
            }
        }
    }

    /// Index 3 symbol: `load_config`, `save_config`, `validate_user_input`;
    /// mọi symbol nằm trong `src/` nên không bị lọc test.
    async fn index_ba_symbol(dir: &tempfile::TempDir) -> Arc<SharedGraphIndex> {
        let db_str = format!("sqlite://{}", dir.path().join("t.db").to_string_lossy());
        {
            let mut idx = codegraph_graph::GraphIndex::open(&db_str).await.unwrap();
            let r = codegraph_graph::ParseResult {
                path: "/repo/src/config.rs".into(),
                language: "rust".into(),
                bytes: 0,
                lines: 3,
                symbols: vec![
                    sym_at("load_config", 1, "/repo/src/config.rs"),
                    sym_at("save_config", 2, "/repo/src/config.rs"),
                    sym_at("validate_user_input", 3, "/repo/src/input.rs"),
                ],
                chains: std::collections::HashMap::new(),
                calls: vec![],
                fn_ptr_assigns: Vec::new(),
                file_deps: None,
            };
            idx.ingest(&[r], None).await.unwrap();
        }
        Arc::new(SharedGraphIndex::open(Some(db_str)).await.unwrap())
    }

    fn jev_req() -> ContextRequest {
        ContextRequest {
            query: "config validate user input".into(),
            limit: 2,
            strip_prefix: Some("/repo".into()),
            rerank: Rerank::Jev,
            ..ContextRequest::default()
        }
    }

    #[tokio::test]
    async fn rerank_jev_sắp_thứ_tự_theo_xác_suất_và_gắn_score() {
        let dir = tempfile::tempdir().unwrap();
        let sgi = index_ba_symbol(&dir).await;
        // Jev trả ngược thứ tự pool: ưu tiên id 3, rồi 1, 2 ở cuối.
        let judge = FakeJudge::new(Fake::Rank(vec![
            ("validate_user_input", 0.72),
            ("load_config", 0.4),
            ("save_config", 0.33),
        ]));
        let resp = build_response_with_judge(&sgi, &jev_req(), None, Some(&judge))
            .await
            .unwrap();
        assert!(!resp.no_match);
        assert_eq!(judge.calls(), 1, "gọi Jev đúng 1 lần cho cả pool");
        assert_eq!(resp.hits.len(), 2, "cắt theo limit, không theo pool");
        assert_eq!(resp.hits[0].symbol.name, "validate_user_input");
        assert_eq!(resp.hits[1].symbol.name, "load_config");
        assert_eq!(resp.hits[0].match_score.map(|m| m.probability), Some(0.72));
        assert!(
            resp.rerank.as_ref().map(|r| r.used).unwrap_or(false),
            "báo đã rerank"
        );
        let md = render_markdown(&resp, Some("/repo"));
        assert!(md.contains("(p=0.72)"), "{md}");
    }

    #[tokio::test]
    async fn rerank_jev_no_match_thì_hits_rỗng() {
        let dir = tempfile::tempdir().unwrap();
        let sgi = index_ba_symbol(&dir).await;
        let judge = FakeJudge::new(Fake::NoMatch);
        let resp = build_response_with_judge(&sgi, &jev_req(), None, Some(&judge))
            .await
            .unwrap();
        assert!(resp.no_match);
        assert!(resp.hits.is_empty(), "no-match không được trả symbol bừa");
        let md = render_markdown(&resp, Some("/repo"));
        assert!(md.contains("_No symbol matches"), "{md}");
    }

    #[tokio::test]
    async fn rerank_jev_lỗi_thì_fail_open_về_pool() {
        let dir = tempfile::tempdir().unwrap();
        let sgi = index_ba_symbol(&dir).await;
        let judge = FakeJudge::new(Fake::Fail);
        let resp = build_response_with_judge(&sgi, &jev_req(), None, Some(&judge))
            .await
            .unwrap();
        assert!(!resp.no_match, "lỗi mạng không được làm mất kết quả");
        assert!(!resp.hits.is_empty());
        assert!(resp.hits.iter().all(|h| h.match_score.is_none()));
        let info = resp.rerank.expect("có lý do fail-open");
        assert!(!info.used);
        assert!(info.reason.unwrap_or_default().contains("timeout"));
    }

    #[tokio::test]
    async fn rerank_jev_pool_quá_nhỏ_thì_không_gọi_api() {
        let dir = tempfile::tempdir().unwrap();
        let sgi = index_ba_symbol(&dir).await;
        let judge = FakeJudge::new(Fake::Rank(vec![("load_config", 0.9)]));
        let req = ContextRequest {
            query: "config".into(),
            rerank: Rerank::Jev,
            ..ContextRequest::default()
        };
        let resp = build_response_with_judge(&sgi, &req, None, Some(&judge))
            .await
            .unwrap();
        assert_eq!(judge.calls(), 0, "2 candidate < MIN_POOL → không gọi Jev");
        assert!(resp.no_match);
        assert!(resp.hits.is_empty());
    }

    #[test]
    fn rerank_parse() {
        assert_eq!(Rerank::parse("jev"), Some(Rerank::Jev));
        assert_eq!(Rerank::parse("none"), Some(Rerank::None));
        assert_eq!(Rerank::parse(""), Some(Rerank::None));
        assert_eq!(Rerank::parse("llm"), None);
    }
}
