//! End-to-end golden trace: two in-group functions (`prepare_order` →
//! `check_stock` real compiled call) with an `if` between them, all external
//! callees mocked by Rhai. Asserts the exact observed-behavior sequence.

use codegraph_core::{
    CallRecord, EffectType, Error, ScopeLevel, Symbol, SymbolKind, MARKER_BRANCH_END,
    MARKER_IF_FALSE, MARKER_IF_TRUE, MARKER_SWITCH_CASE, MARKER_SWITCH_END, SYMBOL_BASE,
};
use codegraph_graph::GraphIndex;
use codegraph_sboxes::{compile, compile_with_mocks, BranchPolicy, SboxConfig};
use std::collections::HashMap;

fn sym(id: u64, name: &str) -> Symbol {
    Symbol {
        id,
        name: name.to_string(),
        kind: SymbolKind::Function,
        scope: ScopeLevel::Global,
        scope_id: 0,
        type_ref: 0,
        type_name: None,
        file: "order.ts".to_string(),
        line: 1,
        end_line: 1,
        signature: None,
        doc: None,
        annotations: Vec::new(),
        language: "test".to_string(),
    }
}

fn rec(caller_id: u64, pos: usize, name: &str, args: usize) -> CallRecord {
    CallRecord {
        caller_id,
        call_name: name.to_string(),
        position: pos,
        arg_exprs: (0..args).map(|i| format!("a{i}")).collect(),
        line: 1,
        condition: None,
        is_loop_body: false,
        effect: EffectType::None,
        effect_desc: None,
        target_class: None,
        target_method: None,
    }
}

fn result(
    path: &str,
    symbols: Vec<Symbol>,
    chains: HashMap<u64, Vec<u64>>,
    calls: Vec<CallRecord>,
) -> codegraph_graph::ParseResult {
    codegraph_graph::ParseResult {
        path: path.to_string(),
        language: "test".to_string(),
        bytes: 0,
        lines: 0,
        symbols,
        chains,
        calls,
    }
}

fn test_config() -> SboxConfig {
    SboxConfig {
        root: ".".into(),
        mock_dirs: vec!["tests/mocks".to_string()],
        loop_cap: 5,
        branch_policy: BranchPolicy::IfTrue,
        effect_rules: Vec::new(),
    }
}

/// `prepare_order`:
///   check_stock() → if in-stock { send_email() } → insert_order()
///
/// `check_stock`: returns the `get_stock` mock result via a switch (first case).
///
/// Golden observed behavior (IfTrue, first case taken):
///   check_stock[group] → get_stock (switch case 1) → send_email (if taken)
///   → insert_order
#[tokio::test]
async fn prepare_order_golden_trace() {
    const PREPARE: u64 = SYMBOL_BASE;
    const CHECK: u64 = SYMBOL_BASE + 1;

    let chains = HashMap::from([
        (
            PREPARE,
            vec![
                PREPARE,           // 0 self
                CHECK,             // 1 group call → real compiled call
                MARKER_IF_TRUE,    // 2
                0,                 // 3 send_email (mock)
                MARKER_BRANCH_END, // 4
                0,                 // 5 insert_order (mock)
            ],
        ),
        (
            CHECK,
            vec![
                CHECK,              // 0 self
                MARKER_SWITCH_CASE, // 1
                0,                  // 2 get_stock (mock, case 1)
                MARKER_SWITCH_END,  // 3
                MARKER_SWITCH_CASE, // 4
                0,                  // 5 get_stock (mock, case 2)
                MARKER_SWITCH_END,  // 6
            ],
        ),
    ]);
    let calls = vec![
        rec(PREPARE, 1, "check_stock", 0),
        rec(PREPARE, 3, "send_email", 1),
        rec(PREPARE, 5, "insert_order", 1),
        rec(CHECK, 2, "get_stock", 0),
        rec(CHECK, 5, "get_stock", 0),
    ];
    let r = result(
        "order.ts",
        vec![sym(PREPARE, "prepare_order"), sym(CHECK, "check_stock")],
        chains,
        calls,
    );
    let mut idx = GraphIndex::in_memory();
    idx.ingest(&[r]).await.unwrap();

    let mut module = compile(&idx, &[PREPARE, CHECK], &test_config())
        .await
        .unwrap();
    let (result, trace) = module.run(&[]);

    // `insert_order` mock returns 42 and is the last call of `prepare_order`.
    assert_eq!(result, 42);
    assert_eq!(
        trace.mock_names(),
        vec!["get_stock", "send_email", "insert_order"]
    );
    assert_eq!(trace.count("get_stock"), 1); // only the first switch case ran
    assert_eq!(trace.count("send_email"), 1);
    assert_eq!(trace.count("insert_order"), 1);

    // Sequence: first case's cond → its body → second case's cond (skipped),
    // then prepare's if cond (taken) → email → insert.
    let seq = trace.sequence();
    assert_eq!(
        seq,
        vec![
            "switch:1".to_string(),
            "call:get_stock".to_string(),
            "switch:0".to_string(),
            "if:1".to_string(),
            "call:send_email".to_string(),
            "call:insert_order".to_string(),
        ]
    );
}

