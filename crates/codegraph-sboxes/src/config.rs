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
use serde::Deserialize;
use std::fs;

/// Why config loading failed. Kept small — most callers can fall back to
/// [`SboxConfig::default`] on error.
#[derive(Debug, thiserror::Error)]
pub enum SboxConfigError {
    #[error("sandbox config io: {0}")]
    Io(#[from] std::io::Error),
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
        let mut cfg = Self::load_from(&root.join(".codegraph").join("config.toml"))?;
        cfg.root = root.to_path_buf();
        Ok(cfg)
    }

    /// Load from an explicit path. Missing file → default.
    pub fn load_from(path: &Utf8Path) -> Result<Self, SboxConfigError> {
        let Ok(text) = fs::read_to_string(path.as_std_path()) else {
            return Ok(Self::default());
        };
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

    #[test]
    fn missing_file_is_default() {
        let cfg = SboxConfig::load_from(Utf8Path::new("/nonexistent/x.toml")).unwrap();
        assert_eq!(cfg.loop_cap, 10);
        assert_eq!(cfg.branch_policy, BranchPolicy::IfTrue);
    }

    #[test]
    fn parse_sandbox_section() {
        let dir = std::env::temp_dir().join("codegraph-sboxes-cfg-test");
        std::fs::create_dir_all(&dir).unwrap();
        let cfg_path = dir.join("config.toml");
        let path = Utf8Path::from_path(cfg_path.as_path()).unwrap();
        std::fs::write(
            path,
            "[sandbox]\nmock_dirs = [\"mocks/a\", \"mocks/b\"]\nloop_cap = 3\nbranch_policy = \"if_false\"\n",
        )
        .unwrap();
        let cfg = SboxConfig::load_from(path).unwrap();
        assert_eq!(cfg.mock_dirs, vec!["mocks/a", "mocks/b"]);
        assert_eq!(cfg.loop_cap, 3);
        assert_eq!(cfg.branch_policy, BranchPolicy::IfFalse);
        let _ = std::fs::remove_file(path.as_std_path());
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn unknown_policy_is_error() {
        let dir = std::env::temp_dir().join("codegraph-sboxes-cfg-bad");
        std::fs::create_dir_all(&dir).unwrap();
        let cfg_path = dir.join("config.toml");
        let path = Utf8Path::from_path(cfg_path.as_path()).unwrap();
        std::fs::write(path, "[sandbox]\nbranch_policy = \"sometimes\"\n").unwrap();
        assert!(SboxConfig::load_from(path).is_err());
        let _ = std::fs::remove_file(path.as_std_path());
        let _ = std::fs::remove_dir(&dir);
    }
}