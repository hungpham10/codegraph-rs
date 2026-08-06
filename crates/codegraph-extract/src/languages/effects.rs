//! Effect classification cho call names.
//!
//! Port nhẹ từ `walle/pkgs/rules/extraction/defaults.go` (DefaultRules) — bảng
//! pattern áp dụng mọi ngôn ngữ, first-match-wins theo thứ tự: pattern cụ thể
//! (framework/library) trước, generic fallback cuối. Không dùng imports để chọn
//! library rule (bản nhẹ) — classify theo call name là đủ cho impact/flow render.

use codegraph_core::EffectType;

#[derive(Clone, Copy)]
enum MatchTy {
    Prefix,
    Contains,
}

#[derive(Clone, Copy)]
struct Pattern {
    matcher: MatchTy,
    text: &'static str,
    effect: EffectType,
}

/// Thứ tự quan trọng — đọc từ trên xuống, pattern đầu tiên match sẽ thắng.
const PATTERNS: &[Pattern] = &[
    // ── Prefix-based (high precision) ──
    Pattern {
        matcher: MatchTy::Prefix,
        text: "http.",
        effect: EffectType::HttpCall,
    },
    Pattern {
        matcher: MatchTy::Prefix,
        text: "net/http.",
        effect: EffectType::HttpCall,
    },
    Pattern {
        matcher: MatchTy::Prefix,
        text: "log.",
        effect: EffectType::Log,
    },
    Pattern {
        matcher: MatchTy::Prefix,
        text: "slog.",
        effect: EffectType::Log,
    },
    Pattern {
        matcher: MatchTy::Prefix,
        text: "os.",
        effect: EffectType::FileRead,
    },
    Pattern {
        matcher: MatchTy::Prefix,
        text: "open(",
        effect: EffectType::FileRead,
    },
    // ── Java library types ──
    Pattern {
        matcher: MatchTy::Contains,
        text: "RestTemplate",
        effect: EffectType::HttpCall,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: "retrofit",
        effect: EffectType::HttpCall,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: "WebClient",
        effect: EffectType::HttpCall,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: "FileInputStream",
        effect: EffectType::FileRead,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: "FileReader",
        effect: EffectType::FileRead,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: "BufferedReader",
        effect: EffectType::FileRead,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: "FileOutputStream",
        effect: EffectType::FileWrite,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: "FileWriter",
        effect: EffectType::FileWrite,
    },
    // ── Messaging / events ──
    Pattern {
        matcher: MatchTy::Contains,
        text: "kafka.",
        effect: EffectType::EventEmit,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: "rabbit",
        effect: EffectType::EventEmit,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: "amqp",
        effect: EffectType::EventEmit,
    },
    // ── SQL — explicit patterns ──
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Query",
        effect: EffectType::SqlQuery,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".QueryRow",
        effect: EffectType::SqlQuery,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Raw",
        effect: EffectType::SqlQuery,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Select",
        effect: EffectType::SqlQuery,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Find",
        effect: EffectType::SqlQuery,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".First",
        effect: EffectType::SqlQuery,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Model(",
        effect: EffectType::SqlQuery,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Exec",
        effect: EffectType::SqlWrite,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Insert",
        effect: EffectType::SqlWrite,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Update",
        effect: EffectType::SqlWrite,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Delete(",
        effect: EffectType::SqlWrite,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Create(",
        effect: EffectType::SqlWrite,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Save(",
        effect: EffectType::SqlWrite,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Session",
        effect: EffectType::SqlWrite,
    },
    // ── HTTP method calls ──
    Pattern {
        matcher: MatchTy::Prefix,
        text: "requests.",
        effect: EffectType::HttpCall,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Get(",
        effect: EffectType::HttpCall,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Post(",
        effect: EffectType::HttpCall,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Put(",
        effect: EffectType::HttpCall,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Delete(",
        effect: EffectType::HttpCall,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Patch(",
        effect: EffectType::HttpCall,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Do(",
        effect: EffectType::HttpCall,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".NewRequest",
        effect: EffectType::HttpCall,
    },
    // ── Event publish/consume ──
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Publish",
        effect: EffectType::EventEmit,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".publish",
        effect: EffectType::EventEmit,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Send",
        effect: EffectType::EventEmit,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".send",
        effect: EffectType::EventEmit,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Produce",
        effect: EffectType::EventEmit,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".produce",
        effect: EffectType::EventEmit,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Consume",
        effect: EffectType::EventEmit,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".consume",
        effect: EffectType::EventEmit,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Subscribe",
        effect: EffectType::EventEmit,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".subscribe",
        effect: EffectType::EventEmit,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Receive",
        effect: EffectType::EventEmit,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".receive",
        effect: EffectType::EventEmit,
    },
    // ── Cache ──
    Pattern {
        matcher: MatchTy::Contains,
        text: ".MGet",
        effect: EffectType::CacheRead,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".MSet",
        effect: EffectType::CacheWrite,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".HGet",
        effect: EffectType::CacheRead,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".HSet",
        effect: EffectType::CacheWrite,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".HGetAll",
        effect: EffectType::CacheRead,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Del(",
        effect: EffectType::CacheWrite,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Expire",
        effect: EffectType::CacheWrite,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Exists",
        effect: EffectType::CacheRead,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".TTL",
        effect: EffectType::CacheRead,
    },
    // ── File I/O ──
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Open",
        effect: EffectType::FileRead,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".ReadFile",
        effect: EffectType::FileRead,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".ReadAll",
        effect: EffectType::FileRead,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".WriteFile",
        effect: EffectType::FileWrite,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".WriteString",
        effect: EffectType::FileWrite,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Create",
        effect: EffectType::FileWrite,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Mkdir",
        effect: EffectType::FileWrite,
    },
    // ── Log ──
    Pattern {
        matcher: MatchTy::Contains,
        text: "logging.",
        effect: EffectType::Log,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: "logger.",
        effect: EffectType::Log,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Printf",
        effect: EffectType::Log,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Println",
        effect: EffectType::Log,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Infof",
        effect: EffectType::Log,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Info",
        effect: EffectType::Log,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Errorf",
        effect: EffectType::Log,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Error",
        effect: EffectType::Log,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Warnf",
        effect: EffectType::Log,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Warn",
        effect: EffectType::Log,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Debugf",
        effect: EffectType::Log,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Debug",
        effect: EffectType::Log,
    },
    // ── Generic fallbacks (no context — last resort) ──
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Set",
        effect: EffectType::CacheWrite,
    },
    Pattern {
        matcher: MatchTy::Contains,
        text: ".Get",
        effect: EffectType::SqlQuery,
    },
];

/// Phân loại effect của một call theo tên callee.
///
/// Trả về `(effect, pattern đã match)` — pattern dùng làm effect_desc.
pub fn classify_effect(call_name: &str) -> (EffectType, Option<&'static str>) {
    for p in PATTERNS {
        let hit = match p.matcher {
            MatchTy::Prefix => call_name.starts_with(p.text),
            MatchTy::Contains => call_name.contains(p.text),
        };
        if hit {
            return (p.effect, Some(p.text));
        }
    }
    (EffectType::None, None)
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
}
