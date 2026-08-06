//! CodSpeed bench: đo **extract → index → query** trên một danh sách repo thật.
//!
//! CodSpeed yêu cầu mỗi phase là một `bench_function` + `b.iter` chuẩn (runner
//! đo bằng hardware counters). Input là danh sách repo được nạp theo thứ tự:
//!
//! 1. env `CODEGRAPH_BENCH_REPOS_LIST` = file chứa 1 path repo mỗi dòng (CI ghi
//!    ra file này từ `.github/benches/repos/sources.txt` bằng
//!    `.github/benches/fetch_repos.sh`); nếu env được set nhưng file không đọc
//!    được/trống → báo lỗi và không chạy (tránh benchmark nhầm input);
//! 2. không set env → tự bench `crates/` (fallback cho lần chạy local đầu tiên).
//!
//! Phải dùng `criterion_group!`/`criterion_main!` — **không** tự
//! `Criterion::default()`. Dưới `cargo codspeed build` (bật `cfg(codspeed)`) các
//! macro này gọi `Criterion::new_instrumented()` để nối với runner; còn
//! `Criterion::default()` trong compat là dummy (`codspeed: None`) nên
//! `benchmark_group` sẽ panic `non instrumented codspeed interface`.
//!
//! Chạy:
//! - CI:  `cargo codspeed build -p codegraph-bench --features codspeed && cargo codspeed run`
//! - Local: `CODEGRAPH_BENCH_REPOS_LIST=repos.txt cargo bench -p codegraph-bench
//!   --bench codspeed --features codspeed` — không có runner thì compat resolve về
//!   criterion thường (wall-time).

#[cfg(feature = "codspeed")]
use codspeed_criterion_compat as crit;
#[cfg(not(feature = "codspeed"))]
use criterion as crit;

use camino::Utf8PathBuf;
use codegraph_bench::{
    BenchOptions, Repo, extract, index, orchestrator, run_queries, sample_query_names,
};

fn push_repo(out: &mut Vec<Repo>, path: Utf8PathBuf) {
    let name = path
        .file_name()
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.as_str().to_string());
    out.push(Repo { name, root: path });
}

fn load_repos() -> Vec<Repo> {
    let mut out = Vec::new();

    // 1) Danh sách rõ ràng từ env (ưu tiên — CI dùng `CODEGRAPH_BENCH_REPOS_LIST`).
    // Nếu env được set mà file không đọc được / trống → đây là lỗi cấu hình, KHÔNG
    // rơi vào fallback `crates` (tránh benchmark nhầm input trong CI).
    if let Ok(list_file) = std::env::var("CODEGRAPH_BENCH_REPOS_LIST") {
        match std::fs::read_to_string(&list_file) {
            Ok(body) => {
                for line in body.lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    push_repo(&mut out, Utf8PathBuf::from(line));
                }
                if out.is_empty() {
                    eprintln!(
                        "Cảnh báo: {list_file} không chứa repo nào (rỗng/comment) — bỏ qua benchmark."
                    );
                }
            }
            Err(e) => {
                eprintln!(
                    "Lỗi: không đọc được CODEGRAPH_BENCH_REPOS_LIST={list_file}: {e}. \
                     Bỏ qua benchmark thay vì benchmark nhầm input."
                );
                return out; // rỗng → benchmark_all in message và thoát.
            }
        }
        return out;
    }

    // 2) Fallback cuối: tự bench source của workspace — chỉ khi env KHÔNG được set
    //    (lần chạy local đầu tiên).
    if out.is_empty() {
        push_repo(&mut out, Utf8PathBuf::from("crates"));
    }
    out
}

/// Đăng ký toàn bộ bench (extract/index/query) theo danh sách repo.
/// Được `crit::criterion_main!` gọi với Criterion đã instrumented.
fn benchmark_all(c: &mut crit::Criterion) {
    let opts = BenchOptions {
        langs: None,
        queries: 200,
        with_flow: false,
    };
    let repos = load_repos();
    if repos.is_empty() {
        eprintln!("Không tìm thấy repo nào để bench (đặt CODEGRAPH_BENCH_REPOS_LIST)");
        return;
    }

    for repo in &repos {
        let name = repo.name.clone();
        let orch = orchestrator(&opts);

        // extract: walk + parse lại trong mỗi iteration (đo trọn phase).
        {
            let mut g = c.benchmark_group(format!("{name}/extract"));
            let orch = &orch;
            let root = repo.root.clone();
            g.bench_function("walk+parse", |b| {
                b.iter(|| {
                    let _ = std::hint::black_box(extract(orch, &root));
                });
            });
        }

        // Parse một lần để dùng chung cho index + query (không đếm lại extract).
        let parsed = match extract(&orch, &repo.root) {
            Ok((p, _)) => p,
            Err(e) => {
                eprintln!("[{name}] extract failed: {e}; skip");
                continue;
            }
        };
        let names = sample_query_names(&parsed, opts.queries);

        // index: dựng GraphIndex in-memory + ingest toàn bộ parsed.
        {
            let mut g = c.benchmark_group(format!("{name}/index"));
            let parsed = &parsed;
            g.bench_function("ingest", |b| {
                b.iter(|| {
                    let _ = std::hint::black_box(index(parsed));
                });
            });
        }

        // query: bộ truy vấn mẫu (search_symbol + callees + flow) trên index đã dựng.
        if let Ok(idx) = index(&parsed) {
            let mut g = c.benchmark_group(format!("{name}/query"));
            let names = &names;
            let with_flow = opts.with_flow;
            g.bench_function("sample", |b| {
                b.iter(|| {
                    let _ = std::hint::black_box(run_queries(&idx, names, with_flow));
                });
            });
        } else {
            eprintln!("[{name}] index failed; skip query");
        }
    }
}

// `criterion_main!` dưới CodSpeed gọi `new_instrumented()`; local (không
// `cfg(codspeed)`) resolve sang criterion thường.
crit::criterion_group!(benches, benchmark_all);
crit::criterion_main!(benches);
