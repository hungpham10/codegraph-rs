//! Profiler RAM: đo RSS theo từng phase của pipeline thật (extract → index →
//! query) trên danh sách repo, kèm ước lượng RAM của các HashMap trong
//! `GraphIndex` để **quy kết quả về đúng cấu trúc dữ liệu**.
//!
//! Vì sao cần: `GraphIndex` in-memory-first — mở index là nạp hết vào RAM
//! (`rebuild()` gọi `load_all_symbols` / `all_chains` / ...). Benchmark thời gian
//! của CodSpeed không thấy được cái giá này, nên phải đo riêng.
//!
//! Chạy:
//! ```bash
//! cargo run -p codegraph-bench --bin mem -- crates
//! CODEGRAPH_BENCH_REPOS_LIST=list.txt cargo run -p codegraph-bench --bin mem
//! ```

use camino::Utf8Path;
use clap::Parser;
use codegraph_bench::{BenchOptions, Repo, extract, index_at, orchestrator, run_queries};
use codegraph_core::{
    Annotation, CallRecord, EdgeMeta, EffectType, ScopeLevel, Symbol, SymbolKind,
};
use codegraph_graph::meminfo::{MemTracker, fmt_bytes, rss_bytes};
use codegraph_graph::memtrack::MemBreakdown;
use std::sync::OnceLock;

fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("dựng tokio runtime")
    })
}

/// Một dòng breakdown — JSON-friendly (thứ tự giữ nguyên như `ranked()`).
#[derive(serde::Serialize)]
struct BreakdownRow {
    structure: String,
    entries: u64,
    fixed_bytes: u64,
    heap_bytes: u64,
    total_bytes: u64,
}

fn breakdown_rows(b: &MemBreakdown) -> Vec<BreakdownRow> {
    b.ranked()
        .into_iter()
        .map(|(name, m)| BreakdownRow {
            structure: name.to_string(),
            entries: m.entries,
            fixed_bytes: m.fixed_bytes,
            heap_bytes: m.heap_bytes,
            total_bytes: m.total_bytes(),
        })
        .collect()
}

