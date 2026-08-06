use crate::languages::effects::EffectClassifier;
use camino::Utf8Path;
use codegraph_core::{EffectCallPattern, EffectRule, EffectType};
use serde::Deserialize;
use std::fs;

/// How `.h` header files should be parsed when both C and C++ extractors are available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HeaderLanguage {
    /// Detect from project layout and file content.
    #[default]
    Auto,
    C,
    Cpp,
}

#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    languages: LanguagesSection,
    /// Project extra effect rules — xét trước bảng default (override).
    #[serde(default)]
    effect_rules: Vec<EffectRuleRaw>,
}

#[derive(Debug, Default, Deserialize)]
struct LanguagesSection {
    /// `"auto"`, `"c"`, or `"cpp"`.
    #[serde(default)]
    headers: Option<String>,
}

/// Raw rule — `effect` để string để rule lỗi (unknown) bị skip + warn, không
/// làm hỏng toàn bộ config; parse lại bằng `EffectType::parse`.
#[derive(Debug, Deserialize)]
struct EffectRuleRaw {
    #[serde(rename = "call")]
    call: EffectCallPattern,
    effect: String,
}

/// Project-level extraction settings (`.codegraph/config.toml`).
#[derive(Debug, Clone, Default)]
pub struct ExtractConfig {
    pub header_language: HeaderLanguage,
    /// Classifier effect của project — config rules override bảng default.
    pub effect_classifier: EffectClassifier,
}

impl ExtractConfig {
    pub fn load(root: &Utf8Path) -> Self {
        let path = root.join(".codegraph").join("config.toml");
        Self::load_from(&path)
    }

    pub fn load_from(path: &Utf8Path) -> Self {
        let Ok(text) = fs::read_to_string(path.as_std_path()) else {
            return Self::default();
        };
        let Ok(file) = toml::from_str::<ConfigFile>(&text) else {
            return Self::default();
        };
        Self {
            header_language: parse_header_language(file.languages.headers.as_deref()),
            effect_classifier: build_classifier(file.effect_rules),
        }
    }
}

/// Setup rule config → skip rule effect unknown (warn) + giữ phần còn lại.
fn build_classifier(raw: Vec<EffectRuleRaw>) -> EffectClassifier {
    let mut rules = Vec::with_capacity(raw.len());
    for r in raw {
        let Some(effect) = EffectType::parse(&r.effect) else {
            tracing::warn!(
                "[[effect_rules]]: unknown effect `{}`, rule ignored",
                r.effect
            );
            continue;
        };
        rules.push(EffectRule {
            call: r.call,
            effect,
        });
    }
    EffectClassifier::with_config(rules)
}

fn parse_header_language(raw: Option<&str>) -> HeaderLanguage {
    match raw.unwrap_or("auto").trim().to_ascii_lowercase().as_str() {
        "c" => HeaderLanguage::C,
        "cpp" | "c++" | "cxx" => HeaderLanguage::Cpp,
        _ => HeaderLanguage::Auto,
    }
}

/// Default `config.toml` written on `codegraph init`.
pub const DEFAULT_CONFIG_TOML: &str = r#"# CodeGraph project configuration
# See https://github.com/Cleboost/codegraph-rs

[languages]
# How to parse .h header files: "auto", "c", or "cpp".
# "auto" detects C++ projects from .cpp/.hpp files and C++ syntax in headers.
headers = "auto"

# Project effect rules — matched before the built-in defaults (first match wins).
# call matchers: prefix / contains / exact. Effects: sql_query, sql_write,
# cache_read, cache_write, http_call, event_emit, file_read, file_write, log.
# [[effect_rules]]
# call = { prefix = "db." }
# effect = "sql_query"
"#;

/// Quick project scan: returns a hint when the tree is clearly C-only or C++-only.
pub fn detect_project_header_hint(root: &Utf8Path) -> Option<HeaderLanguage> {
    let mut c_files = 0u32;
    let mut cpp_files = 0u32;

    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .parents(true)
        .add_custom_ignore_filename(".codegraphignore")
        .build();

    for entry in walker.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let Some(ext) = entry.path().extension().and_then(|s| s.to_str()) else {
            continue;
        };
        match ext {
            "c" => c_files += 1,
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => cpp_files += 1,
            _ => {}
        }
    }

    if cpp_files > 0 && c_files == 0 {
        Some(HeaderLanguage::Cpp)
    } else if c_files > 0 && cpp_files == 0 {
        Some(HeaderLanguage::C)
    } else {
        None
    }
}

/// Heuristic: does this header look like C++ from its source text?
pub fn is_cpp_header(source: &str) -> bool {
    let sample = &source[..source.len().min(8192)];
    const MARKERS: &[&str] = &[
        "namespace ",
        "class ",
        "template ",
        "typename ",
        "constexpr ",
        "noexcept",
        "public:",
        "private:",
        "protected:",
        "operator ",
        "std::",
        "extern \"C\"",
        "using ",
        "::",
    ];
    MARKERS.iter().any(|m| sample.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config_headers() {
        let cfg = toml::from_str::<ConfigFile>(
            r#"
[languages]
headers = "cpp"
"#,
        )
        .unwrap();
        assert_eq!(
            parse_header_language(cfg.languages.headers.as_deref()),
            HeaderLanguage::Cpp
        );
    }

    #[test]
    fn sniff_cpp_header() {
        assert!(is_cpp_header(
            "#pragma once\nnamespace tnl { class String {}; }\n"
        ));
        assert!(!is_cpp_header(
            "#ifndef FOO_H\n#define FOO_H\nstruct foo { int x; };\n#endif\n"
        ));
    }

    /// Parse từ file tạm với `[[effect_rules]]` → classifier áp dụng được.
    #[test]
    fn load_from_file_applies_effect_rules() {
        let dir = std::env::temp_dir().join("codegraph-extract-cfg-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let path = Utf8Path::from_path(path.as_path()).unwrap();
        std::fs::write(
            path.as_std_path(),
            r#"
[languages]
headers = "cpp"

[[effect_rules]]
call = { prefix = "db." }
effect = "sql_query"

[[effect_rules]]
call = { exact = "sendEmail" }
effect = "event_emit"

[[effect_rules]]
call = { contains = "legacy-" }
effect = "not_a_real_effect"
"#,
        )
        .unwrap();

        let cfg = ExtractConfig::load_from(path);
        assert_eq!(cfg.header_language, HeaderLanguage::Cpp);
        // Rule config xét trước default: "db.Exec" → SqlQuery (không phải
        // SqlWrite như default ".Exec").
        let (effect, desc) = cfg.effect_classifier.classify("db.Exec");
        assert_eq!(effect, codegraph_core::EffectType::SqlQuery);
        assert_eq!(desc, Some("db."));
        assert_eq!(
            cfg.effect_classifier.classify("sendEmail").0,
            codegraph_core::EffectType::EventEmit
        );
        // Rule có effect unknown bị skip → "legacy-" không match, rơi về default.
        assert_eq!(
            cfg.effect_classifier.classify("legacy-writer").0,
            codegraph_core::EffectType::None
        );

        let _ = std::fs::remove_file(path.as_std_path());
        let _ = std::fs::remove_dir(&dir);
    }
}
