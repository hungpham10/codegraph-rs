//! Effect classification cho call names — configurable classifier.
//!
//! Port nhẹ từ `walle/pkgs/rules/extraction/defaults.go` (DefaultRules) — bảng
//! pattern áp dụng mọi ngôn ngữ, first-match-wins theo thứ tự: pattern cụ thể
//! (framework/library) trước, generic fallback cuối. Không dùng imports để chọn
//! library rule (bản nhẹ) — classify theo call name là đủ cho impact/flow render.
//!
//! Project có thể bổ sung rule qua `.codegraph/config.toml` `[[effect_rules]]`
//! (schema `EffectRule` trong codegraph-core). Rule config được xét TRƯỚC bảng
//! default → override được, phần còn lại vẫn rơi về defaults.
//!
//! Classifier là "ambient config": `Orchestrator` install classifier của project
//! vào thread-local trước vòng parse song song; leaf `classify_effect` đọc từ
//! thread-local (chưa install → dùng bảng default — test/đường dẫn đơn file).

use codegraph_core::{EffectCallPattern, EffectRule, EffectType};
use std::cell::RefCell;
use std::sync::{Arc, OnceLock};

/// Cách match một rule lên call name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchTy {
    Prefix,
    Contains,
    Exact,
}

/// Một rule cụ thể — chuyển từ `EffectRule` (config) hoặc bảng default.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClassifierRule {
    matcher: MatchTy,
    text: String,
    effect: EffectType,
}