/// In breakdown cấu trúc (đã sort giảm dần) — phần trả lời câu hỏi "RAM nằm ở
/// đâu", tách khỏi RSS tổng.
fn print_breakdown(rows: &[BreakdownRow], caches: &[(String, usize)], rss_index: u64) {
    println!(
        "\n  {:<22} {:>9} {:>12} {:>12} {:>12}",
        "structure", "entries", "fixed", "heap", "total"
    );
    let mut accounted = 0u64;
    for row in rows {
        if row.entries == 0 && row.total_bytes == 0 {
            continue;
        }
        accounted += row.total_bytes;
        println!(
            "  {:<22} {:>9} {:>12} {:>12} {:>12}",
            row.structure,
            row.entries,
            fmt_bytes(row.fixed_bytes),
            fmt_bytes(row.heap_bytes),
            fmt_bytes(row.total_bytes)
        );
    }
    println!(
        "  {:<22} {:>9} {:>12} {:>12} {:>12}",
        "SUM accounted",
        "",
        "",
        "",
        fmt_bytes(accounted)
    );
    if rss_index > 0 {
        let pct = accounted as f64 * 100.0 / rss_index as f64;
        println!("  accounted / rss index = {pct:.1}% — phần còn lại: allocator + radix engine");
    }
    let busy: Vec<&(String, usize)> = caches.iter().filter(|(_, n)| *n > 0).collect();
    if !busy.is_empty() {
        print!("  LRU cache đang dùng: ");
        println!(
            "{}",
            busy.iter()
                .map(|(n, v)| format!("{n}={v}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
}

#[derive(Parser)]
#[command(name = "codegraph-mem", about = "Đo RAM của codegraph theo từng phase")]
struct Cli {
    /// Folder repo cần đo (nhiều được).
    #[arg(value_name = "REPO")]
    repos: Vec<String>,

    /// File chứa danh sách repo (mỗi dòng 1 path, trống + `#` bị bỏ).
    #[arg(short, long)]
    file: Option<String>,

    /// Giới hạn ngôn ngữ: `rust,go`…
    #[arg(long)]
    langs: Option<String>,

    /// Số symbol lấy mẫu cho phase query.
    #[arg(long, default_value_t = 200)]
    queries: usize,

    /// In JSON thay cho bảng.
    #[arg(long)]
    json: bool,

    /// Dựng index synthetic với N function thay vì đọc repo thật — deterministic,
    /// không phụ thuộc network. Mỗi function gọi `--fanout` function khác nên
    /// `edges` + `call_names` (hai cấu trúc đang tối ưu) có quy mô đáng kể.
    #[arg(long, value_name = "N")]
    synthetic: Option<usize>,

    /// Số callee mỗi function trong chế độ `--synthetic`.
    #[arg(long, default_value_t = 4, value_name = "K")]
    fanout: usize,

    /// Thành phần nào của index synthetic cần dựng — dùng để **đo vi sai**:
    /// mỗi shape thiếu một phần, hiệu RSS cho ra chi phí của phần đó (gồm cả
    /// radix engine mà `mem_breakdown` chưa tính).
    ///
    /// - `symbols` — chỉ symbol → `symbols` + `name_index` + **name engine**
    /// - `chains`  — symbol + chain, không call record → thêm **chain engine**
    /// - `full`    — kèm call record → thêm `call_names` + `edges`
    #[arg(long, value_enum, default_value_t = Shape::Full)]
    shape: Shape,

    /// Cách dựng index để đo.
    ///
    /// - `ingest` — parse + `ingest` thẳng vào in-memory. **RSS đo được ở đây là
    ///   high-water của allocator**: `ParseResult` giữ `CallRecord` xuyên suốt
    ///   `ingest` (`all_calls`/`recs_by_caller` chỉ giữ borrow/index) và drop xong
    ///   allocator không trả arena về OS → con số này KHÔNG phải chi phí thường trực.
    /// - `open` — ingest vào sqlite, **drop hết**, rồi `open` lại. Lúc này chỉ
    ///   còn `rebuild()` nạp blob từ storage và dựng HashMap, không có bản sao
    ///   tạm nào → đây mới là chi phí thường trực thật.
    #[arg(long, value_enum, default_value_t = Mode::Ingest)]
    mode: Mode,
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Mode {
    Ingest,
    Open,
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Ingest => "ingest",
            Self::Open => "open",
        };
        f.write_str(s)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Shape {
    Symbols,
    Chains,
    Full,
}

impl std::fmt::Display for Shape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Symbols => "symbols",
            Self::Chains => "chains",
            Self::Full => "full",
        };
        f.write_str(s)
    }
}

/// Kết quả 1 repo — RSS từng phase + phần RAM dự đoán theo cấu trúc.
#[derive(serde::Serialize)]
struct RepoMem {
    repo: String,
    symbols: u64,
    chains: u64,
    edges: u64,
    files: u64,
    /// RSS sau extract (bytes) — chưa có index.
    rss_after_extract: u64,
    /// RSS sau index (bytes) — có `GraphIndex` đầy đủ.
    rss_after_index: u64,
    /// RSS sau query (bytes).
    rss_after_query: u64,
    /// RSS đỉnh theo các mốc đã đánh dấu.
    rss_peak: u64,
    /// RSS tăng do `ingest` (sau_index − sau_extract).
    rss_index_delta: u64,
    /// `symbols.len() × size_of::<Symbol>()` — phần trong HashMap `symbols`.
    predicted_symbols: u64,
    /// `edges.len() × size_of::<EdgeMeta>()` — phần trong HashMap `edges`.
    predicted_edges: u64,
    /// Deep size từng cấu trúc, sort giảm dần.
    breakdown: Vec<BreakdownRow>,
    /// Tổng bytes đã quy được về cấu trúc (chưa gồm allocator + radix engine).
    accounted_total: u64,
    /// LRU cache đang giữ entry (tên → số entry).
    caches: Vec<(String, usize)>,
}

fn load_repos(cli: &Cli) -> Vec<Repo> {
    let mut paths: Vec<String> = cli.repos.clone();
    if paths.is_empty() {
        let list_file = cli
            .file
            .clone()
            .or_else(|| std::env::var("CODEGRAPH_BENCH_REPOS_LIST").ok());
        if let Some(list_file) = list_file
            && let Ok(body) = std::fs::read_to_string(&list_file)
        {
            for line in body.lines() {
                let line = line.trim();
                if !line.is_empty() && !line.starts_with('#') {
                    paths.push(line.to_string());
                }
            }
        }
    }
    if paths.is_empty() {
        paths.push("crates".to_string());
    }
    paths
        .into_iter()
        .map(|p| {
            let name = std::path::Path::new(&p)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&p)
                .to_string();
            Repo {
                name,
                root: p.into(),
            }
        })
        .collect()
}

fn measure(repo: &Repo, opts: &BenchOptions) -> anyhow::Result<RepoMem> {
    let orch = orchestrator(opts);
    let mut tracker = MemTracker::new();

    tracker.mark("start");
    let (parsed, _stats) = extract(&orch, repo.root.as_path())?;
    tracker.mark("extract");

    // `None` = in-memory storage → đo đúng RAM của `GraphIndex`, không lẫn
    // page cache của sqlite/lmdb.
    let idx = index_at(&parsed, None)?;
    tracker.mark("index");

    let names = codegraph_bench::sample_query_names(&parsed, opts.queries);
    let _ = run_queries(&idx, &names, opts.with_flow);
    tracker.mark("query");

    let st = idx.stats();
    let breakdown = runtime().block_on(idx.mem_breakdown());
    let sample = |label: &str| {
        tracker
            .samples()
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, v)| *v)
            .unwrap_or(0)
    };
    let after_extract = sample("extract");
    let after_index = sample("index");

    Ok(RepoMem {
        repo: repo.name.clone(),
        symbols: st.symbols,
        chains: st.chains,
        edges: st.edges,
        files: st.files,
        rss_after_extract: after_extract,
        rss_after_index: after_index,
        rss_after_query: sample("query"),
        rss_peak: tracker.peak(),
        rss_index_delta: after_index.saturating_sub(after_extract),
        predicted_symbols: st.symbols * size_of::<Symbol>() as u64,
        predicted_edges: st.edges * size_of::<EdgeMeta>() as u64,
        accounted_total: breakdown.accounted_total(),
        caches: breakdown.caches.clone(),
        breakdown: breakdown_rows(&breakdown),
    })
}

