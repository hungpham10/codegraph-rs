use crate::languages::effects::EffectClassifier;
use crate::project::{project_db_path, project_dir};
use camino::Utf8Path;
use codegraph_core::{EffectCallPattern, EffectRule, EffectType, StorageRoute};
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

/// Backend storage cho index — chọn backend trong `[storage]` của config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StorageKind {
    /// `sqlite://<path>` (backend mặc định).
    #[default]
    Sqlite,
    /// `lmdb://<path>` (thư mục).
    Lmdb,
    /// `redis://<url>` (cần `dsn`).
    Redis,
    /// In-memory — không persist.
    Memory,
    /// PostgreSQL — multi-tenant, partition theo `repo_id`.
    Postgres,
    /// MySQL — multi-tenant, partition theo `repo_id`.
    MySql,
}

impl StorageKind {
    fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "lmdb" => StorageKind::Lmdb,
            "redis" => StorageKind::Redis,
            "memory" | "in-memory" | "in_memory" => StorageKind::Memory,
            "postgres" | "postgresql" | "pg" => StorageKind::Postgres,
            "mysql" | "maria" | "mariadb" => StorageKind::MySql,
            _ => StorageKind::Sqlite,
        }
    }

    /// Backend này có phải RDBMS (Postgres/MySQL) hay không.
    pub fn is_rdbms(self) -> bool {
        matches!(self, StorageKind::Postgres | StorageKind::MySql)
    }
}

#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    languages: LanguagesSection,
    /// Project extra effect rules — xét trước bảng default (override).
    #[serde(default)]
    effect_rules: Vec<EffectRuleRaw>,
    /// Backend storage (mặc định sqlite).
    #[serde(default)]
    storage: StorageSection,
}

#[derive(Debug, Default, Deserialize)]
struct StorageSection {
    /// `"sqlite"`, `"lmdb"`, `"redis"`, `"memory"`, `"postgres"`, `"mysql"`.
    #[serde(default, rename = "type")]
    type_: Option<String>,
    /// DSN override — ví dụ `lmdb:///data/codegraph.db`.
    #[serde(default)]
    dsn: Option<String>,
    /// `repo_id` (u64) dùng làm partition key cho backend RDBMS
    /// (Postgres/MySQL, multi-tenant). Tự sinh bởi `codegraph init` nếu thiếu.
    #[serde(default)]
    repo_id: Option<u64>,
    /// Danh sách DSN shard cho backend RDBMS. Shard = `repo_id % len(dsns)`.
    #[serde(default)]
    dsns: Vec<String>,
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
    /// Backend storage được chọn trong config (mặc định sqlite).
    pub storage: StorageConfig,
}