/// Bảng default — thứ tự quan trọng, đọc từ trên xuống, pattern đầu tiên match
/// sẽ thắng.
const DEFAULT_RULES: &[(MatchTy, &str, EffectType)] = &[
    // ── Prefix-based (high precision) ──
    (MatchTy::Prefix, "http.", EffectType::HttpCall),
    (MatchTy::Prefix, "net/http.", EffectType::HttpCall),
    (MatchTy::Prefix, "log.", EffectType::Log),
    (MatchTy::Prefix, "slog.", EffectType::Log),
    (MatchTy::Prefix, "os.", EffectType::FileRead),
    (MatchTy::Prefix, "open(", EffectType::FileRead),
    // ── Java library types ──
    (MatchTy::Contains, "RestTemplate", EffectType::HttpCall),
    (MatchTy::Contains, "retrofit", EffectType::HttpCall),
    (MatchTy::Contains, "WebClient", EffectType::HttpCall),
    (MatchTy::Contains, "FileInputStream", EffectType::FileRead),
    (MatchTy::Contains, "FileReader", EffectType::FileRead),
    (MatchTy::Contains, "BufferedReader", EffectType::FileRead),
    (MatchTy::Contains, "FileOutputStream", EffectType::FileWrite),
    (MatchTy::Contains, "FileWriter", EffectType::FileWrite),
    // ── Messaging / events ──
    (MatchTy::Contains, "kafka.", EffectType::EventEmit),
    (MatchTy::Contains, "rabbit", EffectType::EventEmit),
    (MatchTy::Contains, "amqp", EffectType::EventEmit),
    // ── SQL — explicit patterns ──
    (MatchTy::Contains, ".Query", EffectType::SqlQuery),
    (MatchTy::Contains, ".QueryRow", EffectType::SqlQuery),
    (MatchTy::Contains, ".Raw", EffectType::SqlQuery),
    (MatchTy::Contains, ".Select", EffectType::SqlQuery),
    (MatchTy::Contains, ".Find", EffectType::SqlQuery),
    (MatchTy::Contains, ".First", EffectType::SqlQuery),
    (MatchTy::Contains, ".Model(", EffectType::SqlQuery),
    (MatchTy::Contains, ".Exec", EffectType::SqlWrite),
    (MatchTy::Contains, ".Insert", EffectType::SqlWrite),
    (MatchTy::Contains, ".Update", EffectType::SqlWrite),
    (MatchTy::Contains, ".Delete(", EffectType::SqlWrite),
    (MatchTy::Contains, ".Create(", EffectType::SqlWrite),
    (MatchTy::Contains, ".Save(", EffectType::SqlWrite),
    (MatchTy::Contains, ".Session", EffectType::SqlWrite),
    // ── HTTP method calls ──
    (MatchTy::Prefix, "requests.", EffectType::HttpCall),
    (MatchTy::Contains, ".Get(", EffectType::HttpCall),
    (MatchTy::Contains, ".Post(", EffectType::HttpCall),
    (MatchTy::Contains, ".Put(", EffectType::HttpCall),
    (MatchTy::Contains, ".Delete(", EffectType::HttpCall),
    (MatchTy::Contains, ".Patch(", EffectType::HttpCall),
    (MatchTy::Contains, ".Do(", EffectType::HttpCall),
    (MatchTy::Contains, ".NewRequest", EffectType::HttpCall),
    // ── Event publish/consume ──
    (MatchTy::Contains, ".Publish", EffectType::EventEmit),
    (MatchTy::Contains, ".publish", EffectType::EventEmit),
    (MatchTy::Contains, ".Send", EffectType::EventEmit),
    (MatchTy::Contains, ".send", EffectType::EventEmit),
    (MatchTy::Contains, ".Produce", EffectType::EventEmit),
    (MatchTy::Contains, ".produce", EffectType::EventEmit),
    (MatchTy::Contains, ".Consume", EffectType::EventEmit),
    (MatchTy::Contains, ".consume", EffectType::EventEmit),
    (MatchTy::Contains, ".Subscribe", EffectType::EventEmit),
    (MatchTy::Contains, ".subscribe", EffectType::EventEmit),
    (MatchTy::Contains, ".Receive", EffectType::EventEmit),
    (MatchTy::Contains, ".receive", EffectType::EventEmit),
    // ── Cache ──
    (MatchTy::Contains, ".MGet", EffectType::CacheRead),
    (MatchTy::Contains, ".MSet", EffectType::CacheWrite),
    (MatchTy::Contains, ".HGet", EffectType::CacheRead),
    (MatchTy::Contains, ".HSet", EffectType::CacheWrite),
    (MatchTy::Contains, ".HGetAll", EffectType::CacheRead),
    (MatchTy::Contains, ".Del(", EffectType::CacheWrite),
    (MatchTy::Contains, ".Expire", EffectType::CacheWrite),
    (MatchTy::Contains, ".Exists", EffectType::CacheRead),
    (MatchTy::Contains, ".TTL", EffectType::CacheRead),
    // ── File I/O ──
    (MatchTy::Contains, ".Open", EffectType::FileRead),
    (MatchTy::Contains, ".ReadFile", EffectType::FileRead),
    (MatchTy::Contains, ".ReadAll", EffectType::FileRead),
    (MatchTy::Contains, ".WriteFile", EffectType::FileWrite),
    (MatchTy::Contains, ".WriteString", EffectType::FileWrite),
    (MatchTy::Contains, ".Create", EffectType::FileWrite),
    (MatchTy::Contains, ".Mkdir", EffectType::FileWrite),
    // ── Log ──
    (MatchTy::Contains, "logging.", EffectType::Log),
    (MatchTy::Contains, "logger.", EffectType::Log),
    (MatchTy::Contains, ".Printf", EffectType::Log),
    (MatchTy::Contains, ".Println", EffectType::Log),
    (MatchTy::Contains, ".Infof", EffectType::Log),
    (MatchTy::Contains, ".Info", EffectType::Log),
    (MatchTy::Contains, ".Errorf", EffectType::Log),
    (MatchTy::Contains, ".Error", EffectType::Log),
    (MatchTy::Contains, ".Warnf", EffectType::Log),
    (MatchTy::Contains, ".Warn", EffectType::Log),
    (MatchTy::Contains, ".Debugf", EffectType::Log),
    (MatchTy::Contains, ".Debug", EffectType::Log),
    // ── Generic fallbacks (no context — last resort) ──
    (MatchTy::Contains, ".Set", EffectType::CacheWrite),
    (MatchTy::Contains, ".Get", EffectType::SqlQuery),
];

