//! Integration tests cho sqlite-backed index (feature `sqlite`).
//!
//! Old `db.rs`/`traversal.rs` test `Db` (drafts schema cũ) + `Traversal` — đã
//! xoá cùng db/. Thay bằng test của GraphIndex/SharedGraphIndex trên file
//! sqlite duy nhất: ingest (full re-index) → reopen → query, và phát hiện
//! stale qua version bump.

#![cfg(feature = "sqlite")]

use codegraph_core::{CallRecord, EffectType, SYMBOL_BASE, Symbol, SymbolKind, SymbolMatch};
use codegraph_graph::{GraphIndex, ParseResult, Pagination, SharedGraphIndex};
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

/// Như `sym` nhưng cho phép chỉ định kind (Method/Variable/...).
fn sym_kind(file: &str, name: &str, id: u64, kind: SymbolKind) -> Symbol {
    let mut s = sym(file, name, id);
    s.kind = kind;
    s
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
    let path = format!("sqlite://{}/db.sqlite", dir.path().to_string_lossy());

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
    let sf = idx.search_flow(&[SYMBOL_BASE + 1]).await.unwrap();
    assert_eq!(sf.len(), 1);
    assert_eq!(sf[0].function_name, "a");
}

/// Ingest rỗng = full wipe: entity cũ biến mất, version vẫn bump.
#[tokio::test]
async fn empty_ingest_wipes_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = format!("sqlite://{}/db.sqlite", dir.path().to_string_lossy());

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
    let db_str = format!("sqlite://{}", db_path.to_string_lossy());

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
    let sgi = Arc::new(SharedGraphIndex::open(Some(db_str.clone())).await.unwrap());
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

/// Go: 2 hàm cùng tên (`process`) ở 2 package khác nhau = 2 FILE riêng. Mỗi
/// file là một `ParseResult` với id local riêng (cùng `SYMBOL_BASE`) — `ingest`
/// remap sang id global riêng biệt, cả symbol lẫn chain giữ nguyên, không đè
/// nhau theo tên. Cũng khẳng định thứ tự global id: file đầu tiên chiếm
/// `SYMBOL_BASE`, file sau `SYMBOL_BASE + 1`.
#[tokio::test]
async fn ingest_same_function_name_across_files_stays_distinct() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let db_str = format!("sqlite://{}", db_path.to_string_lossy());

    // Hai package khác nhau (`store` và `cache`), mỗi package một hàm `process`.
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

    // Cả 2 symbol cùng tên nhưng id global khác nhau, giữ đúng file.
    assert_eq!(idx.stats().symbols, 2);
    let s1 = idx.symbol_by_id(SYMBOL_BASE).unwrap();
    let s2 = idx.symbol_by_id(SYMBOL_BASE + 1).unwrap();
    assert_eq!(s1.name, "process");
    assert_eq!(s2.name, "process");
    assert_eq!(s1.file, "store/store.go");
    assert_eq!(s2.file, "cache/cache.go");

    // Cả 2 đều giữ chain riêng → flow không bị "chain not found".
    assert_eq!(
        idx.flow(SYMBOL_BASE).await.unwrap().chain_desc,
        vec!["process"]
    );
    assert_eq!(
        idx.flow(SYMBOL_BASE + 1).await.unwrap().chain_desc,
        vec!["process"]
    );

    // Search tên trả đủ 2 kết quả (không hoà trộn thành 1).
    let hits = idx
        .search_symbol_paged_resumable(
            "process",
            Some(SymbolKind::Function),
            SymbolMatch::Contains,
            Pagination { limit: 10, offset: 0 },
            None,
            None,
        )
        .await
        .unwrap()
        .page;
    assert_eq!(hits.len(), 2);
    let mut files: Vec<&str> = hits.iter().map(|s| s.file.as_str()).collect();
    files.sort_unstable();
    assert_eq!(files, vec!["cache/cache.go", "store/store.go"]);
}

