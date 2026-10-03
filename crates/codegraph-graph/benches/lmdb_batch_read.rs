//! Benchmark đọc LMDB: so sánh đọc TỪNG node (mỗi lần 1 read-txn) với đọc BATCH
//! (nhiều node trong 1 read-txn) — đo tác động của `get_nodes`/`get_childrens`.
//!
//! ```bash
//! cargo bench -p codegraph-graph --bench lmdb_batch_read --features lmdb
//! ```
//!
//! Nhóm đo:
//! - `single_read_{1,16,64}` — gọi `get_node` lặp lại cho từng id.
//! - `batch_read_{1,16,64}`  — gọi `get_nodes` 1 lần cho cả id.
//! - `search_dfs`            — DFS thật trên trie đã dựng (đo end-to-end).

use codegraph_graph::{CategoryStorage, LmdbStorage};
use criterion::{Criterion, black_box, criterion_group, criterion_main};

const N_NODES: usize = 4096;

/// Bộ id hợp lệ để đọc — dựng sẵn node thật trong LMDB.
fn setup(rt: &tokio::runtime::Runtime) -> (tempfile::TempDir, String, Vec<usize>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench.lmdb").to_string_lossy().into_owned();
    let ids = rt.block_on(async {
        let mut s = LmdbStorage::open(&path).await.unwrap();
        let mut ids = Vec::with_capacity(N_NODES);
        for i in 0..N_NODES {
            // Prefix ngắn, giá trị record tăng dần — mô phỏng trie phẳng.
            let id = s
                .new_node(format!("n{i:05}").into_bytes(), i + 1)
                .await
                .unwrap();
            ids.push(id);
        }
        ids
    });
    (dir, path, ids)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
}

fn bench_batch_vs_single(c: &mut Criterion) {
    let rt = runtime();
    let (_dir, path, ids) = setup(&rt);
    let st = rt.block_on(async { LmdbStorage::open(&path).await.unwrap() });

    let mut group = c.benchmark_group("lmdb_node_read");

    for &k in &[1usize, 16, 64] {
        let ids: Vec<usize> = ids.iter().copied().take(k).collect();

        group.bench_function(format!("single_{k}"), |b| {
            b.iter(|| {
                rt.block_on(async {
                    for &id in &ids {
                        black_box(st.get_node(id).await.unwrap());
                    }
                });
            });
        });

        group.bench_function(format!("batch_{k}"), |b| {
            b.iter(|| {
                rt.block_on(async {
                    black_box(st.get_nodes(&ids).await.unwrap());
                });
            });
        });
    }

    group.finish();
}

/// Đọc children: `get_children` lặp vs `get_childrens` batch.
fn bench_children_batch(c: &mut Criterion) {
    let rt = runtime();
    let (_dir, path, _ids) = setup(&rt);
    let parents = rt.block_on(async {
        let mut s = LmdbStorage::open(&path).await.unwrap();
        let mut parents = Vec::new();
        for i in 0..64usize {
            let p = s.new_node(format!("p{i:03}").into_bytes(), i).await.unwrap();
            // Mỗi parent có 32 child.
            for j in 0..32usize {
                let c = s
                    .new_node(format!("c{i:03}_{j:03}").into_bytes(), j)
                    .await
                    .unwrap();
                let mut tx = s.new_tx();
                tx.add_child(p, c).await.unwrap();
                tx.commit().await.unwrap();
            }
            parents.push(p);
        }
        parents
    });
    let st = rt.block_on(async { LmdbStorage::open(&path).await.unwrap() });

    let mut group = c.benchmark_group("lmdb_children_read");
    for &k in &[1usize, 16, 64] {
        let ps: Vec<usize> = parents.iter().copied().take(k).collect();
        group.bench_function(format!("single_{k}"), |b| {
            b.iter(|| {
                rt.block_on(async {
                    for &p in &ps {
                        black_box(st.get_children(p).await.unwrap());
                    }
                });
            });
        });
        group.bench_function(format!("batch_{k}"), |b| {
            b.iter(|| {
                rt.block_on(async {
                    black_box(st.get_childrens(&ps).await.unwrap());
                });
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_batch_vs_single,
    bench_children_batch
);
criterion_main!(benches);
