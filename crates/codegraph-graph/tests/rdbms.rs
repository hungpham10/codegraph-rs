//! Integration tests cho RDBMS backend (Postgres/MySQL, feature `postgres` /
//! `mysql`) — multi-tenant + sharding.
//!
//! Chỉ chạy khi có DB thật: đặt `TEST_RDBMS_DSN` (`postgres://...` hoặc
//! `mysql://...`) và `TEST_RDBMS_REPO_ID` (u64), rồi bật feature + bỏ ignore:
//!
//! ```sh
//! TEST_RDBMS_DSN=postgres://user:pass@localhost:5432/codegraph \
//! TEST_RDBMS_REPO_ID=123 \
//! cargo test -p codegraph-graph --features postgres --test rdbms -- --ignored
//! ```
//!
//! Schema (`sql/<engine>/001` + `002`) phải đã được apply thủ công lên server
//! trước (migration không chạy tự động). Test mặc định bị `#[ignore]` nên không
//! ảnh hưởng `cargo test` thường.

#![cfg(any(feature = "postgres", feature = "mysql"))]

use codegraph_core::{CallRecord, EffectType, ScopeLevel, SYMBOL_BASE, Symbol, SymbolKind};
use codegraph_graph::{GraphIndex, ParseResult};
use codegraph_core::StorageRoute;
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

/// Ingest → reopen trên backend RDBMS: entity sống lại từ partition `repo_id`,
/// query surface (symbols/chains/edges/files) khớp, version bump đúng.
#[tokio::test]
#[ignore = "requires a running Postgres/MySQL; set TEST_RDBMS_DSN + TEST_RDBMS_REPO_ID"]
async fn rdbms_ingest_reopen_roundtrip() {
    let dsn = match std::env::var("TEST_RDBMS_DSN") {
        Ok(d) => d,
        Err(_) => {
            eprintln!("skip: TEST_RDBMS_DSN not set");
            return;
        }
    };
    let repo_id: u64 = std::env::var("TEST_RDBMS_REPO_ID")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let route = StorageRoute::Sharded {
        dsns: vec![dsn],
        repo_id: Some(repo_id),
        root: None,
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
        let mut idx = GraphIndex::open_route(&route).await.expect("open rdbms");
        idx.ingest(&[r]).await.expect("ingest");
        assert_eq!(idx.version(), 1);
    }

    // Reopen cùng repo_id → query lại được toàn bộ từ partition.
    let idx = GraphIndex::open_route(&route).await.expect("reopen rdbms");
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

/// Ingest rỗng = full wipe trên partition `repo_id`: entity cũ biến mất, version
/// vẫn bump (như sqlite/lmdb).
#[tokio::test]
#[ignore = "requires a running Postgres/MySQL; set TEST_RDBMS_DSN + TEST_RDBMS_REPO_ID"]
async fn rdbms_empty_ingest_wipes_store() {
    let dsn = match std::env::var("TEST_RDBMS_DSN") {
        Ok(d) => d,
        Err(_) => {
            eprintln!("skip: TEST_RDBMS_DSN not set");
            return;
        }
    };
    let repo_id: u64 = std::env::var("TEST_RDBMS_REPO_ID")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let route = StorageRoute::Sharded {
        dsns: vec![dsn],
        repo_id: Some(repo_id),
        root: None,
    };

    let r = result(
        "a.ts",
        vec![sym("a.ts", "a", SYMBOL_BASE)],
        HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE])]),
        vec![],
    );
    let mut idx = GraphIndex::open_route(&route).await.expect("open rdbms");
    idx.ingest(&[r]).await.expect("ingest");
    assert_eq!(idx.stats().symbols, 1);

    idx.ingest(&[]).await.expect("empty ingest");
    assert_eq!(idx.version(), 2);
    assert_eq!(idx.stats().symbols, 0);
    assert!(idx.symbol_by_id(SYMBOL_BASE).is_none());
}
