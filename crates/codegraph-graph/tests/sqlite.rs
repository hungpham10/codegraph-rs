//! Integration tests cho sqlite-backed index (feature `sqlite`).
//!
//! Old `db.rs`/`traversal.rs` test `Db` (drafts schema cũ) + `Traversal` — đã
//! xoá cùng db/. Thay bằng test của GraphIndex/SharedGraphIndex trên file
//! sqlite duy nhất: ingest (full re-index) → reopen → query, và phát hiện
//! stale qua version bump.

#![cfg(feature = "sqlite")]

use codegraph_core::{CallRecord, EffectType, Symbol, SymbolKind, SYMBOL_BASE};
use codegraph_graph::{GraphIndex, ParseResult, SharedGraphIndex};
use std::collections::HashMap;
use std::sync::Arc;

fn sym(file: &str, name: &str, id: u64) -> Symbol {
    Symbol {
        id,
        name: name.to_string(),
        kind: SymbolKind::Function,
        scope: codegraph_core::ScopeLevel::Global,
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

/// Ingest → reopen: mọi entity (symbols/chains/files/version) + query surface
/// sống lại từ file; edges tái dựng từ chains + call records.
#[tokio::test]
async fn index_ingest_reopen_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db.sqlite");
    let path = path.to_string_lossy().into_owned();

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
        let mut idx = GraphIndex::open(&path).await.unwrap();
        idx.ingest(&[r]).await.unwrap();
        assert_eq!(idx.version(), 1);
    }

    // Reopen — query lại được toàn bộ.
    let idx = GraphIndex::open(&path).await.unwrap();
    assert_eq!(idx.version(), 1);
    assert_eq!(idx.stats().symbols, 2);
    assert_eq!(idx.stats().chains, 1);
    assert_eq!(idx.stats().edges, 1);
    assert_eq!(idx.files().len(), 1);
    assert_eq!(idx.files()[0].path, "a.ts");

    let cees = idx.callees(SYMBOL_BASE).await.unwrap();
    assert_eq!(cees.len(), 1);
    assert_eq!(cees[0].name, "b");
    let cers = idx.callers(SYMBOL_BASE + 1, 1).await.unwrap();
    assert_eq!(cers.len(), 1);
    assert_eq!(cers[0].name, "a");

    let flow = idx.flow(SYMBOL_BASE).await.unwrap();
    assert_eq!(flow.chain_desc, vec!["a", "b"]);
    assert_eq!(flow.calls[0].line, 3);

    // search_flow qua chain engine persistent.
    let sf = idx
        .search_flow(&[SYMBOL_BASE + 1])
        .await
        .unwrap();
    assert_eq!(sf.len(), 1);
    assert_eq!(sf[0].function_name, "a");
}

/// Ingest rỗng = full wipe: entity cũ biến mất, version vẫn bump.
#[tokio::test]
async fn empty_ingest_wipes_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db.sqlite");
    let path = path.to_string_lossy().into_owned();

    let r = result(
        "a.ts",
        vec![sym("a.ts", "a", SYMBOL_BASE)],
        HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE])]),
        vec![],
    );
    let mut idx = GraphIndex::open(&path).await.unwrap();
    idx.ingest(&[r]).await.unwrap();
    assert_eq!(idx.stats().symbols, 1);

    idx.ingest(&[]).await.unwrap();
    assert_eq!(idx.version(), 2);
    assert_eq!(idx.stats().symbols, 0);
    assert!(idx.symbol_by_id(SYMBOL_BASE).is_none());

    // Reopen: vẫn rỗng (đã wipe trên đĩa).
    let idx = GraphIndex::open(&path).await.unwrap();
    assert_eq!(idx.stats().symbols, 0);
    assert_eq!(idx.version(), 2);
}

/// SharedGraphIndex phát hiện stale qua version bump của tiến trình index khác.
#[tokio::test]
async fn shared_index_rebuilds_on_reindex() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let db_str = db_path.to_string_lossy().into_owned();

    // "CLI": index dữ liệu đầu.
    {
        let mut idx = GraphIndex::open(&db_str).await.unwrap();
        let r = result(
            "a.ts",
            vec![sym("a.ts", "a", SYMBOL_BASE)],
            HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE])]),
            vec![],
        );
        idx.ingest(&[r]).await.unwrap();
    }

    // "Server": shared index trên cùng file.
    let sgi = Arc::new(SharedGraphIndex::open(Some(db_path.clone())).await.unwrap());
    let idx = sgi.ensure_fresh().await;
    assert_eq!(idx.version(), 1);
    assert_eq!(idx.symbol_by_id(SYMBOL_BASE).unwrap().name, "a");

    // Re-index với dữ liệu khác → version bump → ensure_fresh swap snapshot.
    {
        let mut idx = GraphIndex::open(&db_str).await.unwrap();
        let r = result(
            "b.ts",
            vec![sym("b.ts", "x", SYMBOL_BASE)],
            HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE])]),
            vec![],
        );
        idx.ingest(&[r]).await.unwrap();
    }
    let idx2 = sgi.ensure_fresh().await;
    assert_eq!(idx2.version(), 2);
    assert_eq!(idx2.stats().symbols, 1);
    assert_eq!(idx2.symbol_by_id(SYMBOL_BASE).unwrap().name, "x");
}
