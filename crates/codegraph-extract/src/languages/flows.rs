//! Flow classification cho call names — macro/magic call → marker
//! control-flow (`[[effect_rules]]` trong .codegraph/config.toml).
//!
//! Khác với effects (có bảng default của framework), ở đây **KHÔNG có bất kỳ
//! rule default nào**: classifier được tạo từ config của project thôi; config
//! trống → mọi call map `None` → chain giữ nguyên hành vi cũ (zero regression).
//! Idiom của từng repo (PG_TRY/CATCH, Go recover, Lua pcall…) do LLM/user khai
//! báo qua MCP `codegraph_config_effects` — chi tiết xem docs/architecture.md
//! § Effect rules.
//!
//! Classifier là "ambient config" y hệt `effects.rs`: `Orchestrator` install
//! vào thread-local trước vòng parse song song; leaf `classify_flow` đọc từ
//! thread-local, chưa install → `None` mọi call (test / đường đơn file).

use codegraph_core::{EffectCallPattern, EffectRule, FlowMarker};
use std::cell::RefCell;
use std::sync::Arc;

/// Cách match một rule lên call name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchTy {
    Prefix,
    Contains,
    Exact,
}

/// Một rule cụ thể — chuyển từ `FlowRule` (config).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClassifierRule {
    matcher: MatchTy,
    text: String,
    marker: FlowMarker,
    label: Option<String>,
    keep_call: bool,
}

/// Kết quả classify một call có rule flow khớp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowHit {
    pub marker: FlowMarker,
    /// Nhãn exception override (chỉ cho `marker = throw`); `None` → nhãn suy
    /// luận từ exception type nếu nhận diện được.
    pub label: Option<String>,
    /// Call thật (throw-er) → giữ CallRecord ngang marker; macro → chỉ marker.
    pub keep_call: bool,
}

/// Classifier flow — user rules only, first-match-wins theo thứ tự khai báo
/// (`list_flow_rules` index order = match order). KHÔNG có bảng default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlowClassifier {
    rules: Vec<ClassifierRule>,
}

impl FlowClassifier {
    /// Classifier từ config — không thêm default nào; config trống → rỗng.
    pub fn with_config(rules: Vec<EffectRule>) -> Self {
        Self {
            rules: rules.into_iter().filter_map(Self::from_rule).collect(),
        }
    }

    fn from_rule(rule: EffectRule) -> Option<ClassifierRule> {
        let marker = rule.marker?;
        let (matcher, text) = match &rule.call {
            EffectCallPattern::Prefix { prefix } => (MatchTy::Prefix, prefix.clone()),
            EffectCallPattern::Contains { contains } => (MatchTy::Contains, contains.clone()),
            EffectCallPattern::Exact { exact } => (MatchTy::Exact, exact.clone()),
        };
        Some(ClassifierRule {
            matcher,
            text,
            marker,
            keep_call: rule.keep_call_or_default(),
            label: rule.label,
        })
    }

    /// Rule đầu tiên match wins — `None` khi không rule nào (hành vi cũ).
    pub fn classify(&self, call_name: &str) -> Option<FlowHit> {
        self.rules
            .iter()
            .find(|r| match r.matcher {
                MatchTy::Prefix => call_name.starts_with(&r.text),
                MatchTy::Contains => call_name.contains(&r.text),
                MatchTy::Exact => call_name == r.text,
            })
            .map(|r| FlowHit {
                marker: r.marker,
                label: r.label.clone(),
                keep_call: r.keep_call,
            })
    }
}

// Thread-local classifier hiện tại — `Orchestrator` install trước vòng parse.
thread_local! {
    static CURRENT: RefCell<Option<Arc<FlowClassifier>>> = const { RefCell::new(None) };
}

/// Install classifier cho thread đang chạy (gọi đầu mỗi job parse). `None`
/// reset về rỗng (không rule nào).
pub fn install_current(classifier: Option<Arc<FlowClassifier>>) {
    CURRENT.with(|slot| *slot.borrow_mut() = classifier);
}

