//! Golden: project effect rules + flow markers (`[[effect_rules]]`) qua pipeline
//! parse thật — chứng minh config override được bảng default và đến được
//! `CallRecord.effect` (đầu vào của graph ingest), cùng control-flow marker
//! (`try_start`, `catch_start`, `try_end`, `throw`, …) được emit vào chain.
use codegraph_core::{
    EffectCallPattern, EffectRule, EffectType, FlowMarker, MARKER_BRANCH_END, MARKER_CATCH_START,
    MARKER_IF_FALSE, MARKER_IF_TRUE, MARKER_LOOP_BACK, MARKER_THROW, MARKER_TRY_END,
    MARKER_TRY_START,
};
use codegraph_extract::languages::effects::{install_current, EffectClassifier};
use codegraph_extract::languages::flows;
use codegraph_extract::registry;
use std::sync::Arc;

fn effects_of(lang: &str, src: &str) -> Vec<(String, EffectType)> {
    let parser = registry()
        .into_iter()
        .find(|p| p.name() == lang)
        .unwrap_or_else(|| panic!("no parser {lang}"));
    let res = parser.parse_file("effects.test", src).expect("parse");
    res.calls
        .iter()
        .map(|c| (c.call_name.clone(), c.effect))
        .collect()
}

/// Config rules (exact + prefix) override defaults và hiện ra trên CallRecord.
#[test]
fn configured_rules_reach_call_record_effect() {
    let rules = vec![
        EffectRule {
            call: EffectCallPattern::Exact {
                exact: "sendEmail".to_string(),
            },
            effect: Some(EffectType::EventEmit),
            marker: None,
            label: None,
            keep_call: None,
        },
        EffectRule {
            call: EffectCallPattern::Prefix {
                prefix: "legacy.".to_string(),
            },
            effect: Some(EffectType::FileWrite),
            marker: None,
            label: None,
            keep_call: None,
        },
    ];
    install_current(Some(Arc::new(EffectClassifier::with_config(rules))));

    let effects = effects_of(
        "javascript",
        "function f() { sendEmail(\"x\"); legacy.write(); db.Query(); }",
    );
    let get = |name: &str| -> EffectType {
        effects
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, e)| *e)
            .unwrap_or_else(|| panic!("call {name} not captured: {effects:?}"))
    };
    // Config rule thắng (default cho "sendEmail" là None).
    assert_eq!(get("sendEmail"), EffectType::EventEmit);
    // Config prefix "legacy." thắng (default None).
    assert_eq!(get("legacy.write"), EffectType::FileWrite);
    // Không có config rule → rơi về bảng default.
    assert_eq!(get("db.Query"), EffectType::SqlQuery);

    install_current(None);
}

/// Không install classifier → parse dùng bảng default (behavior cũ).
#[test]
fn default_classifier_when_not_installed() {
    install_current(None);
    let effects = effects_of("javascript", "function f() { db.Exec(); }");
    assert_eq!(effects[0].1, EffectType::SqlWrite);
}

// ── Flow rules (gộp từ flows_config.rs) ──

fn install_flow_rules(throw_labels: &[Option<&str>]) {
    let mk = |call: EffectCallPattern, marker: FlowMarker, label: Option<&str>| EffectRule {
        call,
        effect: None,
        marker: Some(marker),
        label: label.map(str::to_string),
        keep_call: None,
    };
    let throw_label = throw_labels.first().copied().flatten();
    let rules = vec![
        mk(
            EffectCallPattern::Exact {
                exact: "PG_TRY".into(),
            },
            FlowMarker::TryStart,
            None,
        ),
        mk(
            EffectCallPattern::Exact {
                exact: "PG_CATCH".into(),
            },
            FlowMarker::CatchStart,
            None,
        ),
        mk(
            EffectCallPattern::Exact {
                exact: "PG_END_TRY".into(),
            },
            FlowMarker::TryEnd,
            None,
        ),
        mk(
            EffectCallPattern::Exact {
                exact: "PG_RE_THROW".into(),
            },
            FlowMarker::Throw,
            None,
        ),
        mk(
            EffectCallPattern::Contains {
                contains: "ereport".into(),
            },
            FlowMarker::Throw,
            throw_label,
        ),
    ];
    flows::install_current(Some(Arc::new(flows::FlowClassifier::with_config(rules))));
}

/// Fixture đúng PostgreSQL idiom: `PG_TRY(); … ereport(ERROR); PG_CATCH();
/// handler; PG_END_TRY();` — macro nằm dạng statement tuần tự.
const PG_FIXTURE: &str = r#"
int handle(int v) {
    PG_TRY();
    if (v < 0)
        ereport(ERROR);
    PG_CATCH();
    cleanup();
    PG_END_TRY();
    return 0;
}
"#;

