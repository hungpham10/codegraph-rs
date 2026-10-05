//! `SourceKind` — dịch vụ nguồn phục vụ, và `SourceConfig` — cấu hình theo tên.

use camino::Utf8PathBuf;

/// Dịch vụ mà một Source phục vụ. Quyết định cách diễn giải `include` và
/// payload cần đọc — mỗi dịch vụ tự chọn source của mình (code → `Code`,
/// document → `Document`, binary → `Binary`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    /// Source code — lọc theo extension, đọc cả file (UTF-8) để tree-sitter.
    Code,
    /// Binary — không lọc theo tên, đọc **4 byte đầu** để check magic bytes,
    /// rồi `materialize` để radare2 mở.
    Binary,
    /// Document có cấu trúc (YAML/JSON/TOML/HCL) — lọc theo glob, đọc cả
    /// file (UTF-8 bắt buộc) để `DocParser` dựng node graph.
    Document,
}

impl SourceKind {
    /// Tên ổn định để ghi vào log / config (kebab-case).
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Code => "code",
            SourceKind::Binary => "binary",
            SourceKind::Document => "document",
        }
    }

    /// Parse từ tên trong config. `None` nếu không hợp lệ.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "code" => Some(SourceKind::Code),
            "binary" => Some(SourceKind::Binary),
            "document" => Some(SourceKind::Document),
            _ => None,
        }
    }
}

impl std::fmt::Display for SourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Cấu hình một source — ánh xạ từ `[source.<name>]` trong `.codegraph/config.toml`.
///
/// ```toml
/// [source.repo]
/// kind = "code"          # extension
/// root = "."
/// include = ["rs", "go"]
///
/// [source.docs]
/// kind = "document"      # glob
/// root = "docs"
/// include = ["**/*.json"]
///
/// [source.release]
/// kind = "binary"        # không dùng include
/// root = "target/release"
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceConfig {
    /// Dịch vụ phục vụ.
    pub kind: SourceKind,
    /// Gốc cây nguồn. Mọi `SourceEntry.path` là tương đối so với đây.
    pub root: Utf8PathBuf,
    /// Bộ lọc, diễn giải theo `kind`:
    /// - [`SourceKind::Code`] — danh sách extension (không có dấu chấm).
    /// - [`SourceKind::Document`] — glob pattern.
    /// - [`SourceKind::Binary`] — không dùng.
    pub include: Vec<String>,
    /// Glob loại trừ, áp cho mọi kind (thắng `include`). Rỗng = không loại.
    pub exclude: Vec<String>,
    /// Trần bytes cho mỗi lần `read(None)`. Vượt → `read` trả `Err` **trước
    /// khi** đọc. `None` = không trần.
    pub max_bytes: Option<usize>,
    /// Có bỏ qua `.gitignore` / `.gitignore` cấp thư mục cha không. Mặc định
    /// `true` (khớp hành vi `walk` hiện tại).
    pub respect_ignore: bool,
    /// Bỏ qua entry ẩn (tên bắt đầu `.`). Mặc định `true`.
    pub skip_hidden: bool,
    /// Tên file ignore tùy biến. Mặc định `.codegraphignore`.
    pub ignore_filename: String,
}

impl SourceConfig {
    /// Default theo `kind` — mỗi dịch vụ một bộ riêng.
    pub fn for_kind(kind: SourceKind, root: Utf8PathBuf) -> Self {
        Self {
            kind,
            root,
            include: Vec::new(),
            exclude: Vec::new(),
            // 4 MiB: khớp chặn cũ ở `parse_one` cho code. Binary/Document đọc
            // bằng `read(None)` cũng trần luôn — quá trần thì coi như không
            // phải tài liệu phân tích được.
            max_bytes: Some(4 * 1024 * 1024),
            respect_ignore: true,
            skip_hidden: true,
            ignore_filename: ".codegraphignore".to_string(),
        }
    }

    /// Extension hợp lệ (không dấu chấm, lowercase) — chỉ dùng cho
    /// [`SourceKind::Code`]. Rỗng = mọi extension đều qua.
    pub fn extensions(&self) -> Vec<String> {
        self.include
            .iter()
            .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
            .collect()
    }

    /// `true` nếu `path` (tương đối) khớp ít nhất một glob trong `exclude`.
    pub fn is_excluded(&self, rel_path: &str) -> bool {
        self.exclude
            .iter()
            .any(|pat| glob::Pattern::new(pat).is_ok_and(|g| g.matches(rel_path)))
    }
}

/// Registry: nhiều source theo tên, tra theo `SourceKind`.
///
/// Mỗi dịch vụ chỉ cần `source_for(Kind::X)` — đó là quy tắc "mặc định source
/// này dùng cho dịch vụ nào". Không có source nào khớp kind → `None`, caller
/// fallback về `DiskSource` mặc định để giữ hành vi cũ.
#[derive(Default)]
pub struct SourceRegistry {
    sources: Vec<(String, std::sync::Arc<dyn crate::Source>)>,
}

impl SourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, name: impl Into<String>, source: std::sync::Arc<dyn crate::Source>) {
        self.sources.push((name.into(), source));
    }

    /// Source đầu tiên khớp `kind`.
    pub fn source_for(&self, kind: SourceKind) -> Option<&std::sync::Arc<dyn crate::Source>> {
        self.sources
            .iter()
            .find(|(_, s)| s.kind() == kind)
            .map(|(_, s)| s)
    }

    pub fn by_name(&self, name: &str) -> Option<&std::sync::Arc<dyn crate::Source>> {
        self.sources.iter().find(|(n, _)| n == name).map(|(_, s)| s)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &std::sync::Arc<dyn crate::Source>)> {
        self.sources.iter().map(|(n, s)| (n.as_str(), s))
    }
}
