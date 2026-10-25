//! Unit tests cho full-flow interpreter (`run_flow_sim`): if/else không cắt
//! path, switch path-per-case, loop break/max_iter, call/return frame stack,
//! throw → catch (khớp exception type, xuyên nhiều tầng callee).

use codegraph_core::{
    CallRecord, EffectType, MARKER_BRANCH_END, MARKER_BREAK, MARKER_CALL_ENTER, MARKER_CALL_EXIT,
    MARKER_CATCH_START, MARKER_CONTINUE, MARKER_IF_FALSE, MARKER_IF_TRUE, MARKER_LOOP,
    MARKER_LOOP_BACK, MARKER_RETURN, MARKER_SWITCH_CASE, MARKER_SWITCH_CLOSE, MARKER_SWITCH_END,
    MARKER_SWITCH_START, MARKER_THROW, MARKER_TRY_END, MARKER_TRY_START, PathEnding, SYMBOL_BASE,
    ScopeLevel, Symbol, SymbolKind,
};
use codegraph_graph::{GraphIndex, ParseResult};
use std::collections::HashMap;

const A: u64 = SYMBOL_BASE;
const B: u64 = A + 1;
const C: u64 = A + 2;
const H: u64 = A + 3;
const D: u64 = A + 4;

fn sym(name: &str, id: u64) -> Symbol {
    Symbol {
        id,
        name: name.to_string(),
        kind: SymbolKind::Function,
        scope: ScopeLevel::Global,
        scope_id: 0,
        type_ref: 0,
        type_name: None,
        file: "t.py".to_string(),
        line: 1,
        end_line: 1,
        impl_trait: None,
        sig_key: None,
        signature: None,
        doc: None,
        annotations: Vec::new(),
        language: "python".to_string(),
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
        language: "python".to_string(),
        bytes: 0,
        lines: 0,
        symbols,
        chains,
        calls,
        fn_ptr_assigns: Vec::new(),
        file_deps: None,
    }
}

fn call(caller: u64, name: &str, position: usize) -> CallRecord {
    CallRecord {
        caller_id: caller,
        call_name: name.to_string(),
        position,
        arg_exprs: Vec::new(),
        line: 1,
        condition: None,
        is_loop_body: false,
        effect: EffectType::None,
        effect_desc: None,
        target_class: None,
        target_method: None,
        into_var: None,
        is_new_var: false,
        ..Default::default()
    }
}

/// Nhãn marker (branch label / exception type) — record rỗng call_name.
fn label(caller: u64, position: usize, text: &str) -> CallRecord {
    let mut r = call(caller, "", position);
    r.condition = Some(text.to_string());
    r
}

fn names(path: &codegraph_core::FullFlowPath) -> Vec<String> {
    path.steps.iter().map(|s| s.symbol_name.clone()).collect()
}

async fn build_idx(chains: HashMap<u64, Vec<u64>>, calls: Vec<CallRecord>) -> GraphIndex {
    let mut idx = GraphIndex::in_memory();
    // register() gán id tuần tự theo thứ tự symbols trong Vec — giữ đúng
    // thứ tự [a, b, c, handler] để id khớp với consts A/B/C/H.
    let symbols = vec![
        sym("a", A),
        sym("b", B),
        sym("c", C),
        sym("handler", H),
        sym("d", D),
    ];
    idx.ingest(&[result("t.py", symbols, chains, calls)], None)
        .await
        .unwrap();
    idx
}

#[tokio::test]
async fn if_else_paths_continue_after_block() {
    // Regression: true path không được cắt ở IF_FALSE/BRANCH_END — code sau
    // if/else (call d) phải xuất hiện trong MỌI path.
    let chains = HashMap::from([
        (
            A,
            vec![
                A,
                MARKER_IF_TRUE,
                B,
                MARKER_IF_FALSE,
                C,
                MARKER_BRANCH_END,
                D,
                MARKER_RETURN,
            ],
        ),
        (D, vec![D, MARKER_RETURN]),
        (B, vec![B, MARKER_RETURN]),
        (C, vec![C, MARKER_RETURN]),
    ]);
    let idx = build_idx(chains, vec![]).await;
    let paths = idx.trace_full_flow(A, 5, 50, 1).await.unwrap();
    assert!(paths.len() >= 2, "cần ≥2 path (true/false), got {paths:?}");
    eprintln!("DBG-TEST {paths:?}");
    for p in &paths {
        assert_eq!(p.ended_with, PathEnding::Returned);
        // Mọi path đều chạy code SAU if/else (gọi d) rồi return.
        assert!(
            names(p).contains(&"d".to_string()),
            "path bị cắt ở if/else: {:?}",
            names(p)
        );
        assert!(names(p).iter().any(|n| n == "b") || names(p).iter().any(|n| n == "c"));
    }
    let true_path = paths.iter().find(|p| names(p).contains(&"b".to_string()));
    let false_path = paths.iter().find(|p| names(p).contains(&"c".to_string()));
    assert!(true_path.is_some() && false_path.is_some());
}

