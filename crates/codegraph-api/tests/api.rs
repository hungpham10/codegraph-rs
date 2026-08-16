use codegraph_api::{GraphApi, Pagination};
use codegraph_core::{
    CallRecord, EffectType, ScopeLevel, Symbol, SymbolKind, SymbolMatch, SYMBOL_BASE,
};
use codegraph_graph::{GraphIndex, ParseResult, SharedGraphIndex};
use std::collections::{HashMap, HashSet};
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

    // Substring search (resumable path, no deadline).
    let hits = api
        .search_symbol_paged_resumable(
            "call",
            None,
            SymbolMatch::Contains,
            Pagination { limit: 10, offset: 0 },
            None,
            0,
        )
        .await
        .unwrap()
        .page;
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
        strip_prefix: None,
    };
    let md = api.context_markdown(&req).await.unwrap();
    assert!(md.contains("caller"));
}

/// Seed N symbol tên "order_*" (mỗi tên 1 symbol) — đủ lớn để search chậm hơn
/// 1ms (debug build) và có tổng > limit (test phân trang).
async fn seed_many(db: &str, count: usize) {
    let mut idx = GraphIndex::open(db).await.unwrap();
    let mut results = Vec::new();
    for (id, i) in (SYMBOL_BASE..).zip(0..count) {
        let name = format!("order_{i:05}");
        results.push(ParseResult {
            path: "src/a.ts".into(),
            language: "typescript".into(),
            bytes: 10,
            lines: 4,
            symbols: vec![sym(id, &name)],
            chains: HashMap::new(),
            calls: vec![],
        });
    }
    idx.ingest(&results).await.unwrap();
}

/// Seed N symbol, mỗi symbol gắn annotation `@RestController` (chẵn) / `@Service`
/// (lẻ) — dùng test resumable cho `search_by_annotation`.
async fn seed_annotations(db: &str, count: usize) {
    let mut idx = GraphIndex::open(db).await.unwrap();
    let mut results = Vec::new();
    for (id, i) in (SYMBOL_BASE..).zip(0..count) {
        let mut s = sym(id, &format!("svc_{i}"));
        s.annotations = vec![codegraph_core::Annotation {
            name: (if i % 2 == 0 {
                "@RestController"
            } else {
                "@Service"
            })
            .into(),
            args: HashMap::new(),
            line: 1,
        }];
        results.push(ParseResult {
            path: "src/a.ts".into(),
            language: "typescript".into(),
            bytes: 10,
            lines: 4,
            symbols: vec![s],
            chains: HashMap::new(),
            calls: vec![],
        });
    }
    idx.ingest(&results).await.unwrap();
}

