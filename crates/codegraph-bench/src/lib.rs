//! Pipeline đo chuẩn: **extract** (codegraph-extract: walk + parse) → **index**
//! (codegraph-graph: `ingest`) → **query** (search_symbol / callees / flow trên
//! index đã dựng). Tách riêng 2 crate để benchmark biết chi phí mỗi bên.
//!
//! `main.rs` lướt CLI (danh sách repo) + chạy Criterion; còn các hàm phase ở đây
//! được integration test dùng mà không cần Criterion.

use std::time::{Duration, Instant};
use std::sync::OnceLock;

use camino::Utf8Path;
use codegraph_core::{Error, SymbolKind};
use codegraph_extract::{ExtractStats, Orchestrator};
use codegraph_graph::{GraphIndex, ParseResult};
use tokio::runtime::Runtime;

/// Cấu hình một lần benchmark.
#[derive(Debug, Clone)]
pub struct BenchOptions {
    /// Giới hạn parser theo tên ngôn ngữ (`None` = trọn registry).
    pub langs: Option<Vec<String>>,
    /// Số symbol lấy mẫu cho phase query.
    pub queries: usize,
    /// Thêm `search_flow` (radix) vào phase query — để so bloom on/off.
    pub with_flow: bool,
}

impl Default for BenchOptions {
    fn default() -> Self {
        Self {
            langs: None,
            queries: 200,
            with_flow: false,
        }
    }
}

/// Một repo cần benchmark.
#[derive(Debug, Clone)]
pub struct Repo {
    pub name: String,
    pub root: camino::Utf8PathBuf,
}

/// Kết quả đo 1 repo (counts + thời gian mỗi phase).
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct RepoTimes {
    pub files: u64,
    pub symbols: u64,
    pub chains: u64,
    pub calls: u64,
    pub skipped: u64,
    pub extract_ms: f64,
    pub index_ms: f64,
    pub query_ms: f64,
    pub query_ops: usize,
    pub flow: bool,
}

fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("dựng tokio runtime")
    })
}

/// Dựng `Orchestrator` theo `--langs` (None = registry đầy đủ).
pub fn orchestrator(opts: &BenchOptions) -> Orchestrator {
    match &opts.langs {
        Some(langs) => {
            let names: Vec<&str> = langs.iter().map(String::as_str).collect();
            let parsers: Vec<_> = codegraph_extract::registry()
                .into_iter()
                .filter(|p| names.iter().any(|n| *n == p.name()))
                .collect();
            Orchestrator::new(parsers)
        }
        None => Orchestrator::with_registry(),
    }
}

/// Phase extract: walk + parse, trả `(parsed, stats)` — không ingest.
pub fn extract(
    orch: &Orchestrator,
    root: &Utf8Path,
) -> Result<(Vec<ParseResult>, ExtractStats), Error> {
    orch.parse_project(root)
}

/// Phase index: dựng in-memory `GraphIndex` + `ingest` toàn bộ parsed.
pub fn index(parsed: &[ParseResult]) -> Result<GraphIndex, Error> {
    runtime().block_on(async {
        let mut idx = GraphIndex::in_memory();
        idx.ingest(parsed).await?;
        Ok(idx)
    })
}

/// Lấy mẫu `n` tên function/method (sorted + dedup) để query.
pub fn sample_query_names(parsed: &[ParseResult], n: usize) -> Vec<String> {
    let mut names: Vec<String> = parsed
        .iter()
        .flat_map(|p| p.symbols.iter())
        .filter(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
        .map(|s| s.name.clone())
        .filter(|n| !n.is_empty())
        .collect();
    names.sort();
    names.dedup();
    names.truncate(n.max(1));
    names
}

/// Phase query: chạy bộ truy vấn mẫu trên index đã dựng; trả `(số phép đo, tổng
/// thời gian)`. Mỗi tên: `search_symbol` → hit đầu → `callees` + `flow` (+
/// `search_flow` nếu `with_flow`).
pub fn run_queries(
    idx: &GraphIndex,
    names: &[String],
    with_flow: bool,
) -> Result<(usize, Duration), Error> {
    runtime().block_on(async {
        let start = Instant::now();
        let mut ops = 0usize;
        for name in names {
            if let Ok(hits) = idx.search_symbol(name, None, 5).await {
                ops += 1;
                let Some(h) = hits.first() else { continue };
                // callees + flow = 2 phép đọc chain engine + flow.
                let _ = idx.callees(h.id).await;
                ops += 1;
                let _ = idx.flow(h.id).await;
                ops += 1;
                if with_flow {
                    let _ = idx.search_flow(&[h.id]).await;
                    ops += 1;
                }
            }
        }
        Ok((ops, start.elapsed()))
    })
}

/// Chạy 3 phase trong 1 pass, trả `RepoTimes` (dùng cho bảng + JSON).
pub fn measure_repo(
    orch: &Orchestrator,
    opts: &BenchOptions,
    repo: &Repo,
) -> anyhow::Result<RepoTimes> {
    let t0 = Instant::now();
    let (parsed, stats) = orch.parse_project(&repo.root)?;
    let extract_ms = ms(t0);

    let t1 = Instant::now();
    let idx = index(&parsed)?;
    let index_ms = ms(t1);

    let names = sample_query_names(&parsed, opts.queries);
    let t2 = Instant::now();
    let (ops, _) = run_queries(&idx, &names, opts.with_flow)?;
    let query_ms = ms(t2);

    Ok(RepoTimes {
        files: stats.files,
        symbols: stats.symbols,
        chains: stats.chains,
        calls: stats.calls,
        skipped: stats.skipped,
        extract_ms,
        index_ms,
        query_ms,
        query_ops: ops,
        flow: opts.with_flow,
    })
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}