/// Classify flow của một call theo classifier của project — `None` nếu không
/// rule nào match (classifier chưa install = rỗng, cùng hành vi).
pub fn classify_flow(call_name: &str) -> Option<FlowHit> {
    CURRENT.with(|slot| {
        let borrow = slot.borrow();
        borrow.as_ref().and_then(|c| c.classify(call_name))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::{
        EffectCallPattern::{Contains, Exact},
        EffectType,
    };

    fn rule(call: EffectCallPattern, marker: FlowMarker) -> EffectRule {
        EffectRule {
            call,
            effect: Some(EffectType::None),
            marker: Some(marker),
            label: None,
            keep_call: None,
        }
    }

    /// Rule khai báo match đúng pattern (`keep_call` default: throw=true).
    #[test]
    fn classifies_config_rules() {
        let c = FlowClassifier::with_config(vec![
            rule(
                Exact {
                    exact: "PG_TRY".into(),
                },
                FlowMarker::TryStart,
            ),
            rule(
                Exact {
                    exact: "PG_CATCH".into(),
                },
                FlowMarker::CatchStart,
            ),
            rule(
                Contains {
                    contains: "ereport".into(),
                },
                FlowMarker::Throw,
            ),
        ]);
        assert_eq!(
            c.classify("PG_TRY").map(|h| h.marker),
            Some(FlowMarker::TryStart)
        );
        // Macro try/catch: keep_call=false.
        assert!(!c.classify("PG_TRY").unwrap().keep_call);
        assert_eq!(
            c.classify("ereport(ERROR)").map(|h| h.marker),
            Some(FlowMarker::Throw)
        );
        // Throw-er là call thật: keep_call=true.
        assert!(c.classify("ereport(ERROR)").unwrap().keep_call);
    }

    /// Không rule nào → không match gì (`None`) — hành vi cũ.
    #[test]
    fn empty_classifier_is_none() {
        assert_eq!(FlowClassifier::default().classify("PG_TRY"), None);
        assert_eq!(classify_flow("PG_CATCH"), None);
    }

    /// Rule đầu tiên match wins theo thứ tự khai báo.
    #[test]
    fn first_match_wins() {
        let c = FlowClassifier::with_config(vec![
            rule(
                Exact {
                    exact: "ereport".into(),
                },
                FlowMarker::Return,
            ),
            rule(
                Contains {
                    contains: "ereport".into(),
                },
                FlowMarker::Throw,
            ),
        ]);
        // Exact "ereport" khai báo trước → thắng cho chính xác "ereport".
        assert_eq!(
            c.classify("ereport").map(|h| h.marker),
            Some(FlowMarker::Return)
        );
        assert_eq!(
            c.classify("ereport(ERROR)").map(|h| h.marker),
            Some(FlowMarker::Throw)
        );
    }

    /// Label + keep_call override từ config được giữ nguyên.
    #[test]
    fn label_and_keep_call_override() {
        let c = FlowClassifier::with_config(vec![EffectRule {
            call: Contains {
                contains: "err".into(),
            },
            effect: Some(EffectType::None),
            marker: Some(FlowMarker::Throw),
            label: Some("Error".into()),
            keep_call: Some(false),
        }]);
        let hit = c.classify("errfinish").unwrap();
        assert_eq!(hit.label.as_deref(), Some("Error"));
        assert!(!hit.keep_call);
    }

    /// Install thread-local → `classify_flow` đọc được; reset → rỗng.
    #[test]
    fn installed_classifier_is_used_by_leaf() {
        let c = FlowClassifier::with_config(vec![rule(
            Exact {
                exact: "siglongjmp".into(),
            },
            FlowMarker::Throw,
        )]);
        install_current(Some(Arc::new(c)));
        assert_eq!(
            classify_flow("siglongjmp").map(|h| h.marker),
            Some(FlowMarker::Throw)
        );
        install_current(None);
        assert_eq!(classify_flow("siglongjmp"), None);
    }
}