/// Seed N caller, mỗi caller gọi một library call lowercase `log.println` — dùng
/// test resumable cho `references` (call_name lowercase để khớp substring, do
/// engine filter `name.contains(&q)` với `q` đã lowercased).
async fn seed_references(db: &str, count: usize) {
    let mut idx = GraphIndex::open(db).await.unwrap();
    let mut results = Vec::new();
    for i in 0..count {
        let caller = SYMBOL_BASE + i as u64;
        results.push(ParseResult {
            path: "src/a.ts".into(),
            language: "typescript".into(),
            bytes: 10,
            lines: 4,
            symbols: vec![sym(caller, &format!("caller_{i}"))],
            chains: HashMap::new(),
            calls: vec![CallRecord {
                caller_id: caller,
                call_name: "log.println".to_string(),
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
        });
    }
    idx.ingest(&results).await.unwrap();
}

/// Resume roundtrip: timeout → lấy resume id → retry cùng args + resume → kết
/// quả đầy đủ, không lặp/không mất. Resume id sai → lỗi bảo retry không resume.
///
/// Dùng [`TIMEOUT_EXPIRE_IMMEDIATELY`] để deadline **đã hết hạn ngay** →
/// `timed_out` được đảm bảo trên mọi máy (không phụ thuộc tốc độ đồng hồ tường
/// như `timeout_ms = 1`, vốn có thể "chạy quá nhanh" và skip luồng resume).
#[tokio::test]
async fn search_resumable_timeout_retry_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let db_str = format!("sqlite://{}", db_path.to_string_lossy());
    seed_many(&db_str, 6000).await;
    let api = api(&db_str).await;

    // Call 1: deadline đã hết hạn ngay → chắc chắn timed_out, sinh resume id.
    // total = 5000 vì name engine chặn cứng MAX_RESULTS tên distinct.
    let capped = 5000;
    let first = api
        .search_symbol_paged_resumable(
            "order",
            None,
            SymbolMatch::Contains,
            Pagination { limit: 20, offset: 0 },
            None,
            codegraph_api::TIMEOUT_EXPIRE_IMMEDIATELY,
        )
        .await
        .unwrap();
    assert!(first.timed_out, "expired deadline must time out");
    let resume_id = first.resume.expect("timeout must carry a resume id");

    // Retry: cùng args + resume, không giới hạn thời gian → hoàn tất.
    let out = api
        .search_symbol_paged_resumable(
            "order",
            None,
            SymbolMatch::Contains,
            Pagination { limit: 20, offset: 0 },
            Some(resume_id.clone()),
            0,
        )
        .await
        .unwrap();
    assert!(!out.timed_out);
    assert_eq!(out.total, capped, "total must match the full scan (capped)");
    let ids: std::collections::HashSet<u64> = out.page.iter().map(|s| s.id).collect();
    assert_eq!(
        ids.len(),
        out.page.len(),
        "no duplicate results after resume"
    );
    assert_eq!(out.page.len(), 20);

    // Resume id không tồn tại → lỗi (LLM nên retry không resume).
    assert!(
        api.search_symbol_paged_resumable(
            "order",
            None,
            SymbolMatch::Contains,
            Pagination { limit: 20, offset: 0 },
            Some("deadbeef00000000".into()),
            0,
        )
        .await
        .is_err(),
        "unknown resume id must be rejected"
    );
    // Resume id không khớp query → lỗi.
    assert!(
        api.search_symbol_paged_resumable(
            "totally_different",
            None,
            SymbolMatch::Contains,
            Pagination { limit: 20, offset: 0 },
            Some(resume_id),
            0,
        )
        .await
        .is_err(),
        "resume id for a different query must be rejected"
    );
}

/// Resumable timeout→retry cho `search_by_annotation` — deterministic qua
/// `TIMEOUT_EXPIRE_IMMEDIATELY` (deadline đã hết hạn ngay lập tức trên mọi máy).
#[tokio::test]
async fn annotation_search_resumable_timeout_retry() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let db_str = format!("sqlite://{}", db_path.to_string_lossy());
    seed_annotations(&db_str, 200).await;
    let api = api(&db_str).await;

    // Lần 1: deadline hết hạn ngay → chắc chắn timed_out + mang resume id.
    let first = api
        .search_by_annotation_resumable(
            "@RestController",
            None,
            Pagination {
                limit: 20,
                offset: 0,
            },
            None,
            codegraph_api::TIMEOUT_EXPIRE_IMMEDIATELY,
        )
        .await
        .unwrap();
    assert!(first.timed_out, "expired deadline must time out");
    let resume_id = first.resume.expect("timeout must carry a resume id");

    // Lần 2: retry cùng args + resume id, timeout_ms=0 → hoàn tất.
    let out = api
        .search_by_annotation_resumable(
            "@RestController",
            None,
            Pagination {
                limit: 20,
                offset: 0,
            },
            Some(resume_id.clone()),
            0,
        )
        .await
        .unwrap();
    assert!(!out.timed_out);
    // 200 symbol, i % 2 == 0 → 100 gắn @RestController.
    assert_eq!(out.total, 100, "total must match full scan");
    assert_eq!(out.page.len(), 20);
    assert!(out.resume.is_none());

    // Resume id sai → lỗi.
    assert!(
        api.search_by_annotation_resumable(
            "@RestController",
            None,
            Pagination {
                limit: 20,
                offset: 0
            },
            Some("deadbeef00000000".into()),
            0,
        )
        .await
        .is_err(),
        "unknown resume id must be rejected"
    );
    // Resume id của query khác → lỗi.
    assert!(
        api.search_by_annotation_resumable(
            "@Service",
            None,
            Pagination {
                limit: 20,
                offset: 0
            },
            Some(resume_id),
            0,
        )
        .await
        .is_err(),
        "resume id for a different query must be rejected"
    );
}

/// Resumable timeout→retry cho `references` (deterministic).
#[tokio::test]
async fn references_resumable_timeout_retry() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let db_str = format!("sqlite://{}", db_path.to_string_lossy());
    seed_references(&db_str, 50).await; // 50 caller, mỗi gọi "log.println"
    let api = api(&db_str).await;

    let first = api
        .references_resumable(
            "log.println",
            Pagination {
                limit: 20,
                offset: 0,
            },
            None,
            codegraph_api::TIMEOUT_EXPIRE_IMMEDIATELY,
        )
        .await
        .unwrap();
    assert!(first.timed_out, "expired deadline must time out");
    let resume_id = first.resume.expect("timeout must carry a resume id");

    let out = api
        .references_resumable(
            "log.println",
            Pagination {
                limit: 20,
                offset: 0,
            },
            Some(resume_id.clone()),
            0,
        )
        .await
        .unwrap();
    assert!(!out.timed_out);
    assert_eq!(out.page.len(), 20);
    let ids: HashSet<u64> = out.page.iter().map(|c| c.func_id).collect();
    assert_eq!(ids.len(), 20, "no duplicate callers across the page");
    assert!(out.resume.is_none());

    // Resume id sai → lỗi.
    assert!(
        api.references_resumable(
            "log.println",
            Pagination {
                limit: 20,
                offset: 0
            },
            Some("deadbeef00000000".into()),
            0,
        )
        .await
        .is_err(),
        "unknown resume id must be rejected"
    );
    // Resume id của query khác → lỗi.
    assert!(
        api.references_resumable(
            "other.call",
            Pagination {
                limit: 20,
                offset: 0
            },
            Some(resume_id),
            0,
        )
        .await
        .is_err(),
        "resume id for a different query must be rejected"
    );
}

