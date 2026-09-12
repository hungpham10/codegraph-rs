use camino::{Utf8Path, Utf8PathBuf};
use serde::Deserialize;
use std::fs;

#[cfg(feature = "binary")]
use codegraph_binary::BinaryConfig;
use codegraph_core::{EffectCallPattern, EffectRule, EffectType, StorageRoute};

use crate::languages::effects::EffectClassifier;
use crate::project::{project_db_path, project_dir};

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
    /// Embedding backend cho semantic search (fastembed / hashing) + cache model.
    #[serde(default)]
    embedding: EmbeddingSection,
    /// Document graph — ingest tài liệu cấu trúc lúc `codegraph init`.
    #[serde(default)]
    docgraph: DocGraphSection,
    /// Binary graph — dataset riêng cho symbol binary (`[bingraph]`).
    #[cfg(feature = "binary")]
    #[serde(default)]
    bingraph: BinGraphSection,

    /// Phân tích binary (radare2) — feature `binary`.
    #[cfg(feature = "binary")]
    #[serde(default)]
    binary: Option<codegraph_binary::BinaryConfig>,
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

/// Section `[docgraph]` — cấu hình document graph (ingest tài liệu lúc `init`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DocGraphSection {
    /// Bật ingest docs khi `codegraph init` (mặc định bật khi có `paths`).
    #[serde(default)]
    enabled: Option<bool>,
    /// Danh sách glob (tính từ project root), vd `["infra/*.tf", "config/**/*.yaml"]`.
    /// Mỗi entry hỗ trợ suffix `:<format>` để override, vd `"deploy/README:hcl"`.
    #[serde(default)]
    paths: Vec<String>,
    /// Override storage cho docs — mặc định dataset riêng cùng backend kind của
    /// `[storage]` (sqlite → `.codegraph/docs.sqlite`, lmdb → `docs.lmdb`).
    #[serde(default)]
    storage: Option<DocGraphStorageSection>,
    /// Base id cho node/doc của document graph (mặc định 1e9).
    #[serde(default)]
    doc_base: Option<u64>,
    /// Base id cho mined pattern (mặc định 3e9).
    #[serde(default)]
    pattern_base: Option<u64>,
    /// Bloom-filter cap (mặc định 64).
    #[serde(default)]
    bloom_cap: Option<usize>,
    /// Alias chuẩn hoá key, vd `aliases = [["instances", "replicas"]]`.
    #[serde(default)]
    aliases: Vec<(String, String)>,
}

/// `[docgraph.storage]` — override backend/dsn cho document graph.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DocGraphStorageSection {
    /// `"sqlite"`, `"lmdb"`, `"redis"`, `"memory"`.
    #[serde(default, rename = "type")]
    pub type_: Option<String>,
    /// DSN override (vd `sqlite:///tmp/docs.db`).
    #[serde(default)]
    pub dsn: Option<String>,
}

/// Section `[bingraph]` — cấu hình binary graph: dataset riêng (mặc định
/// `.codegraph/binary.sqlite`) cho symbol binary, tách khỏi code index và
/// docs để query search/list chạy lazy trên SQL index không phải rebuild RAM.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BinGraphSection {
    /// Bật binary graph (mặc định bật).
    #[serde(default)]
    enabled: Option<bool>,
    /// Override storage — hiện chỉ hỗ trợ sqlite; backend khác → in-memory + warn.
    #[serde(default)]
    storage: Option<DocGraphStorageSection>,
    /// Base id cho symbol binary graph (mặc định 2e9 — không đụng dải docs
    /// 1e9/3e9 và dải code index).
    #[serde(default)]
    bin_base: Option<u64>,
}

impl BinGraphSection {
    /// Binary graph có bật hay không (mặc định bật).
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

impl DocGraphSection {
    /// Ingest docs có bật hay không: `enabled` override, mặc định = có `paths`.
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(!self.paths.is_empty())
    }
}