fn chain_of(lang: &str, src: &str) -> Vec<u64> {
    let parser = registry()
        .into_iter()
        .find(|p| p.name() == lang)
        .unwrap_or_else(|| panic!("no parser {lang}"));
    let res = parser.parse_file("flows.test", src).expect("parse");
    let chain = res
        .chains
        .values()
        .next()
        .expect("at least one chain")
        .clone();
    flows::install_current(None);
    chain
}

/// `PG_TRY(); ereport(); PG_CATCH(); handler; PG_END_TRY()` — layout marker
/// đúng trình tự interpreter: TRY_START, body (+THROW), TRY_END, CATCH_START,
/// handler, BRANCH_END.
#[test]
fn pg_macro_emits_try_catch_layout() {
    install_flow_rules(&[None]);
    let chain = chain_of("c", PG_FIXTURE);
    let find_all = |chain: &[u64], m: u64| -> Vec<usize> {
        chain
            .iter()
            .enumerate()
            .filter(|(_, &e)| e == m)
            .map(|(i, _)| i)
            .collect()
    };
    assert_eq!(
        find_all(&chain, MARKER_TRY_START).len(),
        1,
        "chain: {chain:?}"
    );
    // THROW có trên try body (TRƯỚC TRY_END).
    let throw = find_all(&chain, MARKER_THROW);
    assert_eq!(throw.len(), 1, "chain: {chain:?}");
    let try_end = find_all(&chain, MARKER_TRY_END);
    assert_eq!(try_end.len(), 1, "chain: {chain:?}");
    assert!(
        throw[0] < try_end[0],
        "THROW phải nằm trong try body: {chain:?}"
    );
    // CATCH_START nằm ngay SAU TRY_END (giả định handler_positions).
    let catch = find_all(&chain, MARKER_CATCH_START);
    assert_eq!(catch.len(), 1, "chain: {chain:?}");
    assert_eq!(catch[0], try_end[0] + 1, "chain: {chain:?}");
    // Handler được đóng bằng BRANCH_END.
    assert!(chain.contains(&MARKER_BRANCH_END), "chain: {chain:?}");
}

/// `PG_RE_THROW` bên trong handler (giữa CATCH_START…BRANCH_END) — THROW thứ
/// hai xuất hiện, interpreter sẽ unwind vì catch_stack rỗng tại handler.
#[test]
fn pg_rethrow_inside_handler_emits_throw() {
    install_flow_rules(&[None]);
    let chain = chain_of(
        "c",
        r#"
int handle(int v) {
    PG_TRY();
    if (v < 0)
        ereport(ERROR);
    PG_CATCH();
    PG_RE_THROW();
    PG_END_TRY();
    return 0;
}
"#,
    );
    let throw_count = chain.iter().filter(|&&e| e == MARKER_THROW).count();
    assert_eq!(throw_count, 2, "chain: {chain:?}");
}

/// `label` trên flow rule throw override nhãn exception ("throw Error") —
/// branch label hiện ở CallRecord.condition của record nhãn (call_name rỗng).
#[test]
fn throw_rule_label_overrides_exception_type() {
    install_flow_rules(&[Some("Error")]);
    let parser = registry()
        .into_iter()
        .find(|p| p.name() == "c")
        .unwrap_or_else(|| panic!("no parser c"));
    let res = parser.parse_file("flows.test", PG_FIXTURE).expect("parse");
    let labeled = res
        .calls
        .iter()
        .find(|c| c.call_name.is_empty() && c.condition.as_deref() == Some("throw Error"));
    assert!(
        labeled.is_some(),
        "labels: {:?}",
        res.calls
            .iter()
            .filter(|c| c.call_name.is_empty())
            .map(|c| c.condition.clone())
            .collect::<Vec<_>>()
    );
    flows::install_current(None);
}

/// Không có flow rule nào (classifier rỗng/uninstalled) → chain không marker
/// macro — regression: hành vi cũ giữ nguyên.
#[test]
fn no_rules_no_markers() {
    flows::install_current(None);
    let chain = chain_of("c", PG_FIXTURE);
    for m in [
        MARKER_TRY_START,
        MARKER_TRY_END,
        MARKER_CATCH_START,
        MARKER_THROW,
    ] {
        assert!(!chain.contains(&m), "unexpected marker {m}: {chain:?}");
    }
}

