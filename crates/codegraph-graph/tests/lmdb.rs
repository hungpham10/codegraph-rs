//! Integration tests cho backend LMDB (feature `lmdb`) — port subset của
//! `tests/sqlite.rs`. Khác sqlite (path = file), LMDB dùng path = thư mục.
//!
//! `SharedGraphIndex` routing theo scheme trong DSN (`lmdb://...`) — nên bộ
//! test này chạy được dù có bật sqlite hay không.

#![cfg(feature = "lmdb")]

use codegraph_core::{CallRecord, EffectType, SYMBOL_BASE, Symbol, SymbolKind};
use codegraph_graph::GraphIndex;
use codegraph_graph::ParseResult;
use codegraph_graph::SharedGraphIndex;
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

/// Ingest → reopen: entity + query surface sống lại từ file LMDB.
#[tokio::test]
async fn index_ingest_reopen_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = format!("lmdb://{}/db.lmdb", dir.path().to_string_lossy());

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

    let sf = idx.search_flow(&[SYMBOL_BASE + 1]).await.unwrap();
    assert_eq!(sf.len(), 1);
    assert_eq!(sf[0].function_name, "a");
}

/// Ingest rỗng = full wipe; version vẫn bump; wipe giữ trên đĩa sau reopen.
#[tokio::test]
async fn empty_ingest_wipes_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = format!("lmdb://{}/db.lmdb", dir.path().to_string_lossy());

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

    let idx = GraphIndex::open(&path).await.unwrap();
    assert_eq!(idx.stats().symbols, 0);
    assert_eq!(idx.version(), 2);
}

/// SharedGraphIndex phát hiện stale qua version bump (dùng `LmdbStorage::probe_version`).
/// DSN có scheme `lmdb://` → shared mở đúng backend LMDB dù sqlite cũng bật.
#[tokio::test]
async fn shared_index_rebuilds_on_reindex() {
    let dir = tempfile::tempdir().unwrap();
    let db_dir = dir.path().join("db.lmdb");
    let db_str = format!("lmdb://{}", db_dir.to_string_lossy());

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

    let sgi = Arc::new(SharedGraphIndex::open(Some(db_str.clone())).await.unwrap());
    let idx = sgi.ensure_fresh().await;
    assert_eq!(idx.version(), 1);
    assert_eq!(idx.symbol_by_id(SYMBOL_BASE).unwrap().name, "a");

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

/// 2 hàm cùng tên khác file → id global riêng, chain giữ nguyên, search trả đủ.
#[tokio::test]
async fn ingest_same_function_name_across_files_stays_distinct() {
    let dir = tempfile::tempdir().unwrap();
    let db_dir = dir.path().join("db.lmdb");
    let db_str = format!("lmdb://{}", db_dir.to_string_lossy());

    let r_store = result(
        "store/store.go",
        vec![sym("store/store.go", "process", SYMBOL_BASE)],
        HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE])]),
        vec![],
    );
    let r_cache = result(
        "cache/cache.go",
        vec![sym("cache/cache.go", "process", SYMBOL_BASE)],
        HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE])]),
        vec![],
    );

    let mut idx = GraphIndex::open(&db_str).await.unwrap();
    idx.ingest(&[r_store, r_cache]).await.unwrap();

    assert_eq!(idx.stats().symbols, 2);
    let s1 = idx.symbol_by_id(SYMBOL_BASE).unwrap();
    let s2 = idx.symbol_by_id(SYMBOL_BASE + 1).unwrap();
    assert_eq!(s1.name, "process");
    assert_eq!(s2.name, "process");
    assert_eq!(s1.file, "store/store.go");
    assert_eq!(s2.file, "cache/cache.go");

    assert_eq!(idx.flow(SYMBOL_BASE).await.unwrap().chain_desc, vec!["process"]);
    assert_eq!(
        idx.flow(SYMBOL_BASE + 1).await.unwrap().chain_desc,
        vec!["process"]
    );

    let hits = idx
        .search_symbol("process", Some(SymbolKind::Function), 10)
        .await
        .unwrap();
    assert_eq!(hits.len(), 2);
    let mut files: Vec<&str> = hits.iter().map(|s| s.file.as_str()).collect();
    files.sort_unstable();
    assert_eq!(files, vec!["cache/cache.go", "store/store.go"]);
}

/// Regression: LMDB giới hạn key ~511 byte (MDB_BAD_VALSIZE). Path file và
/// call-name vượt giới hạn phải vẫn ingest/reopen đúng (key bound + hash,
/// tên/phí giữ nguyên trong value).
#[tokio::test]
async fn long_path_and_call_name_survive_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let db_str = format!("lmdb://{}/db.lmdb", dir.path().to_string_lossy());

    // path > 511 byte.
    let long_seg = "d".repeat(280); // 280
    let long_path = ["src", &long_seg, &long_seg, "mod.ts"].join("/");
    assert!(long_path.len() > 511, "long_path len = {}", long_path.len());

    // call_name > 511 byte (mangled symbol).
    let long_call = format!("RTX{}MangledType0::method{}X", "t".repeat(300), "q".repeat(300));
    assert!(long_call.len() > 511);

    let calls = vec![CallRecord {
        caller_id: SYMBOL_BASE,
        call_name: long_call.clone(),
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
        &long_path,
        vec![sym(&long_path, "a", SYMBOL_BASE), sym(&long_path, "b", SYMBOL_BASE + 1)],
        HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE, SYMBOL_BASE + 1])]),
        calls,
    );

    {
        let mut idx = GraphIndex::open(&db_str).await.unwrap();
        idx.ingest(&[r]).await.unwrap();
        assert_eq!(idx.version(), 1);
    }

    let idx = GraphIndex::open(&db_str).await.unwrap();
    assert_eq!(idx.version(), 1);
    assert_eq!(idx.files().len(), 1);
    assert_eq!(idx.files()[0].path, long_path);
    assert_eq!(
        idx.callees(SYMBOL_BASE).await.unwrap()[0].name,
        "b"
    );
    // call-name index giữ nguyên tên dài sau reopen.
    let hits = idx.search_flow(&[SYMBOL_BASE]).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].function_name, "a");
}