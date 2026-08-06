//! Golden: project effect rules (installed classifier) reach `CallRecord.effect`
//! qua pipeline parse thật — chứng minh `[[effect_rules]]` config override
//! được bảng default và đến được call record (đầu vào của graph ingest).

use codegraph_core::{EffectCallPattern, EffectRule, EffectType};
use codegraph_extract::languages::effects::{install_current, EffectClassifier};
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
            effect: EffectType::EventEmit,
        },
        EffectRule {
            call: EffectCallPattern::Prefix {
                prefix: "legacy.".to_string(),
            },
            effect: EffectType::FileWrite,
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
