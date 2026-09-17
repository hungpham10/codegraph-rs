//! Context queries on a deterministic SQLite graph; setup is outside timing.

#[cfg(feature = "codspeed")]
use codspeed_criterion_compat as crit;
#[cfg(not(feature = "codspeed"))]
use criterion as crit;

use codegraph_context::{ContextRequest, build};
use codegraph_core::{SYMBOL_BASE, ScopeLevel, Symbol, SymbolKind};
use codegraph_graph::{GraphIndex, ParseResult, SharedGraphIndex};
use std::{collections::HashMap, hint::black_box, sync::Arc};

fn fixture(fan_in: usize) -> ParseResult {
    let mut symbols = Vec::new();
    let mut chains = HashMap::new();
    for i in 0..=fan_in * 2 {
        let id = SYMBOL_BASE + i as u64;
        let name = if i == 0 {
            "context_target".to_string()
        } else {
            format!("worker_{i:05}")
        };
        symbols.push(Symbol {
            id,
            name,
            kind: SymbolKind::Function,
            scope: ScopeLevel::Global,
            scope_id: 0,
            type_ref: 0,
            type_name: None,
            file: "context_fixture.rs".into(),
            line: 1,
            end_line: 1,
            signature: None,
            doc: None,
            annotations: Vec::new(),
            language: "rust".into(),
        });
        let chain = if i == 0 {
            vec![id]
        } else if i <= fan_in {
            vec![id, SYMBOL_BASE, SYMBOL_BASE]
        } else {
            vec![id, SYMBOL_BASE + (i - fan_in) as u64]
        };
        chains.insert(id, chain);
    }
    ParseResult {
        path: "context_fixture.rs".into(),
        language: "rust".into(),
        bytes: 0,
        lines: 1,
        symbols,
        chains,
        calls: Vec::new(),
    }
}

fn benchmark_context(c: &mut crit::Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    for fan_in in [32, 256] {
        let dir = tempfile::tempdir().unwrap();
        let dsn = format!("sqlite://{}", dir.path().join("context.db").display());
        let shared = rt.block_on(async {
            let mut idx = GraphIndex::open(&dsn).await.unwrap();
            idx.ingest(&[fixture(fan_in)]).await.unwrap();
            drop(idx);
            let shared = Arc::new(SharedGraphIndex::open(Some(dsn.clone())).await.unwrap());
            shared.ensure_fresh().await;
            shared
        });
        let mut group = c.benchmark_group(format!("context/sqlite/{fan_in}"));
        for (case, query, depth) in [
            ("warm_depth1", "context_target", 1),
            ("warm_depth2", "context_target", 2),
            ("warm_broad", "worker", 1),
            ("warm_no_hit", "worker_missing", 1),
        ] {
            let req = ContextRequest {
                query: query.into(),
                depth,
                ..ContextRequest::default()
            };
            let response = rt
                .block_on(codegraph_context::build_response(&shared, &req))
                .unwrap();
            match case {
                "warm_depth1" => assert_eq!(response.hits[0].callers.len(), fan_in),
                "warm_depth2" => assert_eq!(response.hits[0].callers.len(), fan_in * 2),
                "warm_broad" => assert_eq!(response.hits.len(), 5),
                _ => assert!(response.hits.is_empty()),
            }
            group.bench_function(case, |b| {
                b.iter(|| black_box(rt.block_on(build(&shared, black_box(&req))).unwrap()));
            });
        }
        let req = ContextRequest {
            query: "context_target".into(),
            ..ContextRequest::default()
        };
        group.bench_function("cold_depth1", |b| {
            b.iter_batched(
                || {
                    Arc::new(
                        rt.block_on(SharedGraphIndex::open(Some(dsn.clone())))
                            .unwrap(),
                    )
                },
                |fresh| black_box(rt.block_on(build(&fresh, black_box(&req))).unwrap()),
                crit::BatchSize::PerIteration,
            );
        });
        group.finish();
    }
}

crit::criterion_group!(benches, benchmark_context);
crit::criterion_main!(benches);
