use camino::{Utf8Path, Utf8PathBuf};
use codegraph_source::DiskSource;
use serde::Deserialize;
use std::fs;

use crate::languages::flows::FlowClassifier;
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
    /// Mỗi rule có thể khai báo `effect`, `marker`, hoặc cả hai (≥1 trong hai).
    /// Key `[[flow_rules]]` đã gộp vào `[[effect_rules]]` — key cũ bị bỏ qua +
    /// warn trong `load_from`.
    #[serde(default)]
    effect_rules: Vec<EffectRuleRaw>,
    /// Backend storage (mặc định sqlite).
    #[serde(default)]
    storage: StorageSection,
    /// Embedding backend cho semantic search (fastembed / hashing) + cache model.
    #[serde(default)]
    embedding: EmbeddingSection,
    /// Rerank `codegraph_context` bằng Jev (TypeSafe SystemOne) — opt-in.
    #[serde(default)]
    jev: JevSection,
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

/// Section `[jev]` — rerank `codegraph_context` bằng Jev (TypeSafe SystemOne).
///
/// OPT-IN: khi bật, query + tên symbol + đường dẫn file được gửi tới
/// `endpoint`. Token **không** nằm trong file này — chỉ đọc từ env var
/// `api_key_env`.
#[derive(Debug, Default, Deserialize)]
struct JevSection {
    /// Bật rerank. Thiếu key → tắt (fail-open về hành vi cũ).
    #[serde(default)]
    enabled: Option<bool>,
    /// URL đầy đủ của SystemOne endpoint.
    #[serde(default)]
    endpoint: Option<String>,
    /// Model id (`jev-1.13-free` mặc định).
    #[serde(default)]
    model: Option<String>,
    /// Tên env var chứa bearer token.
    #[serde(default)]
    api_key_env: Option<String>,
    /// Timeout request (ms) — quá hạn thì fail-open.
    #[serde(default)]
    timeout_ms: Option<u64>,
    /// Số candidate tối đa gửi cho Jev.
    #[serde(default)]
    max_candidates: Option<usize>,
    /// Xác suất tối thiểu của symbol hạng 1 để coi là khớp.
    #[serde(default)]
    min_probability: Option<f32>,
    /// Tên repo trong `state`; rỗng → lấy basename workspace root.
    #[serde(default)]
    repo: Option<String>,
    /// Đưa value của doc node vào mô tả option khi rerank doc search.
    #[serde(default)]
    doc_include_values: Option<bool>,
    /// Số ký tự tối đa của value đưa vào option.
    #[serde(default)]
    doc_value_chars: Option<usize>,
}

// ── Effect-rule runtime config (MCP `codegraph_config_effects` + GraphQL) ──
// Macro/call semantics do LLM khai báo lúc runtime: đọc/ghi `[[effect_rules]]`
// trong .codegraph/config.toml bằng toml_edit (giữ nguyên mọi section khác).
// Orchestrator install_current lại config MỖI lần index → rule mới áp dụng
// từ lần index kế tiếp (đồng thời install ngay cho thread hiện tại).
// Một rule có thể khai báo `effect`, `marker`, hoặc cả hai (≥1 trong hai);
// effect classifier và flow classifier được build từ cùng list, lọc riêng rẽ
// theo từng dimension (first-match-wins trên mỗi dimension).

/// Một rule trong file — dạng raw để list trung thực cả rule không hợp lệ.
#[derive(Debug, Clone)]
pub struct EffectRuleView {
    /// `contains` | `prefix` | `exact`.
    pub matcher: String,
    pub text: String,
    pub effect: String,
    pub marker: String,
    pub label: String,
    pub keep_call: String,
}

fn effect_config_path(root: &Utf8Path) -> Utf8PathBuf {
    root.join(".codegraph").join("config.toml")
}

