//! Benchmark CLI: đưa danh sách folder repo → đo extract/index/query.
//!
//! Chạy:       `cargo run -p codegraph-bench -- /path/to/repo1 /path/to/repo2`
//! Danh sách:  `cargo run -p codegraph-bench -- --file repos.txt`
//! Bloom:      `cargo run -p codegraph-bench --features bloom -- --flow /path/to/repo`

use std::time::Duration;

use camino::Utf8PathBuf;
use clap::Parser;
use codegraph_bench::{
    extract, index, orchestrator, run_queries, sample_query_names, BenchOptions, Repo, RepoTimes,
};

#[derive(Parser)]
#[command(name = "codegraph-bench", about = "Benchmark codegraph-extract + codegraph-graph trên các repo thật")]
struct Cli {
    /// Folder repo cần benchmark (nhiều được).
    #[arg(value_name = "REPO")]
    repos: Vec<String>,

    /// File chứa danh sách repo (mỗi dòng 1 path, trống + `#` bị bỏ qua).
    #[arg(short, long)]
    file: Option<String>,

    /// Giới hạn ngôn ngữ: danh sách tách bằng phẩy, VD `rust,go`.
    #[arg(long)]
    langs: Option<String>,

    /// Số symbol lấy mẫu cho phase query.
    #[arg(long, default_value_t = 200)]
    queries: usize,

    /// Chạy `search_flow` (radix) trong phase query — build kèm `--features bloom`.
    #[arg(long)]
    flow: bool,

    /// Chỉ in bảng + JSON, bỏ qua Criterion statistical pass.
    #[arg(long)]
    no_criterion: bool,

    /// Xuất JSON thay cho bảng.
    #[arg(long)]
    json: bool,

    /// Criterion: số sample (Criterion tối thiểu 10).
    #[arg(long, default_value_t = 10)]
    sample_size: usize,

    /// Criterion: thời gian warm-up (giây).
    #[arg(long, default_value_t = 0.5)]
    warmup: f64,

    /// Criterion: thời gian đo (giây).
    #[arg(long, default_value_t = 1.0)]
    measure: f64,
}

#[derive(serde::Serialize)]
struct RepoJson {
    repo: String,
    #[serde(flatten)]
    times: RepoTimes,
}

#[derive(serde::Serialize, Default)]
struct TotalsJson {
    repos: usize,
    files: u64,
    symbols: u64,
    extract_ms: f64,
    index_ms: f64,
    query_ms: f64,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let repos = load_repos(&cli)?;
    if repos.is_empty() {
        anyhow::bail!("Không có repo nào — truyền folder hoặc dùng --file with danh sách");
    }

    let opts = BenchOptions {
        langs: cli.langs.as_ref().map(|s| {
            s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect()
        }),
        queries: cli.queries,
        with_flow: cli.flow,
    };
    let orch = orchestrator(&opts);

    // ── Pass 1: bảng + JSON (đo 1 pass mỗi repo). ──
    let mut rows: Vec<(Repo, RepoTimes)> = Vec::new();
    for r in &repos {
        print!("{} ... ", r.name);
        std::io::Write::flush(&mut std::io::stdout())?;
        let t = codegraph_bench::measure_repo(&orch, &opts, r)?;
        println!("extract {:7.1}ms  index {:7.1}ms  query {:6.1}ms", t.extract_ms, t.index_ms, t.query_ms);
        rows.push((r.clone(), t));
    }
    render_table(&rows);

    // ── Pass 2: Criterion statistical per phase. ──
    if !cli.no_criterion {
        criterion_pass(&repos, &opts, &cli);
    }

    if cli.json {
        render_json(&rows);
    }
    Ok(())
}

fn load_repos(cli: &Cli) -> anyhow::Result<Vec<Repo>> {
    let mut out = Vec::new();
    if let Some(file) = &cli.file {
        for line in std::fs::read_to_string(file)?.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            push_repo(&mut out, line);
        }
    }
    for p in &cli.repos {
        push_repo(&mut out, p);
    }
    Ok(out)
}