#[tokio::test]
async fn switch_paths_per_case() {
    // switch 2 case: mỗi case một path, tất cả đều chạy code sau switch (return).
    let chains = HashMap::from([
        (
            A,
            vec![
                A,
                MARKER_SWITCH_START,
                MARKER_SWITCH_CASE,
                B,
                MARKER_SWITCH_END,
                MARKER_SWITCH_CASE,
                C,
                MARKER_SWITCH_END,
                MARKER_SWITCH_CLOSE,
                A,
                MARKER_RETURN,
            ],
        ),
        (B, vec![B, MARKER_RETURN]),
        (C, vec![C, MARKER_RETURN]),
    ]);
    let idx = build_idx(chains, vec![]).await;
    let paths = idx.trace_full_flow(A, 5, 50, 1).await.unwrap();
    assert!(paths.len() >= 2, "mỗi case một path, got {paths:?}");
    assert!(paths.iter().any(|p| names(p).contains(&"b".to_string())));
    assert!(paths.iter().any(|p| names(p).contains(&"c".to_string())));
    for p in &paths {
        assert_eq!(p.ended_with, PathEnding::Returned);
        assert_eq!(names(p).last().unwrap(), "a"); // return sau switch
    }
}

#[tokio::test]
async fn loop_break_exits_then_continues() {
    // for { b(); break; } → d(); return: break thoát loop rồi vẫn chạy code sau.
    let chains = HashMap::from([
        (
            A,
            vec![
                A,
                MARKER_LOOP,
                B,
                MARKER_BREAK,
                MARKER_LOOP_BACK,
                A,
                MARKER_RETURN,
            ],
        ),
        (B, vec![B, MARKER_RETURN]),
    ]);
    let idx = build_idx(chains, vec![]).await;
    let paths = idx.trace_full_flow(A, 5, 50, 1).await.unwrap();
    assert!(
        paths.iter().any(|p| names(p).contains(&"b".to_string())
            && names(p).last() == Some(&"a".to_string())
            && p.ended_with == PathEnding::Returned),
        "break path phải chạy tiếp code sau loop: {paths:?}"
    );
}

#[tokio::test]
async fn loop_max_iter_exits_and_continues() {
    // while { b(); } (không break) → path thoát qua max_iter rồi chạy code sau.
    let chains = HashMap::from([
        (
            A,
            vec![A, MARKER_LOOP, B, MARKER_LOOP_BACK, A, MARKER_RETURN],
        ),
        (B, vec![B, MARKER_RETURN]),
    ]);
    let idx = build_idx(chains, vec![]).await;
    let paths = idx.trace_full_flow(A, 5, 50, 1).await.unwrap();
    assert!(
        paths
            .iter()
            .any(|p| p.ended_with == PathEnding::Returned
                && names(p).last() == Some(&"a".to_string())),
        "path phải thoát loop qua max_iter rồi return: {paths:?}"
    );
}