/// E2E interpreter: parse C fixture → ingest GraphIndex (in-memory) →
/// `trace_full_flow`. Throw của `ereport` trong try body của `handle` phải
/// nhảy vào handler `cleanup` (PG_CATCH) và path kết thúc Returned — đúng
/// semantics exception engine, không cần sửa gì phía graph.
#[tokio::test]
async fn pg_throw_is_caught_by_pg_catch_handler() {
    use codegraph_core::PathEnding;

    install_flow_rules(&[None]);
    let parser = registry()
        .into_iter()
        .find(|p| p.name() == "c")
        .unwrap_or_else(|| panic!("no parser c"));
    let res = parser.parse_file("flows.test", PG_FIXTURE).expect("parse");
    assert_eq!(res.chains.len(), 1, "fixture có một function");
    let target = *res.chains.keys().next().unwrap();

    let mut idx = codegraph_graph::GraphIndex::in_memory();
    idx.ingest(&[res], None).await.unwrap();

    let paths = idx.trace_full_flow(target, 5, 50, 1).await.expect("trace");
    assert!(
        paths.iter().any(|p| p.ended_with == PathEnding::Returned
            && p.steps
                .iter()
                .any(|s| s.action == codegraph_core::TraceAction::Throw)),
        "throw phải được ghi trong path: {paths:?}"
    );
    assert!(
        paths
            .iter()
            .any(|p| p.steps.iter().any(|s| s.symbol_name == "cleanup")),
        "handler PG_CATCH (cleanup) phải chạy trong path: {paths:?}"
    );
    flows::install_current(None);
}

/// `effect = "none"` shadow bảng default: `db.Query` mặc định SqlQuery, config
/// effect=None chặn default → EffectType::None.
#[test]
fn effect_none_shadows_default() {
    let rules = vec![EffectRule {
        call: EffectCallPattern::Exact {
            exact: "db.Query".into(),
        },
        effect: Some(EffectType::None),
        marker: None,
        label: None,
        keep_call: None,
    }];
    install_current(Some(Arc::new(EffectClassifier::with_config(rules))));

    let effects = effects_of("javascript", "function f() { db.Query(); }");
    assert_eq!(effects[0].1, EffectType::None, "{effects:?}");
    install_current(None);
}

/// Rule mang CÙNG effect + marker (ereport): CallRecord.effect = Panic VÀ
/// chain có MARKER_TRY_START…THROW…TRY_END…CATCH_START…BRANCH_END.
#[test]
fn rule_with_effect_and_marker() {
    let rules = vec![
        EffectRule {
            call: EffectCallPattern::Exact {
                exact: "PG_TRY".into(),
            },
            effect: None,
            marker: Some(FlowMarker::TryStart),
            label: None,
            keep_call: None,
        },
        EffectRule {
            call: EffectCallPattern::Exact {
                exact: "PG_CATCH".into(),
            },
            effect: None,
            marker: Some(FlowMarker::CatchStart),
            label: None,
            keep_call: None,
        },
        EffectRule {
            call: EffectCallPattern::Exact {
                exact: "PG_END_TRY".into(),
            },
            effect: None,
            marker: Some(FlowMarker::TryEnd),
            label: None,
            keep_call: None,
        },
        EffectRule {
            call: EffectCallPattern::Contains {
                contains: "ereport".into(),
            },
            effect: Some(EffectType::Panic),
            marker: Some(FlowMarker::Throw),
            label: Some("Error".into()),
            keep_call: None,
        },
    ];
    flows::install_current(Some(Arc::new(flows::FlowClassifier::with_config(rules))));
    let effects_rules = vec![EffectRule {
        call: EffectCallPattern::Contains {
            contains: "ereport".into(),
        },
        effect: Some(EffectType::Panic),
        marker: None,
        label: None,
        keep_call: None,
    }];
    install_current(Some(Arc::new(EffectClassifier::with_config(effects_rules))));
    let chain = chain_of("c", PG_FIXTURE);
    let parser = registry()
        .into_iter()
        .find(|p| p.name() == "c")
        .unwrap_or_else(|| panic!("no parser c"));
    let res = parser.parse_file("flows.test", PG_FIXTURE).expect("parse");
    let call = res
        .calls
        .iter()
        .find(|c| c.call_name.contains("ereport"))
        .expect("ereport captured");
    assert_eq!(call.effect, EffectType::Panic, "{call:?}");
    let pos = |m: u64| {
        chain
            .iter()
            .position(|&e| e == m)
            .expect("marker {m}: {chain:?}")
    };
    assert!(pos(MARKER_TRY_START) < pos(MARKER_THROW));
    assert!(pos(MARKER_THROW) < pos(MARKER_TRY_END));
    assert_eq!(pos(MARKER_TRY_END) + 1, pos(MARKER_CATCH_START));
    assert!(chain.contains(&MARKER_BRANCH_END));
    flows::install_current(None);
    install_current(None);
}