/// Đọc `[[effect_rules]]` hiện có (raw — kể cả effect string không hợp lệ).
pub fn list_effect_rules(root: &Utf8Path) -> Vec<EffectRuleView> {
    let Ok(raw) = std::fs::read_to_string(effect_config_path(root)) else {
        return Vec::new();
    };
    let Ok(doc) = raw.parse::<toml_edit::DocumentMut>() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(arr) = doc.get("effect_rules").and_then(|i| i.as_array_of_tables()) {
        for t in arr.iter() {
            let (matcher, text) = t
                .get("call")
                .and_then(|c| c.as_table_like())
                .map(|c| {
                    for k in ["contains", "prefix", "exact"] {
                        if let Some(v) = c.get(k).and_then(|v| v.as_str()) {
                            return (k.to_string(), v.to_string());
                        }
                    }
                    (String::new(), String::new())
                })
                .unwrap_or_default();
            let effect = t
                .get("effect")
                .and_then(|e| e.as_str())
                .unwrap_or_default()
                .to_string();
            let marker = t
                .get("marker")
                .and_then(|e| e.as_str())
                .unwrap_or_default()
                .to_string();
            let label = t
                .get("label")
                .and_then(|e| e.as_str())
                .unwrap_or_default()
                .to_string();
            let keep_call = t
                .get("keep_call")
                .and_then(|e| e.as_bool())
                .map(|b| b.to_string())
                .unwrap_or_default();
            out.push(EffectRuleView {
                matcher,
                text,
                effect,
                marker,
                label,
                keep_call,
            });
        }
    }
    out
}

/// Thêm rule (append cuối — index order = thứ tự match) + áp dụng ngay cho
/// thread hiện tại. Rule config được xét TRƯỚC bảng default (first-match-wins
/// trên từng dimension).
pub struct NewEffectRule<'a> {
    pub matcher: &'a str,
    pub text: &'a str,
    pub effect: Option<&'a str>,
    pub marker: Option<&'a str>,
    pub label: Option<&'a str>,
    pub keep_call: Option<bool>,
}

pub fn add_effect_rule(root: &Utf8Path, rule: NewEffectRule<'_>) -> Result<(), String> {
    if !matches!(rule.matcher, "contains" | "prefix" | "exact") {
        return Err(format!(
            "matcher phải là contains|prefix|exact, nhận được {:?}",
            rule.matcher
        ));
    }
    if let Some(eff) = rule.effect {
        if codegraph_core::EffectType::parse(eff).is_none() {
            return Err(format!(
                "effect không hợp lệ {:?} — dùng snake_case: panic, sql_query, http_call, …",
                eff
            ));
        }
    }
    if let Some(mk) = rule.marker {
        if codegraph_core::FlowMarker::parse(mk).is_none() {
            return Err(format!(
                "marker không hợp lệ {:?} — dùng snake_case: try_start, try_end, catch_start, throw, if_true, if_false, branch_end, break, continue, loop, loop_back, return",
                mk
            ));
        }
    }
    if rule.effect.is_none() && rule.marker.is_none() {
        return Err("phải khai báo ít nhất một trong `effect` hoặc `marker`".into());
    }
    if rule.label.is_some()
        && !matches!(
            rule.marker,
            Some("throw") | Some("catch_start") | Some("if_true")
        )
    {
        return Err("label chỉ hợp lệ với marker = throw | catch_start | if_true".into());
    }
    if rule.keep_call.is_some() && rule.marker.is_none() {
        return Err("keep_call chỉ hợp lệ khi có marker".into());
    }
    if rule.text.trim().is_empty() {
        return Err("text matcher rỗng".into());
    }
    let path = effect_config_path(root);
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc = raw
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("config.toml không parse được: {e}"))?;
    if !doc.contains_key("effect_rules") {
        doc["effect_rules"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let arr = doc["effect_rules"]
        .as_array_of_tables_mut()
        .ok_or("effect_rules phải là array-of-tables")?;
    let mut call = toml_edit::Table::new();
    call.insert(rule.matcher, toml_edit::value(rule.text));
    let mut rule_table = toml_edit::Table::new();
    rule_table.insert("call", toml_edit::Item::Table(call));
    if let Some(eff) = rule.effect {
        rule_table.insert("effect", toml_edit::value(eff));
    }
    if let Some(mk) = rule.marker {
        rule_table.insert("marker", toml_edit::value(mk));
    }
    if let Some(l) = rule.label.filter(|l| !l.trim().is_empty()) {
        rule_table.insert("label", toml_edit::value(l));
    }
    if let Some(kc) = rule.keep_call {
        rule_table.insert("keep_call", toml_edit::value(kc));
    }
    arr.push(rule_table);
    std::fs::write(&path, doc.to_string()).map_err(|e| e.to_string())?;
    // Áp dụng ngay cho thread hiện tại (orchestrator tự reload mỗi index).
    let cfg = ExtractConfig::load(root);
    crate::languages::effects::install_current(Some(std::sync::Arc::new(cfg.effect_classifier)));
    crate::languages::flows::install_current(Some(std::sync::Arc::new(cfg.flow_classifier)));
    Ok(())
}

/// Xoá rule theo index (0-based, theo thứ tự `list_effect_rules`).
pub fn remove_effect_rule(root: &Utf8Path, index: usize) -> Result<bool, String> {
    let path = effect_config_path(root);
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut doc = raw
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("config.toml không parse được: {e}"))?;
    let arr = doc["effect_rules"]
        .as_array_of_tables_mut()
        .ok_or("config không có [[effect_rules]]")?;
    if index >= arr.len() {
        return Ok(false);
    }
    arr.remove(index);
    std::fs::write(&path, doc.to_string()).map_err(|e| e.to_string())?;
    let cfg = ExtractConfig::load(root);
    crate::languages::effects::install_current(Some(std::sync::Arc::new(cfg.effect_classifier)));
    crate::languages::flows::install_current(Some(std::sync::Arc::new(cfg.flow_classifier)));
    Ok(true)
}