/// Storage backend đã parse từ `[storage]` trong config.
#[derive(Debug, Clone, Default)]
pub struct StorageConfig {
    pub kind: StorageKind,
    /// DSN override (`None` = dựng từ `kind` + project path).
    pub dsn: Option<String>,
    /// `repo_id` (u64) — partition key cho backend RDBMS. `None` nếu chưa sinh
    /// (chỉ hợp lệ khi `kind` không phải RDBMS).
    pub repo_id: Option<u64>,
    /// Danh sách DSN shard cho backend RDBMS (shard = `repo_id % len`).
    pub dsns: Vec<String>,
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
            storage: StorageConfig {
                kind: file
                    .storage
                    .type_
                    .as_deref()
                    .map(StorageKind::parse)
                    .unwrap_or_default(),
                dsn: file.storage.dsn,
                repo_id: file.storage.repo_id,
                dsns: file.storage.dsns,
            },
        }
    }

    /// DSN hoàn chỉnh (kèm scheme) cho backend storage — dùng làm input trực
    /// tiếp cho `GraphIndex::open`. `None` = in-memory.
    ///
    /// - `dsn` trong config override → dùng nguyên văn.
    /// - Nếu không, dựng từ `kind`:
    ///   - sqlite → `sqlite://<root>/.codegraph/db.sqlite`
    ///   - lmdb   → `lmdb://<root>/.codegraph/db.lmdb` (thư mục)
    ///   - redis  → phải có `dsn` (không có default hợp lý)
    pub fn storage_dsn(&self, root: &Utf8Path) -> Option<String> {
        if let Some(dsn) = &self.storage.dsn {
            return Some(dsn.clone());
        }
        match self.storage.kind {
            StorageKind::Sqlite => Some(format!("sqlite://{}", project_db_path(root))),
            StorageKind::Lmdb => Some(format!("lmdb://{}", project_dir(root).join("db.lmdb"))),
            StorageKind::Redis => None,
            StorageKind::Memory => None,
            StorageKind::Postgres | StorageKind::MySql => None,
        }
    }

    /// `StorageRoute` mô tả cách mở index — thay thế cho `storage_dsn` khi
    /// backend có thể là RDBMS (multi-tenant + sharding).
    ///
    /// - `memory` → `Memory`
    /// - `sqlite` / `lmdb` / `redis` → `Local(dsn)`
    /// - `postgres` / `mysql` → `Sharded { dsns, repo_id, root }`
    ///   (`repo_id` phải đã được sinh bởi `ensure_repo_id`; nếu thiếu → `None`)
    pub fn storage_route(&self, root: &Utf8Path) -> Option<StorageRoute> {
        match self.storage.kind {
            StorageKind::Memory => Some(StorageRoute::Memory),
            StorageKind::Postgres | StorageKind::MySql => {
                let repo_id = self.storage.repo_id?;
                let dsns = if self.storage.dsns.is_empty() {
                    vec![self.storage.dsn.clone()?]
                } else {
                    self.storage.dsns.clone()
                };
                Some(StorageRoute::Sharded {
                    dsns,
                    repo_id: Some(repo_id),
                    root: Some(root.to_string()),
                })
            }
            StorageKind::Sqlite | StorageKind::Lmdb | StorageKind::Redis => {
                let dsn = self
                    .storage
                    .dsn
                    .clone()
                    .or_else(|| self.storage_dsn(root));
                Some(StorageRoute::Local(dsn?))
            }
        }
    }

    /// Sinh `repo_id` ngẫu nhiên (u64) nếu backend là RDBMS và config chưa có,
    /// rồi ghi vào `[storage]` của `config.toml` (self-heal). Trả `Some(repo_id)`
    /// nếu là RDBMS (kể cả khi đã có sẵn), `None` nếu không phải RDBMS.
    pub fn ensure_repo_id(root: &Utf8Path) -> Option<u64> {
        if !ExtractConfig::load(root).storage.kind.is_rdbms() {
            return None;
        }
        if let Some(id) = ExtractConfig::load(root).storage.repo_id {
            return Some(id);
        }
        let repo_id = {
            let mut buf = [0u8; 8];
            let _ = getrandom::getrandom(&mut buf);
            u64::from_le_bytes(buf)
        };
        let path = root.join(".codegraph").join("config.toml");
        if let Ok(text) = fs::read_to_string(path.as_std_path()) {
            let inserted = if let Some(idx) = text.find("[storage]") {
                let header = "[storage]";
                let mut s = String::with_capacity(text.len() + 40);
                s.push_str(&text[..idx]);
                s.push_str(header);
                s.push_str("\n# repo_id (partition key) — sinh bởi `codegraph init`.\n");
                s.push_str(&format!("repo_id = {repo_id}\n"));
                s.push_str(&text[idx + header.len()..]);
                s
            } else {
                format!("{text}\n[storage]\nrepo_id = {repo_id}\n")
            };
            let _ = fs::write(path.as_std_path(), inserted);
        }
        Some(repo_id)
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

# Critical effect rules — matched before the built-in defaults (first match wins).
# call matchers: prefix / contains / exact. Effects: sql_query, sql_write,
# cache_read, cache_write, http_call, event_emit, file_read, file_write, log.
# [[effect_rules]]
# call = { prefix = "db." }
# effect = "sql_query"

[storage]
# Backend lưu index: "sqlite", "lmdb", "redis", "memory", "postgres", hoặc "mysql".
type = "sqlite"
# DSN override (mặc định dựng từ `type` + project path):
#   sqlite → sqlite://<root>/.codegraph/db.sqlite
#   lmdb   → lmdb://<root>/.codegraph/db.lmdb
#   redis  → bắt buộc khai dsn, ví dụ redis://localhost:6379
#   postgres/mysql → bắt buộc khai `dsns` (hoặc `dsn` nếu 1 shard), ví dụ:
#     dsns = ["postgres://user:pass@db1:5432/codegraph", "postgres://user:pass@db2:5432/codegraph"]
#     repo_id = 14028493579208694412   # sinh bởi `codegraph init` (partition key)
# dsn = "sqlite:///tmp/codegraph.db"
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

    #[test]
    fn parse_storage_kind() {
        assert_eq!(StorageKind::parse("sqlite"), StorageKind::Sqlite);
        assert_eq!(StorageKind::parse("lmdb"), StorageKind::Lmdb);
        assert_eq!(StorageKind::parse("REDIS"), StorageKind::Redis);
        assert_eq!(StorageKind::parse("memory"), StorageKind::Memory);
        assert_eq!(StorageKind::parse("in-memory"), StorageKind::Memory);
        // unknown → sqlite (default).
        assert_eq!(StorageKind::parse("whatsapp"), StorageKind::Sqlite);
    }

    /// `storage_dsn` dựng DSN theo kind; `dsn` override thắng.
    #[test]
    fn storage_dsn_built_or_overridden() {
        let dir = std::env::temp_dir().join("codegraph-extract-dsn-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let path = Utf8Path::from_path(path.as_path()).unwrap();

        std::fs::write(
            path.as_std_path(),
            r#"
[storage]
type = "lmdb"
"#,
        )
        .unwrap();
        let cfg = ExtractConfig::load_from(path);
        let dsn = cfg.storage_dsn(Utf8Path::new("/repo")).unwrap();
        assert!(dsn.starts_with("lmdb://"), "got {dsn}");
        assert!(dsn.contains("/repo/.codegraph/db.lmdb"), "got {dsn}");

        // override dsn thắng kind.
        std::fs::write(
            path.as_std_path(),
            r#"
[storage]
type = "lmdb"
dsn = "sqlite:///tmp/custom.db"
"#,
        )
        .unwrap();
        let cfg = ExtractConfig::load_from(path);
        assert_eq!(
            cfg.storage_dsn(Utf8Path::new("/repo")).unwrap(),
            "sqlite:///tmp/custom.db"
        );

        // memory → None (in-memory).
        std::fs::write(
            path.as_std_path(),
            r#"
[storage]
type = "memory"
"#,
        )
        .unwrap();
        let cfg = ExtractConfig::load_from(path);
        assert!(cfg.storage_dsn(Utf8Path::new("/repo")).is_none());

        let _ = std::fs::remove_file(path.as_std_path());
        let _ = std::fs::remove_dir(&dir);
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