/// 4 nhánh macro if/else/endif + loop emit đúng MARKER_IF_TRUE/IF_FALSE/
/// BRANCH_END/LOOP/LOOP_BACK, keep_call=false (chỉ marker, không CallRecord).
#[test]
fn four_flow_markers_emitted() {
    const IF_FIXTURE: &str = r#"
int f(int x) {
    IF(x > 0);
    do_something();
    ELSE();
    do_nothing();
    ENDIF();
    return 0;
}
"#;
    const LOOP_FIXTURE: &str = r#"
int f(int n) {
    LOOP(i < n);
    work();
    LOOP_BACK();
    return 0;
}
"#;
    let rules = vec![
        EffectRule {
            call: EffectCallPattern::Exact { exact: "IF".into() },
            effect: None,
            marker: Some(FlowMarker::IfTrue),
            label: None,
            keep_call: None,
        },
        EffectRule {
            call: EffectCallPattern::Exact {
                exact: "ELSE".into(),
            },
            effect: None,
            marker: Some(FlowMarker::IfFalse),
            label: None,
            keep_call: None,
        },
        EffectRule {
            call: EffectCallPattern::Exact {
                exact: "ENDIF".into(),
            },
            effect: None,
            marker: Some(FlowMarker::BranchEnd),
            label: None,
            keep_call: None,
        },
        EffectRule {
            call: EffectCallPattern::Exact {
                exact: "LOOP".into(),
            },
            effect: None,
            marker: Some(FlowMarker::Loop),
            label: None,
            keep_call: None,
        },
        EffectRule {
            call: EffectCallPattern::Exact {
                exact: "LOOP_BACK".into(),
            },
            effect: None,
            marker: Some(FlowMarker::LoopBack),
            label: None,
            keep_call: None,
        },
    ];
    flows::install_current(Some(Arc::new(flows::FlowClassifier::with_config(
        rules.clone(),
    ))));

    let chain = chain_of("c", IF_FIXTURE);
    assert!(chain.contains(&MARKER_IF_TRUE), "{chain:?}");
    assert!(chain.contains(&MARKER_IF_FALSE), "{chain:?}");
    assert!(chain.contains(&MARKER_BRANCH_END), "{chain:?}");

    flows::install_current(Some(Arc::new(flows::FlowClassifier::with_config(
        rules.clone(),
    ))));
    let chain = chain_of("c", LOOP_FIXTURE);
    assert!(chain.contains(&MARKER_LOOP_BACK), "{chain:?}");

    flows::install_current(None);
}

/// `branch_end` (ENDIF) không đụng state try: TRY_START → body → branch_end →
/// TRY_END (không handler) vẫn sinh TRY_END, không sinh BRANCH_END ở bước close.
#[test]
fn branch_end_does_not_touch_try_state() {
    const TRY_THEN_ENDIF: &str = r#"
int f(int v) {
    PG_TRY();
    if (v < 0)
        ereport(ERROR);
    ENDIF();
    PG_END_TRY();
    return 0;
}
"#;
    let rules = vec![
        EffectRule {
            call: EffectCallPattern::Exact {
                exact: "PG_TRY".into(),
            },
            effect: None,
            marker: Some(FlowMarker::TryStart),
            label: None,
            keep_call: None,
        },
        EffectRule {
            call: EffectCallPattern::Exact {
                exact: "PG_END_TRY".into(),
            },
            effect: None,
            marker: Some(FlowMarker::TryEnd),
            label: None,
            keep_call: None,
        },
        EffectRule {
            call: EffectCallPattern::Exact {
                exact: "ENDIF".into(),
            },
            effect: None,
            marker: Some(FlowMarker::BranchEnd),
            label: None,
            keep_call: None,
        },
        EffectRule {
            call: EffectCallPattern::Contains {
                contains: "ereport".into(),
            },
            effect: None,
            marker: Some(FlowMarker::Throw),
            label: None,
            keep_call: None,
        },
    ];
    flows::install_current(Some(Arc::new(flows::FlowClassifier::with_config(rules))));
    let chain = chain_of("c", TRY_THEN_ENDIF);
    let pos = |m: u64| {
        chain
            .iter()
            .position(|&e| e == m)
            .expect("marker {m}: {chain:?}")
    };
    // TRY_START mở, PG_END_TRY đóng (không bị ENDIF/BranchEnd làm sớm state try).
    assert!(pos(MARKER_TRY_START) < pos(MARKER_TRY_END));
    // ENDIF sinh BRANCH_END nhưng KHÔNG đóng try → TRY_END vẫn xuất hiện sau.
    assert!(chain.contains(&MARKER_BRANCH_END));
    assert!(pos(MARKER_BRANCH_END) < pos(MARKER_TRY_END));
    flows::install_current(None);
}