/// Raw rule — `effect`/`marker` để string để rule lỗi (unknown) bị skip +
/// warn, không làm hỏng toàn bộ config. ≥1 trong hai phải có giá trị.
/// Parse lại bằng `EffectType::parse` / `FlowMarker::parse`.
#[derive(Debug, Deserialize)]
struct EffectRuleRaw {
    #[serde(rename = "call")]
    call: EffectCallPattern,
    #[serde(default)]
    effect: Option<String>,
    #[serde(default)]
    marker: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    keep_call: Option<bool>,
}

// Xoá `FlowRuleRaw` riêng — logic dùng chung gộp vào `EffectRuleRaw` (xem
// plan effect-rules-control-flow-upgrade.md).

/// Project-level extraction settings (`.codegraph/config.toml`).
#[derive(Debug, Clone, Default)]
pub struct ExtractConfig {
    pub header_language: HeaderLanguage,
    /// Classifier effect của project — config rules override bảng default.
    pub effect_classifier: EffectClassifier,
    /// Classifier flow (macro → marker) — KHÔNG có default: config trống →
    /// mọi call map `None`. LLM khai báo qua MCP `codegraph_config_effects`.
    pub flow_classifier: crate::languages::flows::FlowClassifier,
    /// Backend storage được chọn trong config (mặc định sqlite).
    pub storage: StorageConfig,
    /// Cấu hình embedding backend (semantic search) — đọc từ `[embedding]`.
    pub embedding: codegraph_graph::embeddings::EmbeddingConfig,
    /// Cấu hình Jev rerank cho `codegraph_context` — đọc từ `[jev]`.
    pub jev: codegraph_core::JevConfig,
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
        Self::load_from(&DiskSource::for_config(root.to_path_buf()))
    }

    /// Đọc `.codegraph/config.toml` qua Source layer (không còn `std::fs`).
    ///
    /// Nhận `&DiskSource` (cụ thể) vì đây là đường **sync**: `load` được gọi từ
    /// cả CLI lẫn rayon, mà `block_on` bên trong một tokio runtime đang chạy
    /// sẽ panic. Provider từ xa sẽ thêm biến thể `&dyn Source` ở đợt provider —
    /// lúc đó chỉ thêm hàm, không phải sửa lại call site nào ở đây.
    ///
    /// Mọi thất bại (thiếu file, không phải UTF-8, TOML hỏng) → `default`,
    /// đúng như hành vi `fs::read_to_string` + `toml::from_str` trước đây.
    pub fn load_from(source: &DiskSource) -> Self {
        let Ok(Some(bytes)) = source.read_config_blocking() else {
            return Self::default();
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return Self::default();
        };
        let Ok(file) = toml::from_str::<ConfigFile>(&text) else {
            return Self::default();
        };
        // Key `[[flow_rules]]` cũ đã gộp vào `[[effect_rules]]`.
        if text.contains("flow_rules") {
            tracing::warn!("`[[flow_rules]]` đã gộp vào `[[effect_rules]]` — rule cũ bị bỏ qua");
        }
        let (effect_classifier, flow_classifier) = build_classifiers(file.effect_rules);
        Self {
            header_language: parse_header_language(file.languages.headers.as_deref()),
            effect_classifier,
            flow_classifier,
            embedding: codegraph_graph::embeddings::EmbeddingConfig::from_raw(
                file.embedding.backend.as_deref(),
                file.embedding.model.as_deref(),
                file.embedding.cache_dir.as_deref(),
                file.embedding.vss_extension.as_deref(),
                file.embedding.execution_provider.as_deref(),
            ),
            jev: codegraph_core::JevConfig::from_raw(
                file.jev.enabled,
                file.jev.endpoint.as_deref(),
                file.jev.model.as_deref(),
                file.jev.api_key_env.as_deref(),
                file.jev.timeout_ms,
                file.jev.max_candidates,
                file.jev.min_probability,
                file.jev.repo.as_deref(),
                file.jev.doc_include_values,
                file.jev.doc_value_chars,
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
        // Cùng thời điểm: `[jev]` là config toàn cục cho `codegraph-context`
        // (module context không phụ thuộc `codegraph-extract`, nên truyền qua
        // global giống embedding).
        codegraph_core::set_jev_config(self.jev.clone());
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
        // Đường dẫn dùng chung const với lần **đọc** — cùng một quy ước, một
        // chỗ. (Phần đọc/ghi ở đây vẫn gọi `fs` trực tiếp: đây là chiều *ghi*.)
        let path = root.join(codegraph_source::CONFIG_REL_PATH);
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

/// Setup rule config → skip rule effect/marker unknown (warn) + giữ phần còn
/// lại. `effect`/`marker` là hai dimension độc lập: rule có effect sai vẫn
/// giữ marker đúng và ngược lại. Config trống → classifier rỗng (`classify_flow`
/// → `None`, `classify_effect` chỉ có bảng default).
fn build_classifiers(raw: Vec<EffectRuleRaw>) -> (EffectClassifier, FlowClassifier) {
    let mut effect_rules = Vec::with_capacity(raw.len());
    let mut flow_rules = Vec::with_capacity(raw.len());
    for r in raw {
        // ── effect dimension ──
        if let Some(s) = r.effect.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            match EffectType::parse(s) {
                Some(effect) => effect_rules.push(EffectRule {
                    call: r.call.clone(),
                    effect: Some(effect),
                    marker: None,
                    label: None,
                    keep_call: None,
                }),
                None => tracing::warn!(
                    "[[effect_rules]]: unknown effect `{s}` — effect dimension ignored"
                ),
            }
        }
        // ── marker dimension ──
        if let Some(s) = r.marker.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            match codegraph_core::FlowMarker::parse(s) {
                Some(marker) => flow_rules.push(EffectRule {
                    call: r.call,
                    effect: None,
                    marker: Some(marker),
                    label: r.label.clone(),
                    keep_call: r.keep_call,
                }),
                None => tracing::warn!(
                    "[[effect_rules]]: unknown marker `{s}` — marker dimension ignored"
                ),
            }
        } else if r.label.is_some() || r.keep_call.is_some() {
            tracing::warn!("[[effect_rules]]: label/keep_call without marker — ignored");
        }
    }
    (
        EffectClassifier::with_config(effect_rules),
        FlowClassifier::with_config(flow_rules),
    )
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
# Rule có thể khai báo `effect`, `marker`, hoặc cả hai (≥1 trong hai).
# call matchers: prefix / contains / exact.
# effect: sql_query, sql_write, cache_read, cache_write, http_call, event_emit,
#         file_read, file_write, log, panic (shadow default bằng "none").
# marker: try_start, try_end, catch_start, throw, if_true, if_false, branch_end,
#         break, continue, loop, loop_back, return.
# label (optional): exception / nhánh — chỉ với throw | catch_start | if_true.
# keep_call (optional): mặc định true cho throw, false cho các marker khác.
# Ví dụ PostgreSQL-style exception engine:
# [[effect_rules]]
# call = { exact = "PG_TRY" }
# marker = "try_start"
# [[effect_rules]]
# call = { exact = "PG_CATCH" }
# marker = "catch_start"
# [[effect_rules]]
# call = { exact = "PG_END_TRY" }
# marker = "try_end"
# [[effect_rules]]
# call = { contains = "ereport" }
# effect = "panic"
# marker = "throw"
# Ví dụ macro if/else/endif tự chế:
# [[effect_rules]]
# call = { exact = "IF" }
# marker = "if_true"
# label = "..."
# [[effect_rules]]
# call = { exact = "ELSE" }
# marker = "if_false"
# [[effect_rules]]
# call = { exact = "ENDIF" }
# marker = "branch_end"
# Ví dụ rule chỉ effect (như trước):
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

# [bingraph]
# Dataset riêng cho symbol binary (mặc định .codegraph/binary.sqlite) — tool
# callees/callers/flow route symbol binary sang dataset này theo khoảng id
# `bin_base`. KHÔNG đặt bin_base chồng lên dải docs (1e9/3e9) hoặc dải code
# index (< 1e9); đổi bin_base giữa chừng cần re-index binary.
# enabled = true
# bin_base = 2_000_000_000          # base id symbol binary (mặc định 2e9)
#
# Storage — mặc định dataset RIÊNG cùng backend kind của [storage].
# [bingraph.storage]
# type = "sqlite"
# dsn = "sqlite:///tmp/binary.db"

# [docgraph]
# Document graph — ingest tài liệu cấu trúc (HCL/Terraform, YAML, JSON, TOML)
# lúc `codegraph init`, truy vấn qua MCP (`codegraph_graphdoc_*`) hoặc `codegraph doc`.
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
///
/// Dùng `Source::list` (traversal chung của crate `codegraph-source`) thay vì
/// dựng `WalkBuilder` riêng — trước đây policy ignore bị copy y hệt ở 3 nơi.
pub fn detect_project_header_hint(source: &DiskSource) -> Option<HeaderLanguage> {
    let mut c_files = 0u32;
    let mut cpp_files = 0u32;

    let Ok(entries) = source.list_blocking() else {
        return None;
    };
    for entry in entries {
        let Some(ext) = entry.path.extension() else {
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

#[test]
fn effect_rule_crud_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let root = camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
    // init config mặc định
    crate::project::init_project(&root).unwrap();
    assert!(list_effect_rules(&root).is_empty());
    // add: CHECK macro → panic (exception-like)
    add_effect_rule(
        &root,
        NewEffectRule {
            matcher: "contains",
            text: "CHECK",
            effect: Some("panic"),
            marker: None,
            label: None,
            keep_call: None,
        },
    )
    .unwrap();
    add_effect_rule(
        &root,
        NewEffectRule {
            matcher: "prefix",
            text: "db.",
            effect: Some("sql_query"),
            marker: None,
            label: None,
            keep_call: None,
        },
    )
    .unwrap();
    let rules = list_effect_rules(&root);
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].matcher, "contains");
    assert_eq!(rules[0].text, "CHECK");
    assert_eq!(rules[0].effect, "panic");
    // rule thực sự hiệu lực với classifier load từ file
    let classifier = ExtractConfig::load(&root).effect_classifier;
    assert_eq!(classifier.classify("CHECK_OK").0, EffectType::Panic);
    assert_eq!(classifier.classify("db.Query").0, EffectType::SqlQuery);
    // invalidate: effect sai bị chặn, file không đổi
    assert!(add_effect_rule(
        &root,
        NewEffectRule {
            matcher: "contains",
            text: "x",
            effect: Some("nonsense"),
            marker: None,
            label: None,
            keep_call: None,
        },
    )
    .is_err());
    assert_eq!(list_effect_rules(&root).len(), 2);
    // remove theo index
    assert!(remove_effect_rule(&root, 0).unwrap());
    assert_eq!(list_effect_rules(&root).len(), 1);
    assert_eq!(list_effect_rules(&root)[0].text, "db.");
    assert!(!remove_effect_rule(&root, 9).unwrap());
    // các section khác của config vẫn nguyên (storage etc. parse được)
    let cfg = ExtractConfig::load(&root);
    assert_eq!(cfg.storage.kind, StorageKind::Sqlite);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Root tạm **có sẵn** `.codegraph/config.toml`.
    ///
    /// Trước đây test gọi `load_from(path)` với một path tuỳ ý; giờ config chỉ
    /// đọc được ở đường dẫn quy ước nên fixture dựng đúng hình dạng thật —
    /// không còn đường đọc config nào để bypass.
    struct CfgFixture {
        _dir: tempfile::TempDir,
        root: Utf8PathBuf,
        source: DiskSource,
    }

    impl CfgFixture {
        fn with_config(body: &str) -> Self {
            let this = Self::without_config();
            this.rewrite(body);
            this
        }

        /// Chỉ có thư mục `.codegraph/`, **không** có file — dùng để kiểm tra
        /// "thiếu config → default".
        fn without_config() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
            let cfg = root.join(codegraph_source::CONFIG_REL_PATH);
            std::fs::create_dir_all(cfg.parent().unwrap().as_std_path()).unwrap();
            Self {
                source: DiskSource::for_config(root.clone()),
                root,
                _dir: dir,
            }
        }

        /// Ghi đè nội dung config để test đổi kịch bản mà không dựng lại root.
        fn rewrite(&self, body: &str) {
            let p = self.root.join(codegraph_source::CONFIG_REL_PATH);
            std::fs::write(p.as_std_path(), body).unwrap();
        }

        fn root(&self) -> &Utf8Path {
            &self.root
        }

        fn load(&self) -> ExtractConfig {
            ExtractConfig::load_from(&self.source)
        }
    }

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

    /// `[jev]` parse đủ field; thiếu section → tắt hoàn toàn (opt-in).
    #[test]
    fn parse_jev_section() {
        let fx = CfgFixture::without_config();
        let cfg = fx.load();
        assert!(!cfg.jev.enabled, "thiếu [jev] → tắt");
        assert_eq!(cfg.jev.model, codegraph_core::jev::DEFAULT_MODEL);

        fx.rewrite(
            r#"
[jev]
enabled = true
model = "jev-1.13"
timeout_ms = 500
max_candidates = 12
min_probability = 0.42
repo = "codegraph-rs"
doc_include_values = true
doc_value_chars = 80
"#,
        );
        let jev = fx.load().jev;
        assert!(jev.enabled);
        assert_eq!(jev.model, "jev-1.13");
        assert_eq!(jev.timeout_ms, 500);
        assert_eq!(jev.max_candidates, 12);
        assert!((jev.min_probability - 0.42).abs() < 1e-6);
        assert!(jev.doc_include_values);
        assert_eq!(jev.doc_value_chars, 80);
        assert_eq!(jev.repo.as_deref(), Some("codegraph-rs"));
        assert_eq!(
            jev.endpoint,
            codegraph_core::jev::DEFAULT_ENDPOINT,
            "không khai báo endpoint → default"
        );
        assert_eq!(
            jev.api_key_env,
            codegraph_core::jev::DEFAULT_API_KEY_ENV,
            "token chỉ đọc từ env, không nằm trong config"
        );
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
        let fx = CfgFixture::with_config(
            r#"
[docgraph]
paths = ["*.tf", "sub/*.yaml", "nothing/:hcl"]

[docgraph.storage]
type = "sqlite"
dsn = "sqlite:///tmp/docs-test.db"
"#,
        );
        let root = fx.root();
        std::fs::write(root.join("a.tf"), "resource {}\n").unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("b.yaml"), "k: v\n").unwrap();

        let (doc_cfg, files) = fx.load().doc_config(root).expect("docgraph enabled");
        // Glob khớp đúng 2 file (pattern "nothing/" không có match); format
        // override ":hcl" không nhầm với phần mở rộng thường.
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|(p, _)| p.file_name() != Some("nothing")));
        // Storage override thắng default (không phải docs.sqlite của project).
        let storage = doc_cfg.storage.expect("storage config");
        assert_eq!(storage.dsn.as_deref(), Some("sqlite:///tmp/docs-test.db"));

        // Không `paths` → không ingest.
        fx.rewrite("[docgraph]\nenabled = true\n");
        assert!(fx.load().doc_config(root).is_none());

        // Không `[docgraph]` → dsn mặc định vẫn có (docs.sqlite cho sqlite).
        fx.rewrite("");
        let dsn = fx.load().doc_storage_dsn(root).unwrap();
        assert!(dsn.ends_with("docs.sqlite"), "got {dsn}");
    }

    /// `doc_storage_dsn` override bằng `[docgraph.storage] dsn` thắng kind.
    #[test]
    fn doc_storage_dsn_override() {
        let fx = CfgFixture::with_config(
            r#"
[storage]
type = "lmdb"

[docgraph.storage]
dsn = "sqlite:///tmp/custom-docs.db"
"#,
        );
        let cfg = fx.load();
        assert_eq!(
            cfg.doc_storage_dsn(Utf8Path::new("/repo")).unwrap(),
            "sqlite:///tmp/custom-docs.db"
        );

        // Không override → theo kind của [storage] (lmdb → docs.lmdb).
        fx.rewrite("[storage]\ntype = \"lmdb\"\n");
        let cfg = fx.load();
        let dsn = cfg.doc_storage_dsn(Utf8Path::new("/repo")).unwrap();
        assert!(
            dsn.starts_with("lmdb://") && dsn.ends_with("docs.lmdb"),
            "got {dsn}"
        );
    }

    /// `storage_dsn` dựng DSN theo kind; `dsn` override thắng.
    #[test]
    fn storage_dsn_built_or_overridden() {
        let fx = CfgFixture::with_config(
            r#"
[storage]
type = "lmdb"
"#,
        );
        let cfg = fx.load();
        let dsn = cfg.storage_dsn(Utf8Path::new("/repo")).unwrap();
        assert!(dsn.starts_with("lmdb://"), "got {dsn}");
        assert!(dsn.contains("/repo/.codegraph/db.lmdb"), "got {dsn}");

        // override dsn thắng kind.
        fx.rewrite(
            r#"
[storage]
type = "lmdb"
dsn = "sqlite:///tmp/custom.db"
"#,
        );
        let cfg = fx.load();
        assert_eq!(
            cfg.storage_dsn(Utf8Path::new("/repo")).unwrap(),
            "sqlite:///tmp/custom.db"
        );

        // memory → None (in-memory).
        fx.rewrite(
            r#"
[storage]
type = "memory"
"#,
        );
        assert!(fx.load().storage_dsn(Utf8Path::new("/repo")).is_none());
    }

    /// Parse từ file tạm với `[[effect_rules]]` → classifier áp dụng được.
    #[test]
    fn load_from_file_applies_effect_rules() {
        let fx = CfgFixture::with_config(
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
        );

        let cfg = fx.load();
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
    }

    /// Thiếu file config → `default`, không phải lỗi.
    #[test]
    fn config_thiếu_trả_default() {
        let fx = CfgFixture::without_config();
        let cfg = fx.load();
        assert_eq!(cfg.header_language, HeaderLanguage::Auto);
        assert_eq!(cfg.storage.kind, StorageKind::default());
    }

    /// CRUD: add → list → remove → list.
    #[test]
    fn effect_rule_crud() {
        let fx = CfgFixture::without_config();
        let root = fx.root();
        assert!(list_effect_rules(root).is_empty());

        add_effect_rule(
            root,
            NewEffectRule {
                matcher: "exact",
                text: "PG_TRY",
                effect: None,
                marker: Some("try_start"),
                label: None,
                keep_call: None,
            },
        )
        .unwrap();
        add_effect_rule(
            root,
            NewEffectRule {
                matcher: "contains",
                text: "ereport",
                effect: Some("panic"),
                marker: Some("throw"),
                label: Some("Error"),
                keep_call: Some(true),
            },
        )
        .unwrap();

        let rules = list_effect_rules(root);
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].matcher, "exact");
        assert_eq!(rules[0].text, "PG_TRY");
        assert_eq!(rules[0].marker, "try_start");
        assert_eq!(rules[1].matcher, "contains");
        assert_eq!(rules[1].effect, "panic");
        assert_eq!(rules[1].marker, "throw");
        assert_eq!(rules[1].label, "Error");
        assert_eq!(rules[1].keep_call, "true");

        assert!(remove_effect_rule(root, 0).unwrap());
        let rules = list_effect_rules(root);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].text, "ereport");

        assert!(!remove_effect_rule(root, 99).unwrap());
        assert_eq!(list_effect_rules(root).len(), 1);
    }

    /// Validation: phải có ≥1 trong effect/marker; label chỉ với throw|catch_start|if_true;
    /// keep_call chỉ với marker; effect/marker/matcher phải hợp lệ; text không rỗng.
    #[test]
    fn effect_rule_validation() {
        let fx = CfgFixture::without_config();
        let root = fx.root();

        let bad = |rule: NewEffectRule<'_>| assert!(add_effect_rule(root, rule).is_err());

        // Không có effect và marker → lỗi.
        bad(NewEffectRule {
            matcher: "exact",
            text: "x",
            effect: None,
            marker: None,
            label: None,
            keep_call: None,
        });
        // Label với marker không hợp lệ (branch_end) → lỗi.
        bad(NewEffectRule {
            matcher: "exact",
            text: "x",
            effect: None,
            marker: Some("branch_end"),
            label: Some("err"),
            keep_call: None,
        });
        // keep_call không có marker → lỗi.
        bad(NewEffectRule {
            matcher: "exact",
            text: "x",
            effect: None,
            marker: None,
            label: None,
            keep_call: Some(true),
        });
        // Effect không hợp lệ → lỗi.
        bad(NewEffectRule {
            matcher: "exact",
            text: "x",
            effect: Some("not_an_effect"),
            marker: None,
            label: None,
            keep_call: None,
        });
        // Marker không hợp lệ → lỗi.
        bad(NewEffectRule {
            matcher: "exact",
            text: "x",
            effect: None,
            marker: Some("not_a_marker"),
            label: None,
            keep_call: None,
        });
        // Matcher sai → lỗi.
        bad(NewEffectRule {
            matcher: "regex",
            text: "x",
            effect: Some("panic"),
            marker: None,
            label: None,
            keep_call: None,
        });
        // Text rỗng → lỗi.
        bad(NewEffectRule {
            matcher: "exact",
            text: "",
            effect: Some("panic"),
            marker: None,
            label: None,
            keep_call: None,
        });

        // Hợp lệ: chỉ effect.
        add_effect_rule(
            root,
            NewEffectRule {
                matcher: "exact",
                text: "panic",
                effect: Some("panic"),
                marker: None,
                label: None,
                keep_call: None,
            },
        )
        .unwrap();
        // Hợp lệ: chỉ marker.
        add_effect_rule(
            root,
            NewEffectRule {
                matcher: "exact",
                text: "PG_TRY",
                effect: None,
                marker: Some("try_start"),
                label: None,
                keep_call: None,
            },
        )
        .unwrap();
        assert_eq!(list_effect_rules(root).len(), 2);
    }

    /// Key cũ `[[flow_rules]]` bị bỏ qua (không đọc được như `[[effect_rules]]`).
    #[test]
    fn legacy_flow_rules_ignored() {
        let fx = CfgFixture::with_config(
            r#"
[[flow_rules]]
call = { exact = "PG_TRY" }
marker = "try_start"
"#,
        );
        assert!(list_effect_rules(fx.root()).is_empty());
    }
}
