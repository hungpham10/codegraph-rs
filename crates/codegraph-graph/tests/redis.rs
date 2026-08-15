//! Integration tests cho Redis backend (feature `redis`).
//!
//! Chỉ chạy khi có Redis thật: đặt `TEST_REDIS_DSN` (ví dụ
//! `redis://127.0.0.1:6379`) rồi bật feature + bỏ ignore:
//!
//! ```sh
//! TEST_REDIS_DSN=redis://127.0.0.1:6379 \
//! cargo test -p codegraph-graph --features redis --test redis -- --ignored
//! ```
//!
//! Keyspace prefix được dẫn xuất từ số DB trong DSN (`/15` → `codegraph:idx:15`),
//! nên test này (DB mặc định 0) không đụng hàng với unit test nội bộ (DB 15).
//! Test mặc định bị `#[ignore]` nên không ảnh hưởng `cargo test` thường.

#![cfg(feature = "redis")]

use codegraph_core::{
    CallRecord, EffectType, ScopeLevel, StorageRoute, SYMBOL_BASE, Symbol, SymbolKind,
};
use codegraph_graph::{GraphIndex, ParseResult};
use std::collections::HashMap;

fn sym(file: &str, name: &str, id: u64) -> Symbol {
    Symbol {
        id,
        name: name.to_string(),
        kind: SymbolKind::Function,
        scope: ScopeLevel::Global,
        scope_id: 0,
        type_ref: 0,
        type_name: None,
        file: file.to_string(),
        line: 1,
        end_line: 1,
        signature: None,
        doc: None,
        annotations: Vec::new(),
        language: "ts".to_string(),
    }
}

fn result(
    path: &str,
    symbols: Vec<Symbol>,
    chains: HashMap<u64, Vec<u64>>,
    calls: Vec<CallRecord>,
) -> ParseResult {
    ParseResult {
        path: path.to_string(),
        language: "ts".to_string(),
        bytes: 0,
        lines: 0,
        symbols,
        chains,
        calls,
    }
}

/// Ingest → reopen trên Redis: entity sống lại từ keyspace, query surface
/// (symbols/chains/edges/files) khớp, version bump đúng.
#[tokio::test]
#[ignore = "requires a running Redis; set TEST_REDIS_DSN"]
async fn redis_ingest_reopen_roundtrip() {
    let dsn = match std::env::var("TEST_REDIS_DSN") {
        Ok(d) => d,
        Err(_) => {
            eprintln!("skip: TEST_REDIS_DSN not set");
            return;
        }
    };

    let calls = vec![CallRecord {
        caller_id: SYMBOL_BASE,
        call_name: "b".to_string(),
        position: 1,
        arg_exprs: vec!["x".to_string()],
        line: 3,
        condition: None,
        is_loop_body: false,
        effect: EffectType::None,
        effect_desc: None,
        target_class: None,
        target_method: None,
    }];
    let r = result(
        "a.ts",
        vec![
            sym("a.ts", "a", SYMBOL_BASE),
            sym("a.ts", "b", SYMBOL_BASE + 1),
        ],
        HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE, SYMBOL_BASE + 1])]),
        calls,
    );

    {
        let mut idx = GraphIndex::open_route(&StorageRoute::Local(dsn.clone()))
            .await
            .expect("open redis");
        idx.ingest(&[r]).await.expect("ingest");
        assert_eq!(idx.version(), 1);
    }

    // Reopen cùng keyspace → query lại được toàn bộ.
    let idx = GraphIndex::open_route(&StorageRoute::Local(dsn))
        .await
        .expect("reopen redis");
    assert_eq!(idx.version(), 1);
    assert_eq!(idx.stats().symbols, 2);
    assert_eq!(idx.stats().chains, 1);
    assert_eq!(idx.stats().edges, 1);
    assert_eq!(idx.files().len(), 1);
    assert_eq!(idx.files()[0].path, "a.ts");

    let callees = idx.callees(SYMBOL_BASE).await.unwrap();
    assert_eq!(callees.len(), 1);
    assert_eq!(callees[0].name, "b");
}

/// Ingest rỗng = full wipe trên Redis: entity cũ biến mất, version vẫn bump.
#[tokio::test]
#[ignore = "requires a running Redis; set TEST_REDIS_DSN"]
async fn redis_empty_ingest_wipes_store() {
    let dsn = match std::env::var("TEST_REDIS_DSN") {
        Ok(d) => d,
        Err(_) => {
            eprintln!("skip: TEST_REDIS_DSN not set");
            return;
        }
    };

    let r = result(
        "a.ts",
        vec![sym("a.ts", "a", SYMBOL_BASE)],
        HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE])]),
        vec![],
    );
    let mut idx = GraphIndex::open_route(&StorageRoute::Local(dsn))
        .await
        .expect("open redis");
    idx.ingest(&[r]).await.expect("ingest");
    assert_eq!(idx.stats().symbols, 1);

    idx.ingest(&[]).await.expect("empty ingest");
    assert_eq!(idx.version(), 2);
    assert_eq!(idx.stats().symbols, 0);
    assert!(idx.symbol_by_id(SYMBOL_BASE).is_none());
}