/// Dựng `ParseResult` synthetic: `n` function, mỗi function gọi `fanout`
/// function khác (id local tính từ `SYMBOL_BASE`).
///
/// Mục tiêu là **làm đầy `edges` + `call_names`** — hai `HashMap` đang tốn
/// nhiều RAM nhất trong `GraphIndex`. Call name cố tình trùng lặp (chỉ vài
/// tên lib giả) để `call_names` có nhiều key chứa nhiều site, đúng hình dạng
/// repo thật.
fn synthetic_parse_result(n: usize, fanout: usize, shape: Shape) -> codegraph_graph::ParseResult {
    // `n = 0` sẽ làm `% n` panic ở vòng sinh chain — chặn sớm, báo rõ.
    assert!(n > 0, "--synthetic cần N > 0");
    let mut symbols = Vec::with_capacity(n);
    let mut chains = std::collections::HashMap::with_capacity(n);
    let mut calls = Vec::with_capacity(n * fanout);

    for i in 0..n {
        symbols.push(Symbol {
            id: codegraph_core::SYMBOL_BASE + i as u64,
            name: format!("fn_{i:06}"),
            kind: SymbolKind::Function,
            scope: ScopeLevel::Global,
            scope_id: 0,
            type_ref: 0,
            type_name: None,
            file: format!("src/synthetic_{:04}.rs", i / 64),
            line: 1,
            end_line: 20,
            signature: Some(format!("fn fn_{i:06}(x: i64) -> i64")),
            doc: Some("Synthetic symbol để đo RAM — không phải code thật.".into()),
            annotations: Vec::<Annotation>::new(),
            language: "rust".into(),
        });
    }

    let fanout = if shape == Shape::Symbols {
        0
    } else {
        fanout.max(1)
    };
    for i in 0..n {
        let caller = codegraph_core::SYMBOL_BASE + i as u64;
        let mut chain = vec![caller];
        for k in 0..fanout {
            // Callee = function k vòng sau (wrap-around) → id luôn hợp lệ,
            // không tự gọi chính mình.
            let callee = codegraph_core::SYMBOL_BASE + ((i + k + 1) % n) as u64;
            chain.push(callee);
            // `shape = chains`: có chain nhưng KHÔNG call record → không dựng
            // `call_names`/`edges`, chỉ bật chain engine.
            if shape == Shape::Full {
                calls.push(CallRecord {
                    caller_id: caller,
                    call_name: format!("lib::helper_{}", k % 4),
                    position: chain.len() - 1,
                    arg_exprs: vec!["x".into()],
                    line: 5 + k as u32,
                    condition: (k % 3 == 0).then(|| "x > 0".to_string()),
                    is_loop_body: k % 5 == 0,
                    effect: EffectType::None,
                    effect_desc: None,
                    target_class: None,
                    target_method: None,
                });
            }
        }
        if shape != Shape::Symbols {
            chains.insert(caller, chain);
        }
    }

    codegraph_graph::ParseResult {
        path: "synthetic.rs".into(),
        language: "rust".into(),
        bytes: n as u64 * 512,
        lines: n as u32,
        symbols,
        chains,
        calls,
    }
}