/// Classifier cấu hình được — first-match-wins theo thứ tự `rules`.
#[derive(Debug, Clone)]
pub struct EffectClassifier {
    rules: Vec<ClassifierRule>,
}

/// Default = bảng built-in (behavior hiện tại khi không có config).
impl Default for EffectClassifier {
    fn default() -> Self {
        Self {
            rules: DEFAULT_RULES
                .iter()
                .map(|&(matcher, text, effect)| ClassifierRule {
                    matcher,
                    text: text.to_string(),
                    effect,
                })
                .collect(),
        }
    }
}

impl EffectClassifier {
    /// Rule config xét TRƯỚC bảng default (override), phần còn lại rơi về defaults.
    pub fn with_config(config_rules: Vec<EffectRule>) -> Self {
        let mut rules: Vec<ClassifierRule> =
            config_rules.into_iter().map(Self::from_rule).collect();
        rules.extend(Self::default().rules);
        Self { rules }
    }

    fn from_rule(rule: EffectRule) -> ClassifierRule {
        let (matcher, text) = match rule.call {
            EffectCallPattern::Prefix { prefix } => (MatchTy::Prefix, prefix),
            EffectCallPattern::Contains { contains } => (MatchTy::Contains, contains),
            EffectCallPattern::Exact { exact } => (MatchTy::Exact, exact),
        };
        ClassifierRule {
            matcher,
            text,
            effect: rule.effect,
        }
    }

    /// Phân loại theo rule đầu tiên match — `(effect, text của rule đã match)`.
    pub fn classify(&self, call_name: &str) -> (EffectType, Option<&str>) {
        for r in &self.rules {
            let hit = match r.matcher {
                MatchTy::Prefix => call_name.starts_with(&r.text),
                MatchTy::Contains => call_name.contains(&r.text),
                MatchTy::Exact => call_name == r.text,
            };
            if hit {
                return (r.effect, Some(r.text.as_str()));
            }
        }
        (EffectType::None, None)
    }
}

// Thread-local classifier hiện tại — `Orchestrator` install trước vòng parse.
thread_local! {
    static CURRENT: RefCell<Option<Arc<EffectClassifier>>> = const { RefCell::new(None) };
}

/// Default classifier dùng chung (khi chưa install / test).
fn default_classifier() -> &'static EffectClassifier {
    static DEFAULT: OnceLock<EffectClassifier> = OnceLock::new();
    DEFAULT.get_or_init(EffectClassifier::default)
}

/// Install classifier cho thread đang chạy (gọi đầu mỗi job parse). `None` reset
/// về default.
pub fn install_current(classifier: Option<Arc<EffectClassifier>>) {
    CURRENT.with(|slot| *slot.borrow_mut() = classifier);
}

