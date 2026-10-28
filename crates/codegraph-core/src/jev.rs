//! Config cho Jev (TypeSafe SystemOne) — rerank kết quả `codegraph_context`.
//!
//! Jev là **decision model**, không phải chat model: nhận `state` + `questions`
//! và trả xác suất. Ở đây nó dùng để chọn symbol gần nhất trong danh sách
//! candidate (Choice) kèm một câu hỏi Noul "có cái nào khớp không".
//!
//! Config **OPT-IN**, mặc định TẮT: khi bật, query + tên symbol + đường dẫn file
//! sẽ rời máy. Chỉ đọc `[jev]` trong `.codegraph/config.toml`.
//!
//! Module này chỉ giữ **data + global config** (không HTTP) để `codegraph-extract`
//! (đọc config) và `codegraph-context` (gọi API) dùng chung mà không thêm
//! dependency chéo. Client HTTP nằm ở `codegraph-context`.

use std::sync::OnceLock;

/// Endpoint mặc định của Jev qua OpenCode Zen (OpenAI-agnostic gateway).
pub const DEFAULT_ENDPOINT: &str = "https://opencode.ai/zen/v1/systemone";
/// Model miễn phí — model trả phí (`jev-1.13`) phải tự khai báo trong config.
pub const DEFAULT_MODEL: &str = "jev-1.13-free";
/// Env var mặc định chứa bearer token của OpenCode Zen.
pub const DEFAULT_API_KEY_ENV: &str = "OPENCODE_API_KEY";
/// Nhãn option "không có symbol nào liên quan" trong câu hỏi Choice của Jev.
pub const NONE_OPTION: &str = "none_of_the_above";

/// Cấu hình Jev đã parse từ `[jev]`.
#[derive(Debug, Clone, PartialEq)]
pub struct JevConfig {
    /// Bật rerank bằng Jev. Mặc định `false` (dữ liệu rời máy).
    pub enabled: bool,
    /// URL đầy đủ của SystemOne endpoint (không phải base URL).
    pub endpoint: String,
    /// Model id — free mặc định; đổi sang `jev-1.13` nếu muốn trả phí.
    pub model: String,
    /// Tên biến môi trường chứa token (không bao giờ đọc key từ config.toml).
    pub api_key_env: String,
    /// Timeout request; quá hạn → fail-open (giữ hành vi cũ).
    pub timeout_ms: u64,
    /// Số candidate tối đa gửi cho Jev (Choice nhận tối đa 255 options).
    pub max_candidates: usize,
    /// Xác suất tối thiểu của **symbol đứng đầu** để coi là khớp.
    ///
    /// Chỉ áp cho hạng 1: khi Jev chia xác suất cho nhiều candidate gần nhau,
    /// các hạng sau vẫn giữ nguyên thứ tự thay vì bị cắt sạch.
    pub min_probability: f32,
    /// Tên repo đưa vào `state`. `None` → caller tự lấy basename workspace root.
    pub repo: Option<String>,
    /// Đưa **value** của doc node vào mô tả option khi rerank doc search.
    ///
    /// `key` một mình thường không đủ phân biệt (`spec.replicas` có ở nhiều
    /// file); value giúp Jev chọn đúng node nhưng tốn token theo số candidate,
    /// nên mặc định TẮT.
    pub doc_include_values: bool,
    /// Cắt value còn bao nhiêu ký tự khi `doc_include_values = true`.
    pub doc_value_chars: usize,
}

impl Default for JevConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: DEFAULT_ENDPOINT.to_string(),
            model: DEFAULT_MODEL.to_string(),
            api_key_env: DEFAULT_API_KEY_ENV.to_string(),
            timeout_ms: 2_000,
            max_candidates: 40,
            min_probability: 0.25,
            repo: None,
            doc_include_values: false,
            doc_value_chars: 120,
        }
    }
}

