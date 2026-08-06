//! Benchmark thử nghiệm prune nhánh bằng bloom filter (feature `bloom-search`).
//! So sánh baseline (không bloom) vs có bloom — chạy 2 feature config:
//!
//! ```bash
//! cargo bench -p codegraph-graph --bench search_bloom            # baseline
//! cargo bench -p codegraph-graph --bench search_bloom --features bloom-search
//! ```
//!
//! Nhóm đo:
//! - `insert_*` — thông lượng insert (rõ chi phí duy trì bloom mỗi insert).
//! - `search_hit_*` — search pattern tồn tại (correctness, độ trễ có bloom).
//! - `search_miss_*` — search pattern KHÔNG tồn tại nhưng có chung prefix dài
//!   (đây là nơi bloom prune nhánh rỗng và phát huy nhất).

use codegraph_graph::Search;
use criterion::{black_box, criterion_group, criterion_main, Criterion};

const N: usize = 4000;

/// Sinh `n` keys có prefix dài dùng chung (radix sâu) — 4 prefixes lẫn nhau.
fn gen_keys(n: usize) -> Vec<Vec<u8>> {
    (0..n)
        .map(|i| {
            let prefix = match i % 4 {
                0 => "alpha",
                1 => "beta",
                2 => "gamma",
                _ => "delta",
            };
            format!("{prefix}_{i:06}").into_bytes()
        })
        .collect()
}

/// Các pattern chắc chắn tồn tại (substring).
const HITS: &[&[u8]] = &[b"alpha", b"beta_000", b"lph", b"000042", b"elta"];

/// Các pattern KHÔNG tồn tại nhưng có prefix dài giống keys → DFS phải dò sâu
/// nhiều nhánh rồi mới biết không có (bloom có thể prune chúng).
const MISSES: &[&[u8]] = &[b"alpha_999999", b"betazzz", b"gamma_q", b"qwerty", b"zzzz"];

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().build().unwrap()
}

fn build_index(n: usize) -> Search<u8> {
    runtime().block_on(async {
        let mut search = Search::in_memory(16);
        let keys = gen_keys(n);
        for (i, key) in keys.iter().enumerate() {
            let metas: Vec<Option<&[u8]>> = vec![None; key.len()];
            search.insert_chain(i + 1, key, &metas).await.unwrap();
        }
        search
    })
}

/// Cây sâu: nhiều keys, prefix chung rất dài → candidate-subtree lớn. Đây là
/// tình huống prune nhánh (bỏ cả nhánh con nhiều node) mới thực sự có lợi.
fn build_deep(n: usize) -> Search<u8> {
    runtime().block_on(async {
        let mut search = Search::in_memory(16);
        for i in 0..n {
            let key = format!("alpha_{i:06}").into_bytes();
            let metas: Vec<Option<&[u8]>> = vec![None; key.len()];
            search.insert_chain(i + 1, &key, &metas).await.unwrap();
        }
        search
    })
}

fn bench_insert(c: &mut Criterion) {
    c.bench_function("insert_2000", |b| {
        b.iter(|| {
            runtime().block_on(async {
                let mut search = Search::in_memory(16);
                let keys = gen_keys(2000);
                for (i, key) in keys.iter().enumerate() {
                    let metas: Vec<Option<&[u8]>> = vec![None; key.len()];
                    search.insert_chain(i + 1, key, &metas).await.unwrap();
                }
                black_box(&search);
            });
        });
    });
}

fn bench_search(c: &mut Criterion) {
    let search = build_index(N);
    let rt = runtime();

    c.bench_function("search_hit_5_patterns", |b| {
        b.iter(|| {
            rt.block_on(async {
                for p in HITS {
                    let r = search.search(p, None).await;
                    let _ = black_box(r);
                }
            });
        });
    });

    c.bench_function("search_miss_5_patterns", |b| {
        b.iter(|| {
            rt.block_on(async {
                for p in MISSES {
                    let r = search.search(p, None).await;
                    let _ = black_box(r);
                }
            });
        });
    });
}

/// Subtree lớn: miss chỉ cần diverge ở cuối prefix dài → baseline dò toàn bộ
/// nhánh lớn, bloom prune được ngay sau prefix.
fn bench_search_deep(c: &mut Criterion) {
    const DEEP: usize = 20_000;
    let search = build_deep(DEEP);
    let rt = runtime();

    c.bench_function("deep_search_miss_4_patterns", |b| {
        b.iter(|| {
            rt.block_on(async {
                for p in DEEP_MISSES {
                    let r = search.search(p, None).await;
                    let _ = black_box(r);
                }
            });
        });
    });
}

const DEEP_MISSES: &[&[u8]] = &[b"alpha_999999", b"alpha_888888", b"bet", b"zzzzzz"];

criterion_group!(benches, bench_insert, bench_search, bench_search_deep);
criterion_main!(benches);