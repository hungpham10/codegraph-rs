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
use codegraph_core::{EdgeMeta, Symbol};
use codegraph_graph::meminfo::{MemTracker, fmt_bytes, rss_bytes};

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
    })
}

/// Dựng `ParseResult` synthetic: `n` function, mỗi function gọi `fanout`
/// function khác (id local tính từ `SYMBOL_BASE`).
///
/// Mục tiêu là **làm đầy `edges` + `call_names`** — hai `HashMap` đang tốn
/// nhiều RAM nhất trong `GraphIndex`. Call name cố tình trùng lặp (chỉ vài
/// tên lib giả) để `call_names` có nhiều key chứa nhiều site, đúng hình dạng
/// repo thật.
fn synthetic_parse_result(n: usize, fanout: usize) -> codegraph_graph::ParseResult {
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

    let fanout = fanout.max(1);
    for i in 0..n {
        let caller = codegraph_core::SYMBOL_BASE + i as u64;
        let mut chain = vec![caller];
        for k in 0..fanout {
            // Callee = function k vòng sau (wrap-around) → id luôn hợp lệ,
            // không tự gọi chính mình.
            let callee = codegraph_core::SYMBOL_BASE + ((i + k + 1) % n) as u64;
            chain.push(callee);
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
        chains.insert(caller, chain);
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

fn measure_synthetic(n: usize, fanout: usize) -> anyhow::Result<RepoMem> {
    let mut tracker = MemTracker::new();
    tracker.mark("start");

    let parsed = vec![synthetic_parse_result(n, fanout)];
    tracker.mark("extract");

    let idx = index_at(&parsed, None)?;
    tracker.mark("index");

    let st = idx.stats();
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
        repo: format!("synthetic({n})"),
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
        results.push(measure_synthetic(n, cli.fanout)?);
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
    if let Some(peak) = codegraph_graph::meminfo::peak_rss_bytes() {
        println!("\npeak RSS (VmHWM): {}", fmt_bytes(peak));
    } else {
        println!("\npeak RSS: không đọc được trên nền tảng này");
    }
    Ok(())
}
