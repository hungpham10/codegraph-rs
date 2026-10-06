//! Sandbox configuration — read from the project's `.codegraph/config.toml`
//! `[sandbox]` section (same file `codegraph-extract` already uses for
//! `[languages]`, so there is exactly one project config file).
//!
//! ```toml
//! [sandbox]
//! mock_dirs = ["sandbox/mocks"]
//! loop_cap = 10
//! branch_policy = "if_true"
//!
//! # Effect rules (Piece 2) — dùng chung schema với codegraph-extract.
//! # [[effect_rules]]
//! # call = { prefix = "db." }
//! # effect = "sql_query"
//! ```

use crate::runtime::BranchPolicy;
use camino::{Utf8Path, Utf8PathBuf};
use codegraph_core::EffectRule;
use codegraph_source::DiskSource;
use serde::Deserialize;

/// Why config loading failed. Kept small — most callers can fall back to
/// [`SboxConfig::default`] on error.
#[derive(Debug, thiserror::Error)]
pub enum SboxConfigError {
    #[error("sandbox config io: {0}")]
    Io(#[from] std::io::Error),
    #[error("sandbox config source: {0}")]
    Source(#[from] codegraph_core::Error),
    #[error("sandbox config is not valid UTF-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("sandbox config parse: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("sandbox config: unknown branch_policy `{0}` (expected if_true/if_false)")]
    BranchPolicy(String),
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ConfigFile {
    sandbox: SandboxSection,
    /// Effect rules dùng chung (schema `EffectRule` trong codegraph-core, cùng
    /// file `[[effect_rules]]` mà codegraph-extract đọc). Consumed bởi Piece 3.
    #[serde(default)]
    effect_rules: Vec<EffectRule>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct SandboxSection {
    mock_dirs: Vec<String>,
    loop_cap: Option<usize>,
    branch_policy: Option<String>,
}

/// Sandbox behavior configuration.
#[derive(Debug, Clone)]
pub struct SboxConfig {
    /// Project root that relative `mock_dirs` resolve against.
    pub root: Utf8PathBuf,
    /// Directories (relative to `root`) containing `*.rhai` mocks.
    pub mock_dirs: Vec<String>,
    /// Max iterations for any loop; guarantees termination.
    pub loop_cap: usize,
    /// How conditions are resolved at run time (deterministic by default).
    pub branch_policy: BranchPolicy,
    /// Project effect rules (top-level `[[effect_rules]]` in config.toml) —
    /// consumed bởi Piece 3 (state delta theo effect).
    #[allow(dead_code, reason = "Piece 3: effect rules drive state deltas")]
    pub effect_rules: Vec<EffectRule>,
}

impl Default for SboxConfig {
    fn default() -> Self {
        Self {
            root: Utf8PathBuf::from("."),
            mock_dirs: vec!["sandbox/mocks".to_string()],
            loop_cap: 10,
            branch_policy: BranchPolicy::IfTrue,
            effect_rules: Vec::new(),
        }
    }
}

impl SboxConfig {
    /// Load `.codegraph/config.toml` under `root`. Missing file → default
    /// (with `root` still set so relative mock dirs resolve correctly).
    pub fn load(root: &Utf8Path) -> Result<Self, SboxConfigError> {
        let mut cfg = Self::load_from(&DiskSource::for_config(root.to_path_buf()))?;
        cfg.root = root.to_path_buf();
        Ok(cfg)
    }

    /// Load config qua Source layer (không còn `std::fs`). Missing file →
    /// default; lỗi đọc/parse thật thì trả `Err` — khác `ExtractConfig` vốn
    /// không có kênh lỗi nên phải rơi về default.
    pub fn load_from(source: &DiskSource) -> Result<Self, SboxConfigError> {
        let Some(bytes) = source.read_config_blocking()? else {
            return Ok(Self::default());
        };
        let text = String::from_utf8(bytes)?;
        let cfg: ConfigFile = toml::from_str(&text)?;
        let policy = match cfg.sandbox.branch_policy.as_deref() {
            None => BranchPolicy::IfTrue,
            Some("if_true") => BranchPolicy::IfTrue,
            Some("if_false") => BranchPolicy::IfFalse,
            Some(other) => return Err(SboxConfigError::BranchPolicy(other.to_string())),
        };
        Ok(Self {
            root: Utf8PathBuf::from("."),
            mock_dirs: if cfg.sandbox.mock_dirs.is_empty() {
                vec!["sandbox/mocks".to_string()]
            } else {
                cfg.sandbox.mock_dirs
            },
            loop_cap: cfg.sandbox.loop_cap.unwrap_or(10),
            branch_policy: policy,
            effect_rules: cfg.effect_rules,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Root tạm ở đúng hình dạng thật (`.codegraph/config.toml`) — config chỉ
    /// đọc được ở đường dẫn quy ước nên không còn đường đọc để bypass.
    struct CfgFixture {
        dir: std::path::PathBuf,
        source: DiskSource,
    }

    impl CfgFixture {
        /// `None` = không có file config (kiểm tra "thiếu → default").
        fn new(name: &str, body: Option<&str>) -> Self {
            let dir = std::env::temp_dir().join(format!("codegraph-sboxes-{name}"));
            let _ = std::fs::remove_dir_all(&dir);
            let root = Utf8PathBuf::from_path_buf(dir.clone()).unwrap();
            let cfg = root.join(codegraph_source::CONFIG_REL_PATH);
            std::fs::create_dir_all(cfg.parent().unwrap().as_std_path()).unwrap();
            if let Some(body) = body {
                std::fs::write(cfg.as_std_path(), body).unwrap();
            }
            Self {
                source: DiskSource::for_config(root),
                dir,
            }
        }

        fn load(&self) -> Result<SboxConfig, SboxConfigError> {
            SboxConfig::load_from(&self.source)
        }
    }

    #[test]
    fn missing_file_is_default() {
        let fx = CfgFixture::new("cfg-missing", None);
        let cfg = fx.load().unwrap();
        assert_eq!(cfg.loop_cap, 10);
        assert_eq!(cfg.branch_policy, BranchPolicy::IfTrue);
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn parse_sandbox_section() {
        let fx = CfgFixture::new(
            "cfg-ok",
            Some(
                "[sandbox]\nmock_dirs = [\"mocks/a\", \"mocks/b\"]\nloop_cap = 3\nbranch_policy = \"if_false\"\n",
            ),
        );
        let cfg = fx.load().unwrap();
        assert_eq!(cfg.mock_dirs, vec!["mocks/a", "mocks/b"]);
        assert_eq!(cfg.loop_cap, 3);
        assert_eq!(cfg.branch_policy, BranchPolicy::IfFalse);
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn unknown_policy_is_error() {
        let fx = CfgFixture::new(
            "cfg-bad",
            Some("[sandbox]\nbranch_policy = \"sometimes\"\n"),
        );
        assert!(fx.load().is_err());
        let _ = std::fs::remove_dir_all(&fx.dir);
    }
}