/// Phân loại effect của một call theo classifier của project (fallback default).
///
/// Trả về `(effect, pattern đã match)` — pattern dùng làm effect_desc.
pub fn classify_effect(call_name: &str) -> (EffectType, Option<String>) {
    CURRENT.with(|slot| {
        let borrow = slot.borrow();
        match borrow.as_ref() {
            Some(c) => {
                let (e, m) = c.classify(call_name);
                (e, m.map(str::to_owned))
            }
            None => {
                let (e, m) = default_classifier().classify(call_name);
                (e, m.map(str::to_owned))
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_patterns() {
        assert_eq!(classify_effect("db.Query").0, EffectType::SqlQuery);
        assert_eq!(classify_effect("r.DB.QueryRow").0, EffectType::SqlQuery);
        assert_eq!(classify_effect("orm.Model(").0, EffectType::SqlQuery);
        assert_eq!(classify_effect("tx.Exec").0, EffectType::SqlWrite);
        assert_eq!(classify_effect("repo.Insert").0, EffectType::SqlWrite);
        assert_eq!(classify_effect("db.Create(").0, EffectType::SqlWrite);
    }

    #[test]
    fn http_patterns() {
        assert_eq!(classify_effect("requests.get").0, EffectType::HttpCall);
        assert_eq!(classify_effect("client.Post(").0, EffectType::HttpCall);
        assert_eq!(classify_effect("http.Get").0, EffectType::HttpCall);
        assert_eq!(classify_effect("svc.NewRequest").0, EffectType::HttpCall);
    }

    #[test]
    fn cache_patterns() {
        assert_eq!(classify_effect("r.Get(").0, EffectType::HttpCall);
        assert_eq!(classify_effect("cache.Get(").0, EffectType::HttpCall);
        assert_eq!(classify_effect("cache.Set(").0, EffectType::CacheWrite);
        assert_eq!(classify_effect("redis.HGet").0, EffectType::CacheRead);
        assert_eq!(classify_effect("redis.HSet").0, EffectType::CacheWrite);
    }

    #[test]
    fn event_file_log() {
        assert_eq!(classify_effect("kafka.Produce").0, EffectType::EventEmit);
        assert_eq!(classify_effect("producer.Send").0, EffectType::EventEmit);
        assert_eq!(classify_effect("mq.consume").0, EffectType::EventEmit);
        assert_eq!(classify_effect("os.ReadFile").0, EffectType::FileRead);
        assert_eq!(classify_effect("f.WriteString").0, EffectType::FileWrite);
        assert_eq!(classify_effect("log.Info").0, EffectType::Log);
        assert_eq!(classify_effect("fmt.Println").0, EffectType::Log);
    }

    #[test]
    fn unknown_is_none() {
        assert_eq!(classify_effect("validateUser"), (EffectType::None, None));
        assert_eq!(classify_effect("sendEmail"), (EffectType::None, None));
    }

    /// Config rule (prefix/contains/exact) được xét trước default → override.
    #[test]
    fn config_rules_override_defaults() {
        let rules = vec![
            EffectRule {
                call: EffectCallPattern::Prefix {
                    prefix: "db.".to_string(),
                },
                effect: EffectType::SqlQuery,
            },
            EffectRule {
                call: EffectCallPattern::Exact {
                    exact: "sendEmail".to_string(),
                },
                effect: EffectType::EventEmit,
            },
        ];
        let c = EffectClassifier::with_config(rules);
        assert_eq!(c.classify("db.Exec").0, EffectType::SqlQuery); // trước default ".Exec"
        assert_eq!(c.classify("sendEmail").0, EffectType::EventEmit);
        assert_eq!(
            c.classify("sendEmail"),
            (EffectType::EventEmit, Some("sendEmail"))
        );
        // Không có rule config → rơi về default.
        assert_eq!(c.classify("kafka.Produce").0, EffectType::EventEmit);
        assert_eq!(c.classify("noSuchThing"), (EffectType::None, None));
    }

    /// Rule config install vào thread-local → `classify_effect` đọc được.
    #[test]
    fn installed_classifier_is_used_by_leaf() {
        let rules = vec![EffectRule {
            call: EffectCallPattern::Contains {
                contains: "legacy-".to_string(),
            },
            effect: EffectType::FileWrite,
        }];
        install_current(Some(Arc::new(EffectClassifier::with_config(rules))));
        assert_eq!(classify_effect("legacy-writer").0, EffectType::FileWrite);
        assert_eq!(
            classify_effect("legacy-writer").1.as_deref(),
            Some("legacy-")
        );
        install_current(None);
        assert_eq!(classify_effect("legacy-writer").0, EffectType::None);
    }
}