fn measure_synthetic(n: usize, fanout: usize, shape: Shape) -> anyhow::Result<RepoMem> {
    let mut tracker = MemTracker::new();
    tracker.mark("start");

    let parsed = vec![synthetic_parse_result(n, fanout, shape)];
    tracker.mark("extract");

    let idx = index_at(&parsed, None)?;
    tracker.mark("index");

    let st = idx.stats();
    let breakdown = runtime().block_on(idx.mem_breakdown());
    let sample = |label: &str| {
        tracker
            .samples()
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, v)| *v)
            .unwrap_or(0)
    };
    let after_extract = sample("extract");
    let after_index = sample("index");

    Ok(RepoMem {
        repo: format!("synthetic({n},{shape})"),
        symbols: st.symbols,
        chains: st.chains,
        edges: st.edges,
        files: st.files,
        rss_after_extract: after_extract,
        rss_after_index: after_index,
        rss_after_query: 0,
        rss_peak: tracker.peak(),
        rss_index_delta: after_index.saturating_sub(after_extract),
        predicted_symbols: st.symbols * size_of::<Symbol>() as u64,
        predicted_edges: st.edges * size_of::<EdgeMeta>() as u64,
        accounted_total: breakdown.accounted_total(),
        caches: breakdown.caches.clone(),
        breakdown: breakdown_rows(&breakdown),
    })
}