/// With `IfFalse`, `prepare_order` skips `send_email` but still inserts.
#[tokio::test]
async fn prepare_order_if_false_skips_email() {
    const PREPARE: u64 = SYMBOL_BASE;

    let chains = HashMap::from([(
        PREPARE,
        vec![
            PREPARE,
            MARKER_IF_TRUE,
            0, // send_email
            MARKER_IF_FALSE,
            0, // log_skip (mock, else branch)
            MARKER_BRANCH_END,
            0, // insert_order
        ],
    )]);
    let calls = vec![
        rec(PREPARE, 2, "send_email", 0),
        rec(PREPARE, 4, "log_skip", 0),
        rec(PREPARE, 6, "insert_order", 0),
    ];
    let r = result(
        "order.ts",
        vec![sym(PREPARE, "prepare_order")],
        chains,
        calls,
    );
    let mut idx = GraphIndex::in_memory();
    idx.ingest(&[r]).await.unwrap();

    let cfg = SboxConfig {
        branch_policy: BranchPolicy::IfFalse,
        ..test_config()
    };
    let mut module = compile(&idx, &[PREPARE], &cfg).await.unwrap();
    let (_, trace) = module.run(&[]);

    assert_eq!(trace.count("send_email"), 0);
    assert_eq!(trace.count("log_skip"), 1);
    assert_eq!(trace.count("insert_order"), 1);
}

/// Link-time missing-mock detection: a callee that will be mock-dispatched but
/// has no mock (file or inline) fails the compile with the exact list, instead
/// of silently running a `0` fallback.
#[tokio::test]
async fn link_fails_on_unmocked_callees() {
    const RUN: u64 = SYMBOL_BASE;

    // run_order: submit(...) → compute_sku(...)
    let chains = HashMap::from([(
        RUN,
        vec![
            RUN, // 0 self
            0,   // 1 submit (no mock anywhere)
            0,   // 2 compute_sku (inline mock only)
        ],
    )]);
    let calls = vec![rec(RUN, 1, "submit", 1), rec(RUN, 2, "compute_sku", 2)];
    let r = result("order.ts", vec![sym(RUN, "run_order")], chains, calls);
    let mut idx = GraphIndex::in_memory();
    idx.ingest(&[r]).await.unwrap();

    // `compute_sku` is covered inline; `submit` is not → link error listing it.
    let mocks = vec![("compute_sku".to_string(), "77".to_string())];
    let res = compile_with_mocks(&idx, &[RUN], &test_config(), &mocks).await;
    assert!(matches!(
        res,
        Err(Error::MissingMocks(m)) if m == vec!["submit".to_string()]
    ));
}

/// `compile_with_mocks` satisfies link-time validation: per-call inline mocks
/// cover the callees missing from the file mock dir, the run dispatches to them,
/// and nothing lands in `trace.missing`.
#[tokio::test]
async fn inline_mocks_satisfy_link_and_run() {
    const RUN: u64 = SYMBOL_BASE;

    // run_order: submit(...) → compute_sku(...)
    let chains = HashMap::from([(
        RUN,
        vec![
            RUN, // 0 self
            0,   // 1 submit (inline mock)
            0,   // 2 compute_sku (inline mock)
        ],
    )]);
    let calls = vec![rec(RUN, 1, "submit", 1), rec(RUN, 2, "compute_sku", 2)];
    let r = result("order.ts", vec![sym(RUN, "run_order")], chains, calls);
    let mut idx = GraphIndex::in_memory();
    idx.ingest(&[r]).await.unwrap();

    // Neither callee is in tests/mocks/order.rhai — only the inline mocks cover
    // them, so link-time validation is satisfied by the inline set alone.
    let mocks = vec![
        ("submit".to_string(), "5".to_string()),
        ("compute_sku".to_string(), "77".to_string()),
    ];
    let mut module = compile_with_mocks(&idx, &[RUN], &test_config(), &mocks)
        .await
        .unwrap();
    let (result, trace) = module.run(&[40, 37]);

    // Inline mocks run (5 then 77); `compute_sku` is the last call → result.
    assert_eq!(trace.count("submit"), 1);
    assert_eq!(trace.count("compute_sku"), 1);
    assert_eq!(result, 77);
    assert!(trace.missing.is_empty());
}
