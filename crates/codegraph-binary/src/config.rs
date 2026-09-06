//! Cấu hình phân tích binary (`.codegraph/config.toml` section `[binary]`).

use serde::Deserialize;

/// Độ sâu phân tích của radare2 cho một binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnalysisDepth {
    /// Phân tích đầy đủ (`aaa`). Chậm hơn nhưng chính xác nhất.
    #[default]
    Aaa,
    /// Nhanh hơn: `af` + `aar` + `aac` (không chạy `aaaa`). Phù hợp binary lớn.
    Fast,
}

impl AnalysisDepth {
    /// Chuỗi lệnh tương ứng với r2.
    pub fn command(self) -> &'static str {
        match self {
            Self::Aaa => "aaa",
            Self::Fast => "af; aar; aac",
        }
    }
}

impl std::fmt::Display for AnalysisDepth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl AnalysisDepth {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Aaa => "aaa",
            Self::Fast => "fast",
        }
    }
}

/// Cấu hình section `[binary]` trong `.codegraph/config.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BinaryConfig {
    /// Bật tắt việc phân tích binary bằng radare2 khi index (mặc định bật).
    pub enabled: bool,
    /// Độ sâu phân tích (mặc định `aaa`).
    pub depth: AnalysisDepth,
    /// Xây dựng marker IF/LOOP/SWITCH từ CFG của mỗi function (`pdfj`).
    pub cfg_markers: bool,
    /// Cache kết quả phân tích theo (path, mtime, size) để tránh chạy `aaa` lại.
    pub cache: bool,
}

impl Default for BinaryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            depth: AnalysisDepth::default(),
            cfg_markers: true,
            cache: true,
        }
    }
}