/// Bug D: sandbox lookup entry phải tìm được cả Java `Method`, không chỉ Rust/
/// Go free `Function`. `codegraph context getProfile` vốn dùng `kind=None` nên
/// resolve được — còn sandbox lọc `Some(SymbolKind::Function)` → "no function
/// matching". `search_symbol_kinds` phải trả về Method.
#[tokio::test]
async fn sandbox_search_kinds_finds_java_method() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let db_str = format!("sqlite://{}", db_path.to_string_lossy());

    let r = result(
        "UserController.java",
        vec![sym_kind(
            "UserController.java",
            "getProfile",
            SYMBOL_BASE,
            SymbolKind::Method,
        )],
        HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE])]),
        vec![],
    );
    let mut idx = GraphIndex::open(&db_str).await.unwrap();
    idx.ingest(&[r]).await.unwrap();

    // Trước fix: lọc Function-only → bỏ Method → empty (sandbox fail).
    let only_func = idx
        .search_symbol_paged_resumable(
            "getProfile",
            Some(SymbolKind::Function),
            SymbolMatch::Contains,
            Pagination { limit: 1, offset: 0 },
            None,
            None,
        )
        .await
        .unwrap()
        .page;
    assert!(only_func.is_empty());

    // Fix: sandbox chấp nhận Function | Method.
    let hits = idx
        .search_symbol_paged_resumable(
            "getProfile",
            None,
            SymbolMatch::Contains,
            Pagination { limit: 1, offset: 0 },
            None,
            None,
        )
        .await
        .unwrap()
        .page
        .into_iter()
        .find(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
        .into_iter()
        .collect::<Vec<_>>();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].name, "getProfile");
    assert_eq!(hits[0].kind, SymbolKind::Method);
}

/// Bug C: lời gọi method của receiver external không resolve được (`WrapResponse.
/// ok(...)`) KHÔNG được link nhầm vào local variable `boolean ok` trong file
/// khác (fallback tên ngắn từng trả bất kỳ symbol trùng tên, gồm Variable).
/// Chuỗi chỉ còn callee thật `selectDepartment`.
#[tokio::test]
async fn external_qualified_call_not_linked_to_local_variable() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let db_str = format!("sqlite://{}", db_path.to_string_lossy());

    let a = SYMBOL_BASE; // getProfile (caller)
    let b = SYMBOL_BASE + 1; // selectDepartment (callee thật)
    let v = SYMBOL_BASE + 2; // local `boolean ok` (Variable) — KHÔNG được link

    let calls = vec![CallRecord {
        caller_id: a,
        call_name: "WrapResponse.ok".to_string(),
        position: 1, // placeholder 0 trong chain
        arg_exprs: vec![],
        line: 2,
        condition: None,
        is_loop_body: false,
        effect: EffectType::None,
        effect_desc: None,
        target_class: None,
        target_method: None,
    }];
    let r = result(
        "UserController.java",
        vec![
            sym_kind("UserController.java", "getProfile", a, SymbolKind::Method),
            sym_kind(
                "UserController.java",
                "selectDepartment",
                b,
                SymbolKind::Method,
            ),
            sym_kind("HierarchyRefreshWorker.java", "ok", v, SymbolKind::Variable),
        ],
        HashMap::from([(a, vec![a, 0, b])]),
        calls,
    );
    let mut idx = GraphIndex::open(&db_str).await.unwrap();
    idx.ingest(&[r]).await.unwrap();

    let cees = idx.callees(a).await.unwrap();
    assert!(
        !cees.iter().any(|s| s.name == "ok"),
        "external `WrapResponse.ok` must not resolve to the local `ok` variable"
    );
    assert_eq!(cees.len(), 1, "chỉ còn callee thật của getProfile");
    assert_eq!(cees[0].name, "selectDepartment");
    assert_eq!(cees[0].file, "UserController.java");
}
