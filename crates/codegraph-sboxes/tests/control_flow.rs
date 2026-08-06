//! Control-flow lowering: build a small in-memory graph whose `flow()` chains
//! carry IF / LOOP / SWITCH / RETURN markers, compile the group with Cranelift,
//! and assert the *observed behavior* (mock call order + condition decisions).
//!
//! The fixture graph is hand-built as `ParseResult`s (same shapes the
//! codegraph-graph tests use), then ingested into `GraphIndex::in_memory()`.

use codegraph_core::{
    CallRecord, EffectType, ScopeLevel, Symbol, SymbolKind, SYMBOL_BASE, MARKER_BRANCH_END,
    MARKER_IF_FALSE, MARKER_IF_TRUE, MARKER_LOOP, MARKER_LOOP_BACK, MARKER_SWITCH_CASE,
    MARKER_SWITCH_END,
};
use codegraph_graph::GraphIndex;
use codegraph_sboxes::{BranchPolicy, CondKind, SboxConfig, compile};
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
        file: "test.ts".to_string(),
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

/// `compute`: calls in-group `helper`, then `if` → `notify`, then a capped
/// `loop` → `poll`, then `done`. `helper` calls the `seed` mock.
///
/// With the IfTrue policy the expected observed behavior is:
/// `seed` (via helper), `notify` (if taken), `poll` × loop_cap, `done`.
#[tokio::test]
async fn if_and_capped_loop() {
    const COMPUTE: u64 = SYMBOL_BASE;
    const HELPER: u64 = SYMBOL_BASE + 1;

    let chains = HashMap::from([
        (
            COMPUTE,
            vec![
                COMPUTE, // 0 self
                HELPER,  // 1 group call
                MARKER_IF_TRUE, // 2
                0,              // 3 notify (mock)
                MARKER_BRANCH_END, // 4
                MARKER_LOOP,       // 5
                0,                 // 6 poll (mock)
                MARKER_LOOP_BACK,  // 7
                0,                 // 8 done (mock)
            ],
        ),
        (
            HELPER,
            vec![
                HELPER, // 0 self
                0,      // 1 seed (mock)
            ],
        ),
    ]);
    let calls = vec![
        rec(COMPUTE, 1, "helper", 0),
        rec(COMPUTE, 3, "notify", 1),
        rec(COMPUTE, 6, "poll", 0),
        rec(COMPUTE, 8, "done", 1),
        rec(HELPER, 1, "seed", 0),
    ];
    let r = result(
        "test.ts",
        vec![sym(COMPUTE, "compute"), sym(HELPER, "helper")],
        chains,
        calls,
    );
    let mut idx = GraphIndex::in_memory();
    idx.ingest(&[r]).await.unwrap();

    let mut module = compile(&idx, &[COMPUTE, HELPER], &test_config())
        .await
        .unwrap();
    let (result, trace) = module.run(&[]);

    // `done` is the last expression of `compute`, so the entry returns its mock value.
    assert_eq!(result, 5);
    assert_eq!(trace.count("seed"), 1);
    assert_eq!(trace.count("notify"), 1);
    assert_eq!(trace.count("poll"), 5); // loop capped at 5 iterations
    assert_eq!(trace.count("done"), 1);
    assert_eq!(
        trace.mock_names(),
        vec!["seed", "notify", "poll", "poll", "poll", "poll", "poll", "done"]
    );

    // One if-condition decision (taken) + one loop-cap-exit evaluation extra.
    let ifs = trace.conds.iter().filter(|c| c.kind == CondKind::If).count();
    let loops = trace.conds.iter().filter(|c| c.kind == CondKind::Loop).count();
    assert_eq!(ifs, 1);
    assert_eq!(loops, 6); // 5 taken + the 6th that fails the cap and exits
}