/// Đo chi phí thường trực bằng cách **reopen**: ingest vào sqlite → drop sạch
/// (`index`, `ParseResult`) → `open` lại.
///
/// Vì sao cần: `ParseResult` giữ `CallRecord` xuyên suốt `ingest` và drop xong
/// allocator không trả arena về OS, nên RSS sau `ingest` là **peak**, không phải
/// live. `open` chỉ chạy `rebuild()` — nạp blob + dựng HashMap, không giữ `ParseResult`
/// tạm — nên
/// `rss_after_open − rss_before_open` mới là chi phí thường trực.
fn measure_reopen(n: usize, fanout: usize, shape: Shape) -> anyhow::Result<RepoMem> {
    let mut tracker = MemTracker::new();
    tracker.mark("start");

    let parsed = vec![synthetic_parse_result(n, fanout, shape)];

    // Thư mục riêng cho mỗi lần chạy — không dùng `tempfile` vì `src/bin/` không
    // có dev-dependency.
    let dir = std::env::temp_dir().join(format!("codegraph-mem-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| anyhow::anyhow!("tạo {}: {e}", dir.display()))?;
    let dsn = format!("sqlite://{}/db.sqlite", dir.display());

    let ingest_idx = index_at(&parsed, Some(&dsn))?;
    let st = ingest_idx.stats();
    drop(ingest_idx);
    // **Quan trọng**: nhả `ParseResult` (chứa toàn bộ `CallRecord`) trước khi đo,
    // không thì nó chiếm RSS suốt và mọi delta đều sai.
    drop(parsed);
    tracker.mark("before_open");

    let idx = runtime().block_on(codegraph_graph::GraphIndex::open(&dsn))?;
    tracker.mark("open");

    let breakdown = runtime().block_on(idx.mem_breakdown());
    let sample = |label: &str| {
        tracker
            .samples()
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, v)| *v)
            .unwrap_or(0)
    };
    let before_open = sample("before_open");
    let after_open = sample("open");

    let _ = std::fs::remove_dir_all(&dir);

    Ok(RepoMem {
        repo: format!("reopen({n},{shape})"),
        symbols: st.symbols,
        chains: st.chains,
        edges: st.edges,
        files: st.files,
        rss_after_extract: before_open,
        rss_after_index: after_open,
        rss_after_query: 0,
        rss_peak: tracker.peak(),
        rss_index_delta: after_open.saturating_sub(before_open),
        predicted_symbols: st.symbols * size_of::<Symbol>() as u64,
        predicted_edges: st.edges * size_of::<EdgeMeta>() as u64,
        accounted_total: breakdown.accounted_total(),
        caches: breakdown.caches.clone(),
        breakdown: breakdown_rows(&breakdown),
    })
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if rss_bytes().is_none() {
        eprintln!("cảnh báo: không đọc được RSS trên nền tảng này — số liệu sẽ là 0");
    }
    let opts = BenchOptions {
        langs: cli
            .langs
            .as_ref()
            .map(|s| s.split(',').map(|x| x.trim().to_string()).collect()),
        queries: cli.queries,
        with_flow: false,
    };

    let mut results = Vec::new();
    if let Some(n) = cli.synthetic {
        results.push(match cli.mode {
            Mode::Ingest => measure_synthetic(n, cli.fanout, cli.shape)?,
            Mode::Open => measure_reopen(n, cli.fanout, cli.shape)?,
        });
    } else {
        for repo in load_repos(&cli) {
            if Utf8Path::from_path(repo.root.as_std_path()).is_none() {
                eprintln!("bỏ qua {}: path không phải UTF-8", repo.root);
                continue;
            }
            match measure(&repo, &opts) {
                Ok(r) => results.push(r),
                Err(e) => eprintln!("lỗi {}: {e}", repo.root),
            }
        }
    }

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&results)?);
        return Ok(());
    }

    println!(
        "{:<14} {:>8} {:>8} {:>10} {:>10} {:>10} {:>12} {:>12}",
        "repo",
        "symbols",
        "edges",
        "rss extract",
        "rss index",
        "Δ index",
        "pred symbols",
        "pred edges"
    );
    for r in &results {
        println!(
            "{:<14} {:>8} {:>8} {:>10} {:>10} {:>10} {:>12} {:>12}",
            r.repo,
            r.symbols,
            r.edges,
            fmt_bytes(r.rss_after_extract),
            fmt_bytes(r.rss_after_index),
            fmt_bytes(r.rss_index_delta),
            fmt_bytes(r.predicted_symbols),
            fmt_bytes(r.predicted_edges),
        );
    }
    for r in &results {
        println!("\n=== breakdown: {} ===", r.repo);
        print_breakdown(&r.breakdown, &r.caches, r.rss_after_index);
    }
    if let Some(peak) = codegraph_graph::meminfo::peak_rss_bytes() {
        println!("\npeak RSS (VmHWM): {}", fmt_bytes(peak));
    } else {
        println!("\npeak RSS: không đọc được trên nền tảng này");
    }
    Ok(())
}