#[tokio::test]
async fn call_descends_and_returns_with_stack_depths() {
    // a → b → c: trace b phải descend vào c, stack_depth tăng/giảm đúng.
    let chains = HashMap::from([
        (
            A,
            vec![A, MARKER_CALL_ENTER, B, MARKER_CALL_EXIT, A, MARKER_RETURN],
        ),
        (
            B,
            vec![B, MARKER_CALL_ENTER, C, MARKER_CALL_EXIT, B, MARKER_RETURN],
        ),
        (C, vec![C, MARKER_RETURN]),
    ]);
    let idx = build_idx(chains, vec![]).await;
    let paths = idx.trace_full_flow(B, 5, 50, 1).await.unwrap();
    assert!(
        paths.iter().any(|p| names(p).contains(&"c".to_string())),
        "phải descend vào c: {paths:?}"
    );
    let p = paths
        .iter()
        .find(|p| names(p).contains(&"c".to_string()))
        .unwrap();
    let depths: Vec<(String, usize, &str)> = p
        .steps
        .iter()
        .map(|s| (s.symbol_name.clone(), s.stack_depth, ""))
        .collect();
    // Call c ở depth 3 (base a=1, root b=2, c=3) — kiểm tra c được ghi depth 3.
    let c_call = p.steps.iter().find(|s| s.symbol_name == "c").unwrap();
    assert_eq!(c_call.action, codegraph_core::TraceAction::Call);
    assert_eq!(c_call.stack_depth, 3, "{depths:?}");
    // Return của c rồi return của b.
    let returns: Vec<&codegraph_core::FlowStep> =
        p.steps.iter().filter(|s| s.symbol_name == "c").collect();
    assert!(
        returns
            .iter()
            .any(|s| s.action == codegraph_core::TraceAction::Return)
    );
}

#[tokio::test]
async fn throw_unwinds_to_matching_catch() {
    // python: def a(): try: b() except ValueError: handler(); return
    // b raise ValueError → unwind về catch của a, chạy handler, return bình thường.
    let chains = HashMap::from([
        (
            A,
            vec![
                A,
                MARKER_TRY_START,
                MARKER_CALL_ENTER,
                B,
                MARKER_CALL_EXIT,
                MARKER_TRY_END,
                MARKER_CATCH_START,
                MARKER_CALL_ENTER,
                H,
                MARKER_CALL_EXIT,
                MARKER_BRANCH_END,
                A,
                MARKER_RETURN,
            ],
        ),
        (B, vec![B, MARKER_THROW]),
        (H, vec![H, MARKER_RETURN]),
    ]);
    let calls = vec![
        call(A, "b", 2),
        call(A, "handler", 7),
        label(B, 1, "throw ValueError"),
        label(A, 6, "catch ValueError"),
    ];
    let idx = build_idx(chains, calls).await;
    let paths = idx.trace_full_flow(B, 5, 50, 1).await.unwrap();
    assert!(
        paths.iter().any(|p| {
            p.ended_with == PathEnding::Returned
                && p.steps
                    .iter()
                    .any(|s| s.action == codegraph_core::TraceAction::Throw)
                && names(p).contains(&"handler".to_string())
        }),
        "throw phải unwind về catch rồi chạy handler: {paths:?}"
    );
}

#[tokio::test]
async fn throw_wrong_type_unwinds_through() {
    // raise TypeError nhưng handler chỉ catch ValueError → không catch, path Threw.
    let chains = HashMap::from([
        (
            A,
            vec![
                A,
                MARKER_TRY_START,
                MARKER_CALL_ENTER,
                B,
                MARKER_CALL_EXIT,
                MARKER_TRY_END,
                MARKER_CATCH_START,
                MARKER_CALL_ENTER,
                H,
                MARKER_CALL_EXIT,
                MARKER_BRANCH_END,
                A,
                MARKER_RETURN,
            ],
        ),
        (B, vec![B, MARKER_THROW]),
        (H, vec![H, MARKER_RETURN]),
    ]);
    let calls = vec![
        call(A, "b", 2),
        call(A, "handler", 7),
        label(B, 1, "throw TypeError"),
        label(A, 6, "catch ValueError"),
    ];
    let idx = build_idx(chains, calls).await;
    let paths = idx.trace_full_flow(B, 5, 50, 1).await.unwrap();
    assert!(
        paths.iter().any(|p| p.ended_with == PathEnding::Threw),
        "type không khớp phải unwind hết: {paths:?}"
    );
    assert!(
        !paths
            .iter()
            .any(|p| names(p).contains(&"handler".to_string())),
        "handler không được chạy khi type không khớp: {paths:?}"
    );
}