impl JevConfig {
    /// Parse từ các raw string của `[jev]` — `None`/chuỗi rỗng dùng default.
    ///
    /// `enabled` chỉ `true` khi khai báo tường minh `enabled = true`: giống
    /// `[embedding]`, không có key thì không gửi gì ra ngoài.
    #[allow(clippy::too_many_arguments)]
    pub fn from_raw(
        enabled: Option<bool>,
        endpoint: Option<&str>,
        model: Option<&str>,
        api_key_env: Option<&str>,
        timeout_ms: Option<u64>,
        max_candidates: Option<usize>,
        min_probability: Option<f32>,
        repo: Option<&str>,
        doc_include_values: Option<bool>,
        doc_value_chars: Option<usize>,
    ) -> Self {
        let d = Self::default();
        let text = |v: Option<&str>, fallback: &str| {
            v.map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(fallback)
                .to_string()
        };
        Self {
            enabled: enabled.unwrap_or(false),
            endpoint: text(endpoint, &d.endpoint),
            model: text(model, &d.model),
            api_key_env: text(api_key_env, &d.api_key_env),
            timeout_ms: timeout_ms.filter(|v| *v > 0).unwrap_or(d.timeout_ms),
            max_candidates: max_candidates
                .filter(|v| *v > 0)
                .unwrap_or(d.max_candidates),
            min_probability: min_probability
                .filter(|v| (0.0..=1.0).contains(v))
                .unwrap_or(d.min_probability),
            repo: repo
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            doc_include_values: doc_include_values.unwrap_or(d.doc_include_values),
            doc_value_chars: doc_value_chars
                .filter(|v| *v > 0)
                .unwrap_or(d.doc_value_chars),
        }
    }

    /// Token đọc từ env var đã cấu hình. `None` → không gọi API (fail-open).
    pub fn api_key(&self) -> Option<String> {
        let raw = std::env::var(&self.api_key_env).ok()?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    /// Chỉ chạy khi bật + có token. Thiếu một trong hai → giữ hành vi cũ.
    pub fn is_ready(&self) -> bool {
        self.enabled && self.api_key().is_some()
    }
}

static JEV_CONFIG: OnceLock<JevConfig> = OnceLock::new();
static DEFAULT_JEV_CONFIG: OnceLock<JevConfig> = OnceLock::new();

/// Áp dụng config (chỉ lần đầu; các lần sau bị bỏ qua, như `set_embedding_config`).
pub fn set_jev_config(cfg: JevConfig) {
    let _ = JEV_CONFIG.set(cfg);
}

/// Config hiện tại — mặc định TẮT nếu chưa set.
///
/// Không tự khởi tạo [`JEV_CONFIG`] bằng default khi đọc: nếu làm vậy, config
/// mặc định (tắt) sẽ bị "khoá" trước khi startup kịp set.
pub fn jev_config() -> &'static JevConfig {
    match JEV_CONFIG.get() {
        Some(cfg) => cfg,
        None => DEFAULT_JEV_CONFIG.get_or_init(JevConfig::default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_raw_rỗng_thì_dùng_default_và_tắt() {
        let cfg = JevConfig::from_raw(None, None, None, None, None, None, None, None, None, None);
        assert!(!cfg.enabled);
        assert_eq!(cfg.endpoint, DEFAULT_ENDPOINT);
        assert_eq!(cfg.model, DEFAULT_MODEL);
        assert_eq!(cfg.api_key_env, DEFAULT_API_KEY_ENV);
        assert_eq!(cfg.timeout_ms, 2_000);
        assert_eq!(cfg.max_candidates, 40);
        assert_eq!(cfg.min_probability, 0.25);
        assert!(cfg.repo.is_none());
        assert!(
            !cfg.doc_include_values,
            "value doc tắt mặc định (tốn token)"
        );
        assert_eq!(cfg.doc_value_chars, 120);
    }

    #[test]
    fn from_raw_bỏ_qua_giá_trị_vô_lý() {
        let cfg = JevConfig::from_raw(
            Some(true),
            Some("  "),
            Some("jev-1.13"),
            Some(""),
            Some(0),
            Some(0),
            Some(-0.2),
            Some("  "),
            Some(true),
            Some(0),
        );
        assert!(cfg.enabled);
        assert_eq!(cfg.endpoint, DEFAULT_ENDPOINT, "chuỗi rỗng → default");
        assert_eq!(cfg.model, "jev-1.13");
        assert_eq!(cfg.api_key_env, DEFAULT_API_KEY_ENV);
        assert_eq!(cfg.timeout_ms, 2_000, "0 → default");
        assert_eq!(cfg.max_candidates, 40, "0 → default");
        assert_eq!(cfg.min_probability, 0.25, "ngoài [0,1] → default");
        assert!(cfg.repo.is_none(), "chuỗi rỗng → None");
        assert!(cfg.doc_include_values);
        assert_eq!(cfg.doc_value_chars, 120, "0 → default");
    }

    #[test]
    fn ready_cần_cả_enabled_lẫn_env() {
        let mut cfg = JevConfig::default();
        assert!(!cfg.is_ready(), "mặc định tắt");
        cfg.enabled = true;
        cfg.api_key_env = "CODEGRAPH_TEST_JEV_KEY_ABSENT".into();
        assert!(!cfg.is_ready(), "thiếu env → không chạy");
    }
}
