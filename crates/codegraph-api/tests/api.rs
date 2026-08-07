use codegraph_api::GraphApi;
use codegraph_core::{CallRecord, EffectType, ScopeLevel, Symbol, SymbolKind, SYMBOL_BASE};
use codegraph_graph::{GraphIndex, ParseResult, SharedGraphIndex};
use std::collections::HashMap;
use std::sync::Arc;

fn sym(id: u64, name: &str) -> Symbol {
    Symbol {
        id,
        name: name.to_string(),
        kind: SymbolKind::Function,
        scope: ScopeLevel::Global,
        scope_id: 0,
        type_ref: 0,
        type_name: None,
        file: "src/a.ts".into(),
        line: 1,
        end_line: 2,
        signature: None,
        doc: None,
        annotations: Vec::new(),
        language: "typescript".into(),
    }
}

/// Seed index sqlite: caller → callee; callee → helper (via placeholder call).
/// Kèm 1 CallRecord (`fmt.Println` ở vị trí 1 của callee) để test call-name index.
async fn seed_index(path: &str) -> (u64, u64, u64) {
    let mut idx = GraphIndex::open(path).await.unwrap();
    let caller = SYMBOL_BASE;
    let callee = SYMBOL_BASE + 1;
    let helper = SYMBOL_BASE + 2;
    let r = ParseResult {
        path: "src/a.ts".into(),
        language: "typescript".into(),
        bytes: 10,
        lines: 4,
        symbols: vec![
            sym(caller, "caller"),
            sym(callee, "callee"),
            sym(helper, "helper"),
        ],
        chains: HashMap::from([
            (caller, vec![caller, callee]),
            (callee, vec![callee, helper]),
        ]),
        calls: vec![CallRecord {
            caller_id: callee,
            call_name: "fmt.Println".to_string(),
            position: 1,
            arg_exprs: vec!["msg".into()],
            line: 3,
            condition: None,
            is_loop_body: false,
            effect: EffectType::Log,
            effect_desc: None,
            target_class: None,
            target_method: None,
        }],
    };
    idx.ingest(&[r]).await.unwrap();
    (caller, callee, helper)
}

async fn api(path: &str) -> GraphApi {
    let index = Arc::new(SharedGraphIndex::open(Some(path.into())).await.unwrap());
    GraphApi::new_with_index(index)
}

#[tokio::test]
async fn search_and_symbol_by_id() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let db_str = format!("sqlite://{}", db_path.to_string_lossy());
    let (caller, _, _) = seed_index(&db_str).await;
    let api = api(&db_str).await;

    // Substring search.
    let hits = api.search("call", 10).await.unwrap();
    assert!(hits.iter().any(|s| s.id == caller));
    // Symbol by id.
    assert_eq!(api.symbol_by_id(caller).await.unwrap().name, "caller");
    assert!(api.symbol_by_id(9999).await.is_none());
}

#[tokio::test]
async fn callers_callees_and_flow() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let db_str = format!("sqlite://{}", db_path.to_string_lossy());
    let (caller, callee, helper) = seed_index(&db_str).await;
    let api = api(&db_str).await;

    // callees của caller = [callee]; của callee = [helper].
    let cees = api.callees(caller).await.unwrap();
    assert_eq!(cees.len(), 1);
    assert_eq!(cees[0].id, callee);
    // callers transitive: caller gọi callee → callers(callee) = [caller];
    // callers(helper, depth 2) = [caller, callee].
    let c1 = api.callers(helper, 1).await.unwrap();
    assert_eq!(c1.len(), 1);
    assert_eq!(c1[0].id, callee);
    let c2 = api.callers(helper, 2).await.unwrap();
    assert_eq!(c2.len(), 2);

    // Flow render.
    let flow = api.flow(caller).await.unwrap();
    assert_eq!(flow.symbol.name, "caller");
    assert_eq!(flow.chain_desc, vec!["caller", "callee"]);

    // Impact = callers transitive.
    let impact = api.impact(helper, 2).await.unwrap();
    assert_eq!(impact.len(), 2);
}

#[tokio::test]
async fn search_flow_pattern_and_references() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let db_str = format!("sqlite://{}", db_path.to_string_lossy());
    let (caller, callee, _) = seed_index(&db_str).await;
    let api = api(&db_str).await;

    // Pattern theo tên symbol.
    let sf = api
        .search_flow_pattern(&format!("{caller}, {callee}"))
        .await
        .unwrap();
    assert_eq!(sf.len(), 1);
    assert_eq!(sf[0].function_name, "caller");

    // Pattern theo tên — resolve exact.
    let sf2 = api.search_flow_pattern("caller, callee").await.unwrap();
    assert_eq!(sf2.len(), 1);

    // Pattern sai → lỗi Invalid.
    assert!(api.search_flow_pattern("nope_symbol").await.is_err());

    // references theo call name (callers_by_call_name) — "fmt.Println".
    let refs = api.references("fmt", 10).await.unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].func_name, "callee");
    assert_eq!(refs[0].call_sites[0].call_name, "fmt.Println");
}

#[tokio::test]
async fn files_stats_and_context() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let db_str = format!("sqlite://{}", db_path.to_string_lossy());
    seed_index(&db_str).await;
    let api = api(&db_str).await;

    let files = api.files("").await;
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "src/a.ts");
    assert!(api.files("zzz/").await.is_empty());

    let stats = api.stats().await;
    assert_eq!(stats.symbols, 3);
    assert_eq!(stats.chains, 2);
    assert_eq!(stats.edges, 2);

    let req = codegraph_context::ContextRequest {
        query: "caller".into(),
        depth: 1,
        include_source: false,
        limit: 5,
        format: codegraph_context::Format::Markdown,
    };
    let md = api.context_markdown(&req).await.unwrap();
    assert!(md.contains("caller"));
}