#[derive(Debug, Default, Deserialize)]
struct EmbeddingSection {
    /// `"fastembed"` | `"hashing"`.
    #[serde(default)]
    backend: Option<String>,
    /// Tên model fastembed (alias hoặc variant name).
    #[serde(default)]
    model: Option<String>,
    /// Thư mục cache model (global). Mặc định `~/.cache/codegraph/embeddings`.
    #[serde(default)]
    cache_dir: Option<String>,
    /// Thư mục chứa extension sqlite-vss (`vector0`/`vss0`) — CHỈ backend SQLite.
    /// Khi set (và file tồn tại), KNN semantic chạy qua `vss0` (HNSW ANN trong
    /// SQLite). Thiếu file → fallback brute-force. `None` → tự dò `<cache_dir>/vss`.
    #[serde(default)]
    vss_extension: Option<String>,
    /// Execution provider cho ONNX Runtime — `"cpu"` (mặc định) | `"coreml"`
    /// (Apple Neural Engine / GPU, macOS) | `"metal"` (GPU, macOS). Chỉ có hiệu
    /// lực khi build `--features fastembed,apple-accel` trên macOS; ngược lại bỏ
    /// qua (chạy CPU). Platform khác macOS luôn CPU.
    #[serde(default)]
    execution_provider: Option<String>,
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
    /// Cấu hình embedding backend (semantic search) — đọc từ `[embedding]`.
    pub embedding: codegraph_graph::embeddings::EmbeddingConfig,
    /// Cấu hình document graph — đọc từ `[docgraph]`.
    pub docgraph: DocGraphSection,
    /// Cấu hình binary graph — đọc từ `[bingraph]`.
    #[cfg(feature = "binary")]
    pub bingraph: BinGraphSection,
    /// Cấu hình phân tích binary (radare2).
    #[cfg(feature = "binary")]
    pub binary: BinaryConfig,
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

/// Một file docs khớp glob `[docgraph] paths`: path + format override
/// (từ suffix `:<format>` của entry, nếu có).
pub type DocFile = (Utf8PathBuf, Option<String>);

/// Config document graph + danh sách file docs cần ingest lúc `codegraph init`.
pub type DocFiles = (codegraph_docs::DocConfig, Vec<DocFile>);

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
            embedding: codegraph_graph::embeddings::EmbeddingConfig::from_raw(
                file.embedding.backend.as_deref(),
                file.embedding.model.as_deref(),
                file.embedding.cache_dir.as_deref(),
                file.embedding.vss_extension.as_deref(),
                file.embedding.execution_provider.as_deref(),
            ),
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
            docgraph: file.docgraph,
            #[cfg(feature = "binary")]
            bingraph: file.bingraph,
            #[cfg(feature = "binary")]
            binary: file.binary.unwrap_or_default(),
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
        // Áp dụng config embedding (backend/model/cache) cho process trước khi
        // mở index — `GraphIndex::new_with_storage` đọc global này để quyết định
        // có bật vector index hay không (opt-in: chỉ khi backend = "fastembed").
        codegraph_graph::embeddings::set_embedding_config(self.embedding.clone());
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
                let dsn = self.storage.dsn.clone().or_else(|| self.storage_dsn(root));
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

    /// DSN dataset **riêng** cho document graph (tries của docs đụng namespace
    /// record/shard với code index nên KHÔNG dùng chung 1 dataset được — dùng
    /// cùng backend kind nhưng file/keyspace riêng).
    ///
    /// - `[docgraph.storage] dsn` override → dùng nguyên văn.
    /// - Mặc định theo backend kind (override được bằng `[docgraph.storage] type`):
    ///   - sqlite → `sqlite://<root>/.codegraph/docs.sqlite`
    ///   - lmdb   → `lmdb://<root>/.codegraph/docs.lmdb`
    ///   - redis  → DSN của `[storage]` (helper mở keyspace prefix riêng)
    ///   - memory → `None` (in-memory)
    ///   - postgres/mysql → chưa hỗ trợ dataset riêng → `None`
    pub fn doc_storage_dsn(&self, root: &Utf8Path) -> Option<String> {
        if let Some(dsn) = self
            .docgraph
            .storage
            .as_ref()
            .and_then(|s| s.dsn.as_deref())
        {
            return Some(dsn.to_string());
        }
        let kind = self
            .docgraph
            .storage
            .as_ref()
            .and_then(|s| s.type_.as_deref())
            .map(StorageKind::parse)
            .unwrap_or(self.storage.kind);
        match kind {
            StorageKind::Sqlite => Some(format!(
                "sqlite://{}",
                project_dir(root).join("docs.sqlite")
            )),
            StorageKind::Lmdb => Some(format!("lmdb://{}", project_dir(root).join("docs.lmdb"))),
            StorageKind::Redis => self.storage.dsn.clone(),
            StorageKind::Memory | StorageKind::Postgres | StorageKind::MySql => None,
        }
    }