#[tokio::test]
async fn throw_crosses_two_frames_to_catch() {
    // a → b → c, c raise ValueError, try chỉ bọc quanh call b của a:
    // throw phải xuyên qua b về catch của a.
    let chains = HashMap::from([
        (
            A,
            vec![
                A,
                MARKER_TRY_START,
                MARKER_CALL_ENTER,
                B,
                MARKER_CALL_EXIT,
                MARKER_TRY_END,
                MARKER_CATCH_START,
                MARKER_CALL_ENTER,
                H,
                MARKER_CALL_EXIT,
                MARKER_BRANCH_END,
                A,
                MARKER_RETURN,
            ],
        ),
        (B, vec![B, MARKER_CALL_ENTER, C, MARKER_CALL_EXIT]),
        (C, vec![C, MARKER_THROW]),
    ]);
    let calls = vec![
        call(A, "b", 2),
        call(A, "handler", 7),
        call(B, "c", 1),
        label(C, 1, "throw ValueError"),
        label(A, 6, "catch ValueError"),
    ];
    let idx = build_idx(chains, calls).await;
    let paths = idx.trace_full_flow(C, 5, 50, 1).await.unwrap();
    assert!(
        paths.iter().any(|p| {
            p.ended_with == PathEnding::Returned && names(p).contains(&"handler".to_string())
        }),
        "throw xuyên 2 tầng phải về catch ở a: {paths:?}"
    );
}

#[tokio::test]
async fn no_double_return_after_leaf_call() {
    // call leaf ngay trước RETURN của hàm: chỉ MỘT step Return của a
    // (CALL_EXIT không ghi nữa khi phần tử kế là RETURN).
    let chains = HashMap::from([
        (
            A,
            vec![A, MARKER_CALL_ENTER, C, MARKER_CALL_EXIT, MARKER_RETURN],
        ),
        (C, vec![C, MARKER_RETURN]),
    ]);
    let calls = vec![call(A, "c", 2)];
    let idx = build_idx(chains, calls).await;
    let paths = idx.trace_full_flow(A, 5, 50, 1).await.unwrap();
    assert!(!paths.is_empty());
    for p in &paths {
        let a_returns = p
            .steps
            .iter()
            .filter(|s| s.symbol_name == "a" && s.action == codegraph_core::TraceAction::Return)
            .count();
        assert_eq!(a_returns, 1, "double return: {paths:?}");
    }
}

#[tokio::test]
async fn max_depth_one_still_returns_direct_caller_path() {
    // max_depth=1 (caller hops): path caller→target dài 2 vẫn phải được trace.
    let chains = HashMap::from([
        (
            A,
            vec![A, MARKER_CALL_ENTER, B, MARKER_CALL_EXIT, A, MARKER_RETURN],
        ),
        (B, vec![B, MARKER_RETURN]),
    ]);
    let idx = build_idx(chains, vec![]).await;
    let paths = idx.trace_full_flow(B, 1, 50, 1).await.unwrap();
    assert!(
        paths.iter().any(|p| names(p).contains(&"a".to_string())),
        "max_depth=1 phải trả path qua direct caller: {paths:?}"
    );
}

#[tokio::test]
async fn continue_heavy_loop_bounded_by_loop_cap() {
    // while { if (skip) { i++; continue; } i++; } — continue không được phép
    // mở rộng loop vô hạn: cap (mặc định 1) phải kết thúc loop.
    let chains = HashMap::from([(
        A,
        vec![
            A,
            MARKER_LOOP,
            MARKER_IF_TRUE,
            MARKER_CONTINUE,
            MARKER_BRANCH_END,
            A,
            MARKER_LOOP_BACK,
            A,
            MARKER_RETURN,
        ],
    )]);
    let idx = build_idx(chains, vec![]).await;
    let paths = idx.trace_full_flow(A, 5, 50, 1).await.unwrap();
    assert!(
        paths
            .iter()
            .any(|p| p.ended_with == PathEnding::Returned
                && names(p).last() == Some(&"a".to_string())),
        "continue phải thoát loop theo cap rồi return: {paths:?}"
    );
    for p in &paths {
        let loop_steps = p
            .steps
            .iter()
            .filter(|s| s.action == codegraph_core::TraceAction::Loop)
            .count();
        assert!(
            loop_steps <= 4,
            "loop không được mở rộng quá cap: {loop_steps}"
        );
    }
}