fn push_repo(out: &mut Vec<Repo>, path: &str) {
    let root = Utf8PathBuf::from(path);
    let name = root
        .file_name()
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.to_string());
    out.push(Repo { name, root });
}

fn render_table(rows: &[(Repo, RepoTimes)]) {
    println!("\n── Summary ──");
    println!(
        "{:<24} {:>8} {:>8} {:>8} {:>8} {:>10} {:>10} {:>10} {:>8}",
        "repo", "files", "symbols", "chains", "calls", "extract_ms", "index_ms", "query_ms", "ops"
    );
    let mut total_files = 0u64;
    let mut total_symbols = 0u64;
    let mut total_extract = 0f64;
    let mut total_index = 0f64;
    let mut total_query = 0f64;
    for (r, t) in rows {
        total_files += t.files;
        total_symbols += t.symbols;
        total_extract += t.extract_ms;
        total_index += t.index_ms;
        total_query += t.query_ms;
        println!(
            "{:<24} {:>8} {:>8} {:>8} {:>8} {:>10.1} {:>10.1} {:>10.1} {:>8}",
            r.name, t.files, t.symbols, t.chains, t.calls, t.extract_ms, t.index_ms, t.query_ms, t.query_ops
        );
    }
    println!(
        "{:<24} {:>8} {:>8} {:>8} {:>8} {:>10.1} {:>10.1} {:>10.1}",
        "TOTAL", total_files, total_symbols, "-", "-", total_extract, total_index, total_query
    );
    println!();
}

fn render_json(rows: &[(Repo, RepoTimes)]) {
    let mut total = TotalsJson::default();
    let items: Vec<RepoJson> = rows
        .iter()
        .map(|(r, t)| {
            total.repos += 1;
            total.files += t.files;
            total.symbols += t.symbols;
            total.extract_ms += t.extract_ms;
            total.index_ms += t.index_ms;
            total.query_ms += t.query_ms;
            RepoJson { repo: r.name.clone(), times: t.clone() }
        })
        .collect();
    let out = serde_json::json!({ "repos": items, "total": total });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

/// Pass Criterion: per-phase statistical trên cùng repos.
fn criterion_pass(repos: &[Repo], opts: &BenchOptions, cli: &Cli) {
    let sample_size = cli.sample_size.max(10);
    // Criterion đòi duration dương — clamp để tránh 0/âm.
    let warmup = Duration::from_secs_f64(cli.warmup.max(0.01));
    let measure = Duration::from_secs_f64(cli.measure.max(0.01));
    let mut c = criterion::Criterion::default()
        .sample_size(sample_size)
        .warm_up_time(warmup)
        .measurement_time(measure);

    for repo in repos {
        // Stage parsed một lần rồi dùng cho cả index + query (không parse lại).
        let orch = orchestrator(opts);
        let name = repo.name.clone();
        // extract.
        {
            let mut g = c.benchmark_group(&format!("{name}/extract"));
            let orch = &orch;
            let root = repo.root.clone();
            g.bench_function("walk+parse", |b| {
                b.iter(|| {
                    let _ = std::hint::black_box(extract(orch, &root));
                });
            });
        }
        let parsed = match extract(&orch, &repo.root) {
            Ok((p, _)) => p,
            Err(e) => {
                eprintln!("[criterion] {name}: extract failed: {e}; skip");
                continue;
            }
        };
        let names = sample_query_names(&parsed, opts.queries);
        // index.
        {
            let mut g = c.benchmark_group(&format!("{name}/index"));
            let parsed = &parsed;
            g.bench_function("ingest", |b| {
                b.iter(|| {
                    let _ = std::hint::black_box(index(parsed));
                });
            });
        }
        // query.
        {
            let mut g = c.benchmark_group(&format!("{name}/query"));
            let names = &names;
            let with_flow = opts.with_flow;
            if let Ok(idx) = index(&parsed) {
                g.bench_function("sample", |b| {
                    b.iter(|| {
                        let _ = std::hint::black_box(run_queries(&idx, names, with_flow));
                    });
                });
            } else {
                eprintln!("[criterion] {name}: index failed; skip query");
            }
        }
    }
}