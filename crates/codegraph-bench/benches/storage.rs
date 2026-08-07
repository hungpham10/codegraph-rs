//! Benchmark **storage backend** qua đúng pipeline luồng thật (extract → index →
//! query) như `codspeed.rs`, nhưng mỗi backend một group và mỗi iteration dựng
//! storage **mới** (file mới) để đo chi phí open+ingest không bị tích luỹ.
//!
//! Backend được chọn bằng DSN scheme (đúng cơ chế `GraphIndex::open(dsn)`):
//! - `in_memory` — `GraphIndex::in_memory()` (baseline RAM, không persist)
//! - `sqlite`    — `sqlite://<dir>/db.sqlite` (persist)
//! - `lmdb`      — `lmdb://<dir>/db` (persist)
//!
//! Chạy (repo list giống codspeed: `CODEGRAPH_BENCH_REPOS_LIST` hoặc fallback
//! `crates`):
//! ```bash
//! CODEGRAPH_BENCH_REPOS_LIST=repos.txt cargo bench -p codegraph-bench --bench storage
//! ```

use codegraph_bench::{
    BenchOptions, Repo, extract, index_at, orchestrator, run_queries, sample_query_names,
};
use criterion::{Criterion, black_box, criterion_group, criterion_main};

fn load_repos() -> Vec<Repo> {
    let mut out = Vec::new();
    if let Ok(list_file) = std::env::var("CODEGRAPH_BENCH_REPOS_LIST") {
        if let Ok(body) = std::fs::read_to_string(&list_file) {
            for line in body.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let name = std::path::Path::new(line)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(String::from)
                    .unwrap_or_else(|| line.to_string());
                out.push(Repo {
                    name,
                    root: line.into(),
                });
            }
        }
        return out;
    }
    out.push(Repo {
        name: "crates".into(),
        root: "crates".into(),
    });
    out
}

/// Dung lượng trên đĩa của một thư mục (đệ quy), dùng để so sánh footprint
/// của sqlite vs lmdb trên cùng một corpus.
fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(rd) = std::fs::read_dir(path) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(md) = std::fs::metadata(&p) {
                total += md.len();
            }
        }
    }
    total
}

/// Đo một lần dung lượng file thật trên đĩa cho sqlite vs lmdb (không chạy
/// trong benchmark lặp) để báo cáo footprint. Mỗi backend một tempdir riêng.
fn measure_on_disk(parsed: &[codegraph_graph::ParseResult]) {
    let sqlite_dir = tempfile::tempdir().unwrap().keep();
    let sqlite = format!("sqlite://{}/db.sqlite", sqlite_dir.to_string_lossy());
    if let Ok(_idx) = index_at(parsed, Some(&sqlite)) {}
    let sqlite_bytes = dir_size(&sqlite_dir);

    let lmdb_dir = tempfile::tempdir().unwrap().keep();
    let lmdb = format!("lmdb://{}", lmdb_dir.to_string_lossy());
    if let Ok(_idx) = index_at(parsed, Some(&lmdb)) {}
    let lmdb_bytes = dir_size(&lmdb_dir);

    eprintln!(
        "on-disk: sqlite={} bytes | lmdb={} bytes",
        sqlite_bytes, lmdb_bytes
    );
}

fn main_benchmark(c: &mut Criterion) {
    let opts = BenchOptions {
        langs: None,
        queries: 200,
        with_flow: false,
    };
    let repos = load_repos();
    for repo in &repos {
        let name = repo.name.clone();
        // Parse một lần (extract), dùng chung cho mọi backend.
        let parsed = match extract(&orchestrator(&opts), &repo.root) {
            Ok((p, _)) => p,
            Err(e) => {
                eprintln!("[{name}] extract failed: {e}; skip");
                continue;
            }
        };
        let names = sample_query_names(&parsed, opts.queries);
        measure_on_disk(&parsed);

        // ── index: mỗi backend một group, storage MỚI mỗi iteration ──
        // Mỗi backend là một closure `mk_dsn()` trả DSN cho một storage trống
        // (tempdir mới). Với in-memory, dsn = None.
        let mk_backends: Vec<(&str, Box<dyn Fn() -> Option<String>>)> = vec![
            ("in_memory", Box::new(|| None)),
            (
                "sqlite",
                Box::new(|| {
                    let dir = tempfile::tempdir().unwrap().keep();
                    Some(format!("sqlite://{}/db.sqlite", dir.to_string_lossy()))
                }),
            ),
            (
                "lmdb",
                Box::new(|| {
                    let dir = tempfile::tempdir().unwrap().keep();
                    Some(format!("lmdb://{}", dir.to_string_lossy()))
                }),
            ),
        ];

        for (bname, mk_dsn) in mk_backends {
            let parsed = &parsed;
            let mut g = c.benchmark_group(format!("{name}/{bname}/index"));
            g.bench_function("open+ingest", |b| {
                b.iter(|| {
                    let dsn = mk_dsn();
                    let _ = black_box(index_at(parsed, dsn.as_deref()));
                });
            });
            g.finish();
        }

        // ── query trên index in-memory (backend không ảnh hưởng query — engine
        // in-memory sau ingest) — giữ để pipeline giống codspeed. ──
        if let Ok(idx) = index_at(&parsed, None) {
            let mut g = c.benchmark_group(format!("{name}/query"));
            let names = &names;
            g.bench_function("sample", |b| {
                b.iter(|| {
                    let _ = black_box(run_queries(&idx, names, false));
                });
            });
            g.finish();
        }
    }
}

criterion_group!(benches, main_benchmark);
criterion_main!(benches);