/// Resumable timeout→retry cho `search_flow_pattern` (deterministic).
#[tokio::test]
async fn flow_search_resumable_timeout_retry() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let db_str = format!("sqlite://{}", db_path.to_string_lossy());
    let (caller, callee, _helper) = seed_index(&db_str).await;
    let api = api(&db_str).await;

    // Chain chứa callee → 2 hit (caller→[caller,callee], callee→[callee,helper]).
    let pattern = callee.to_string();

    let first = api
        .search_flow_pattern_resumable(
            &pattern,
            Pagination {
                limit: 20,
                offset: 0,
            },
            None,
            codegraph_api::TIMEOUT_EXPIRE_IMMEDIATELY,
        )
        .await
        .unwrap();
    assert!(first.timed_out, "expired deadline must time out");
    let resume_id = first.resume.expect("timeout must carry a resume id");

    let out = api
        .search_flow_pattern_resumable(
            &pattern,
            Pagination {
                limit: 20,
                offset: 0,
            },
            Some(resume_id.clone()),
            0,
        )
        .await
        .unwrap();
    assert!(!out.timed_out);
    assert_eq!(
        out.page.len(),
        2,
        "two functions have chain containing callee"
    );
    assert!(out.resume.is_none());

    // Resume id của pattern khác → lỗi.
    assert!(
        api.search_flow_pattern_resumable(
            &caller.to_string(),
            Pagination {
                limit: 20,
                offset: 0
            },
            Some(resume_id),
            0,
        )
        .await
        .is_err(),
        "resume id for a different pattern must be rejected"
    );
}

/// Phân trang qua resume (Paged cursor): call 1 limit=10 (timeout_ms=0) hoàn
/// tất + còn page sau → resume id; call 2 cùng resume + offset=10 → page rời,
/// tổng nhất quán.
#[tokio::test]
async fn search_symbol_paged_resume_paging() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("db.sqlite");
    let db_str = format!("sqlite://{}", db_path.to_string_lossy());
    seed_many(&db_str, 1500).await;
    let api = api(&db_str).await;

    let first = api
        .search_symbol_paged_resumable(
            "order",
            None,
            SymbolMatch::Contains,
            Pagination {
                limit: 10,
                offset: 0,
            },
            None,
            0,
        )
        .await
        .unwrap();
    assert!(!first.timed_out);
    assert_eq!(first.total, 1500);
    assert_eq!(first.page.len(), 10);
    assert!(
        first.resume.is_some(),
        "more pages remain -> response must carry a resume id"
    );
    let resume_id = first.resume.unwrap();

    // Trang 2 qua resume (Paged cursor — không quét lại).
    let second = api
        .search_symbol_paged_resumable(
            "order",
            None,
            SymbolMatch::Contains,
            Pagination {
                limit: 10,
                offset: 10,
            },
            Some(resume_id.clone()),
            0,
        )
        .await
        .unwrap();
    assert!(!second.timed_out);
    assert_eq!(second.total, 1500, "total must be stable across pages");
    let page1: std::collections::HashSet<u64> = first.page.iter().map(|s| s.id).collect();
    let page2: std::collections::HashSet<u64> = second.page.iter().map(|s| s.id).collect();
    assert!(page1.is_disjoint(&page2), "pages must be disjoint");
    assert_eq!(second.page.len(), 10);

    // Page cuối rời.
    let last = api
        .search_symbol_paged_resumable(
            "order",
            None,
            SymbolMatch::Contains,
            Pagination {
                limit: 10,
                offset: 1490,
            },
            Some(resume_id.clone()),
            0,
        )
        .await
        .unwrap();
    assert_eq!(last.page.len(), 10);
    assert!(last.resume.is_none(), "last page: no more resume");

    // Resume id này thuộc query "order" — dùng cho query khác → lỗi.
    assert!(api
        .search_symbol_paged_resumable(
            "zzz",
            None,
            SymbolMatch::Contains,
            Pagination {
                limit: 10,
                offset: 0
            },
            Some(resume_id),
            0
        )
        .await
        .is_err());
}