/// Same graph, `IfFalse` policy: `notify` must NOT be called, but the loop still
/// runs (loops are capped by iteration count, not by the branch policy).
#[tokio::test]
async fn if_false_policy_skips_then_branch() {
    const COMPUTE: u64 = SYMBOL_BASE;

    let chains = HashMap::from([(
        COMPUTE,
        vec![
            COMPUTE,
            MARKER_IF_TRUE,
            0, // notify
            MARKER_BRANCH_END,
            MARKER_LOOP,
            0, // poll
            MARKER_LOOP_BACK,
            0, // done
        ],
    )]);
    let calls = vec![
        rec(COMPUTE, 2, "notify", 0),
        rec(COMPUTE, 5, "poll", 0),
        rec(COMPUTE, 7, "done", 0),
    ];
    let r = result(
        "test.ts",
        vec![sym(COMPUTE, "compute")],
        chains,
        calls,
    );
    let mut idx = GraphIndex::in_memory();
    idx.ingest(&[r]).await.unwrap();

    let cfg = SboxConfig {
        branch_policy: BranchPolicy::IfFalse,
        ..test_config()
    };
    let mut module = compile(&idx, &[COMPUTE], &cfg).await.unwrap();
    let (_, trace) = module.run(&[]);

    assert_eq!(trace.count("notify"), 0);
    assert_eq!(trace.count("poll"), 5);
    assert_eq!(trace.count("done"), 1);
    let ifs = trace.conds.iter().filter(|c| c.kind == CondKind::If).count();
    assert_eq!(ifs, 1);
}

/// Switch: first case taken (policy), `get_stock` called once even though two
/// `SWITCH_CASE … SWITCH_END` blocks exist in the chain.
#[tokio::test]
async fn switch_first_case_taken() {
    const COMPUTE: u64 = SYMBOL_BASE;

    let chains = HashMap::from([(
        COMPUTE,
        vec![
            COMPUTE,
            MARKER_SWITCH_CASE,
            0, // get_stock (case 1)
            MARKER_SWITCH_END,
            MARKER_SWITCH_CASE,
            0, // get_stock (case 2)
            MARKER_SWITCH_END,
            0, // done
        ],
    )]);
    let calls = vec![
        rec(COMPUTE, 2, "get_stock", 0),
        rec(COMPUTE, 5, "get_stock", 0),
        rec(COMPUTE, 7, "done", 0),
    ];
    let r = result(
        "test.ts",
        vec![sym(COMPUTE, "compute")],
        chains,
        calls,
    );
    let mut idx = GraphIndex::in_memory();
    idx.ingest(&[r]).await.unwrap();

    let mut module = compile(&idx, &[COMPUTE], &test_config())
        .await
        .unwrap();
    let (_, trace) = module.run(&[]);

    assert_eq!(trace.count("get_stock"), 1);
    assert_eq!(trace.count("done"), 1);
    // Case-1 condition true, case-2 condition false → 2 switch decisions.
    let switches = trace.conds.iter().filter(|c| c.kind == CondKind::Switch).count();
    assert_eq!(switches, 2);
}

/// `if … else`: both branches compile; the IfFalse policy takes the `else`
/// branch (IF_FALSE → then-body skipped, else-body mock runs).
#[tokio::test]
async fn if_else_takes_else_branch() {
    const COMPUTE: u64 = SYMBOL_BASE;

    let chains = HashMap::from([(
        COMPUTE,
        vec![
            COMPUTE,
            MARKER_IF_TRUE,
            0, // then_mock
            MARKER_IF_FALSE,
            0, // else_mock
            MARKER_BRANCH_END,
            0, // done
        ],
    )]);
    let calls = vec![
        rec(COMPUTE, 2, "then_mock", 0),
        rec(COMPUTE, 4, "else_mock", 0),
        rec(COMPUTE, 6, "done", 0),
    ];
    let r = result(
        "test.ts",
        vec![sym(COMPUTE, "compute")],
        chains,
        calls,
    );
    let mut idx = GraphIndex::in_memory();
    idx.ingest(&[r]).await.unwrap();

    // IfFalse → else branch taken.
    let cfg = SboxConfig {
        branch_policy: BranchPolicy::IfFalse,
        ..test_config()
    };
    let mut module = compile(&idx, &[COMPUTE], &cfg).await.unwrap();
    let (_, trace) = module.run(&[]);

    assert_eq!(trace.count("then_mock"), 0);
    assert_eq!(trace.count("else_mock"), 1);
    assert_eq!(trace.count("done"), 1);
}