    /// DSN dataset **riêng** cho binary graph (`[bingraph]`) — dataset chạy trên
    /// trait `Storage` nên hỗ trợ mọi backend local/remote:
    /// - `[bingraph.storage] dsn` override → dùng nguyên văn.
    /// - Mặc định theo backend kind (override được bằng `[bingraph.storage] type`):
    ///   - sqlite → `sqlite://<root>/.codegraph/binary.sqlite`
    ///   - lmdb   → `lmdb://<root>/.codegraph/binary.lmdb`
    ///   - redis  → DSN của `[storage]` (keyspace `codegraph:binary`)
    ///   - memory / RDBMS → `None` (in-memory + warn ở caller)
    #[cfg(feature = "binary")]
    pub fn bingraph_dsn(&self, root: &Utf8Path) -> Option<String> {
        if let Some(dsn) = self
            .bingraph
            .storage
            .as_ref()
            .and_then(|s| s.dsn.as_deref())
        {
            return Some(dsn.to_string());
        }
        let kind = self
            .bingraph
            .storage
            .as_ref()
            .and_then(|s| s.type_.as_deref())
            .map(StorageKind::parse)
            .unwrap_or(self.storage.kind);
        match kind {
            StorageKind::Sqlite => Some(format!(
                "sqlite://{}",
                project_dir(root).join("binary.sqlite")
            )),
            StorageKind::Lmdb => Some(format!("lmdb://{}", project_dir(root).join("binary.lmdb"))),
            StorageKind::Redis => self.storage.dsn.clone(),
            _ => None,
        }
    }

    /// Base id cho symbol binary graph (mặc định 2e9).
    #[cfg(feature = "binary")]
    pub fn bin_base(&self) -> u64 {
        self.bingraph.bin_base.unwrap_or(2_000_000_000)
    }

    /// Config document graph + danh sách file khớp glob `[docgraph] paths`
    /// (path kèm format override). Trả `None` khi `[docgraph]` không bật /
    /// không khai báo `paths`.
    pub fn doc_config(&self, root: &Utf8Path) -> Option<DocFiles> {
        if !self.docgraph.is_enabled() {
            return None;
        }
        let mut files: Vec<DocFile> = Vec::new();
        for entry in &self.docgraph.paths {
            let (pattern, format) = split_format_override(entry);
            let full = root.join(pattern).to_string();
            let Ok(matches) = glob::glob(&full) else {
                tracing::warn!("[docgraph] glob `{pattern}` không hợp lệ — bỏ qua");
                continue;
            };
            for path in matches.flatten() {
                if !path.is_file() {
                    continue;
                }
                let Ok(path) = Utf8PathBuf::from_path_buf(path) else {
                    tracing::warn!("[docgraph] path không phải UTF-8 — bỏ qua");
                    continue;
                };
                if !files.iter().any(|(p, _)| *p == path) {
                    files.push((path, format.map(str::to_string)));
                }
            }
        }
        // Không có file nào khớp → coi như không cấu hình (init bỏ qua ingest).
        if files.is_empty() {
            return None;
        }
        let dsn = self.doc_storage_dsn(root);
        if dsn.is_none() && self.storage.kind.is_rdbms() {
            tracing::warn!(
                "[docgraph] backend RDBMS chưa hỗ trợ dataset riêng cho docs — \
                 dùng in-memory (override bằng [docgraph.storage] dsn)"
            );
        }
        let config = codegraph_docs::DocConfig {
            storage: dsn.map(|dsn| codegraph_docs::StorageConfig {
                r#type: Some(dsn.split("://").next().unwrap_or("sqlite").to_string()),
                dsn: Some(dsn),
            }),
            doc_base: self.docgraph.doc_base,
            pattern_base: self.docgraph.pattern_base,
            bloom_cap: self.docgraph.bloom_cap,
            aliases: (!self.docgraph.aliases.is_empty()).then(|| self.docgraph.aliases.clone()),
        };
        Some((config, files))
    }
}

/// Tách suffix `:<format>` khỏi một entry `[docgraph] paths` (chỉ nhận format
/// đã biết để không nhầm với ký tự `:` khác trong pattern).
fn split_format_override(entry: &str) -> (&str, Option<&str>) {
    const FORMATS: [&str; 8] = ["hcl", "tf", "yaml", "yml", "json", "toml", "nginx", "conf"];
    if let Some((pattern, format)) = entry.rsplit_once(':') {
        if FORMATS.contains(&format.to_ascii_lowercase().as_str()) {
            return (pattern, Some(format));
        }
    }
    (entry, None)
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

[embedding]
# Semantic search (vector KNN/k-means) là OPT-IN — MẶC ĐỊNH TẮT.
# Bỏ comment + set "fastembed" để bật vector search (cần compile `--features fastembed`
# và tải model ONNX lúc chạy). Nếu tắt, semantic/hybrid search sẽ báo lỗi rõ ràng
# (KHÔNG fallback silent sang lexical).
# backend = "fastembed"
# Model fastembed — alias thân thiện hoặc variant name, VD:
#   bge-small-en-v1.5 (mặc định, 384-dim), bge-base-en-v1.5, bge-large-en-v1.5,
#   all-minilm-l6-v2, all-mpnet-base-v2, nomic-embed-text-v1.5, multilingual-e5-small.
# model = "bge-small-en-v1.5"
# Thư mục cache model (global, chia sẻ mọi project) — pre-download bằng
# `codegraph embed --model <x>` để chạy offline. Mặc định ~/.cache/codegraph/embeddings.
# cache_dir = "~/.cache/codegraph/embeddings"
# SQLite-only: dùng sqlite-vss (vector0/vss0) để KNN chạy HNSW ANN ngay trong
# SQLite thay vì brute-force in-memory. Cần 2 file extension trong thư mục này
# (tự build hoặc tải prebuilt). Có mặt → bật; thiếu → fallback brute-force.
# vss_extension = "~/.cache/codegraph/embeddings/vss"
# Execution provider cho ONNX Runtime (chỉ macOS, build `--features fastembed,apple-accel`):
#   "cpu"     (mặc định)  → CPU + Accelerate/vecLib SIMD, mọi core
#   "coreml"  → Core ML EP (Apple Neural Engine / GPU trên Apple Silicon)
#   "metal"   → Metal EP (GPU)
# Build thiếu `apple-accel`, hoặc platform khác macOS → bỏ qua, chạy CPU.
# execution_provider = "cpu"

[binary]
# Phân tích binary (ELF/Mach-O/PE) bằng radare2 — yêu cầu `r2` trong PATH.
# `codegraph doctor` kiểm tra sự có mặt của r2.
# enabled = true        # bỏ comment để bật
# depth = "aaa"         # "aaa" (full) hoặc "fast" (af; aar; aac — nhanh hơn cho binary lớn)
# cfg_markers = true    # xây marker IF/LOOP/SWITCH từ CFG của mỗi function
# cache = true          # cache kết quả phân tích theo (path, mtime, size)

# [docgraph]
# Document graph — ingest tài liệu cấu trúc (HCL/Terraform, YAML, JSON, TOML)
# lúc `codegraph init`, truy vấn qua MCP (`codegraph_doc_*`) hoặc `codegraph doc`.
# Bỏ comment section + `paths` để bật:
# [docgraph]
# enabled = true                      # mặc định bật khi có `paths`
# Glob tính từ project root; suffix `:<format>` override format theo entry.
# paths = ["infra/*.tf", "deploy/*.yaml", "config/settings.toml"]
#
# Storage cho docs — mặc định dataset RIÊNG cùng backend kind của [storage]
# (sqlite → .codegraph/docs.sqlite, lmdb → docs.lmdb, redis → keyspace riêng).
# [docgraph.storage]
# type = "sqlite"
# dsn = "sqlite:///tmp/docs.db"
#
# doc_base = 1_000_000_000            # base id node/doc (mặc định 1e9)
# pattern_base = 3_000_000_000        # base id mined pattern (mặc định 3e9)
# bloom_cap = 64                      # bloom-filter cap cho doc search
# aliases = [["instances", "replicas"]]  # chuẩn hoá key khi tra cứu
"#;

/// Default `config.toml` section `[binary]` (ghi chú, thêm bởi `codegraph init`).
pub const BINARY_CONFIG_NOTE: &str = r#"
[binary]
# Phân tích binary (ELF/Mach-O/PE) bằng radare2 — yêu cầu `r2` trong PATH.
# `codegraph doctor` kiểm tra sự có mặt của r2.
# enabled = true        # bỏ comment để bật
# depth = "aaa"         # "aaa" (full) hoặc "fast" (af; aar; aac — nhanh hơn cho binary lớn)
# cfg_markers = true    # xây marker IF/LOOP/SWITCH từ CFG của mỗi function
# cache = true          # cache kết quả phân tích theo (path, mtime, size)
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

    /// Parse `[docgraph]` — glob mở rộng, format override theo entry, storage
    /// override; không khai báo `paths` → `doc_config` trả `None`.
    #[test]
    fn docgraph_parse_and_glob() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.tf"), "resource {}\n").unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("b.yaml"), "k: v\n").unwrap();
        let cfg_path = root.join("config.toml");
        let cfg_path = Utf8Path::from_path(&cfg_path).unwrap();
        std::fs::write(
            cfg_path.as_std_path(),
            r#"
[docgraph]
paths = ["*.tf", "sub/*.yaml", "nothing/:hcl"]

[docgraph.storage]
type = "sqlite"
dsn = "sqlite:///tmp/docs-test.db"
"#,
        )
        .unwrap();
        let root = Utf8Path::from_path(root).unwrap();
        let cfg = ExtractConfig::load_from(cfg_path);
        let (doc_cfg, files) = cfg.doc_config(root).expect("docgraph enabled");
        // Glob khớp đúng 2 file (pattern "nothing/" không có match); format
        // override ":hcl" không nhầm với phần mở rộng thường.
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|(p, _)| p.file_name() != Some("nothing")));
        // Storage override thắng default (không phải docs.sqlite của project).
        let storage = doc_cfg.storage.expect("storage config");
        assert_eq!(storage.dsn.as_deref(), Some("sqlite:///tmp/docs-test.db"));

        // Không `paths` → không ingest.
        std::fs::write(cfg_path.as_std_path(), "[docgraph]\nenabled = true\n").unwrap();
        let cfg = ExtractConfig::load_from(cfg_path);
        assert!(cfg.doc_config(root).is_none());

        // Không `[docgraph]` → dsn mặc định vẫn có (docs.sqlite cho sqlite).
        std::fs::write(cfg_path.as_std_path(), "").unwrap();
        let cfg = ExtractConfig::load_from(cfg_path);
        let dsn = cfg.doc_storage_dsn(root).unwrap();
        assert!(dsn.ends_with("docs.sqlite"), "got {dsn}");
    }

    /// `doc_storage_dsn` override bằng `[docgraph.storage] dsn` thắng kind.
    #[test]
    fn doc_storage_dsn_override() {
        let dir = std::env::temp_dir().join("codegraph-extract-docdsn-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let path = Utf8Path::from_path(path.as_path()).unwrap();
        std::fs::write(
            path.as_std_path(),
            r#"
[storage]
type = "lmdb"

[docgraph.storage]
dsn = "sqlite:///tmp/custom-docs.db"
"#,
        )
        .unwrap();
        let cfg = ExtractConfig::load_from(path);
        assert_eq!(
            cfg.doc_storage_dsn(Utf8Path::new("/repo")).unwrap(),
            "sqlite:///tmp/custom-docs.db"
        );

        // Không override → theo kind của [storage] (lmdb → docs.lmdb).
        std::fs::write(path.as_std_path(), "[storage]\ntype = \"lmdb\"\n").unwrap();
        let cfg = ExtractConfig::load_from(path);
        let dsn = cfg.doc_storage_dsn(Utf8Path::new("/repo")).unwrap();
        assert!(
            dsn.starts_with("lmdb://") && dsn.ends_with("docs.lmdb"),
            "got {dsn}"
        );

        let _ = std::fs::remove_file(path.as_std_path());
        let _ = std::fs::remove_dir(&dir);
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
