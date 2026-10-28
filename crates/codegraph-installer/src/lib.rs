//! Multi-agent installer. One target per file in `targets/`.

pub mod skills;
pub mod targets;
pub mod workflow;

use anyhow::Result;
use camino::Utf8PathBuf;
use std::sync::Arc;

pub const INSTRUCTIONS_MD: &str = include_str!("instructions-template.md");

#[derive(Debug, Clone)]
pub struct InstallOpts {
    /// Workspace root (for project-scoped installs).
    pub project_root: Option<Utf8PathBuf>,
    /// Install globally (in user home) rather than project-local.
    pub global: bool,
    /// Absolute path to the `codegraph` binary (for MCP `command`).
    pub binary_path: Utf8PathBuf,
    /// Override home directory (used in tests for isolation).
    /// None = use dirs::home_dir().
    pub home_dir: Option<Utf8PathBuf>,
}

impl InstallOpts {
    pub fn home_dir(&self) -> Option<std::path::PathBuf> {
        self.home_dir
            .as_ref()
            .map(|p| p.as_std_path().to_path_buf())
            .or_else(dirs::home_dir)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectStatus {
    NotFound,
    Found,
    AlreadyConfigured,
}

#[derive(Debug, Clone)]
pub enum InstallReport {
    Installed(Vec<Utf8PathBuf>),
    Unchanged,
    Updated(Vec<Utf8PathBuf>),
    Skipped(String),
}

pub trait AgentTarget: Send + Sync {
    fn id(&self) -> &'static str;
    fn label(&self) -> &'static str;
    fn detect(&self, opts: &InstallOpts) -> DetectStatus;
    fn install(&self, opts: &InstallOpts) -> Result<InstallReport>;
    fn uninstall(&self, opts: &InstallOpts) -> Result<InstallReport>;
    /// Thư mục cài skills của agent (None = agent không hỗ trợ skills).
    fn skills_dir(&self, opts: &InstallOpts) -> Option<Utf8PathBuf> {
        let _ = opts;
        None
    }
}

pub fn registry() -> Vec<Arc<dyn AgentTarget>> {
    vec![
        Arc::new(targets::claude::ClaudeTarget),
        Arc::new(targets::cursor::CursorTarget),
        Arc::new(targets::codex::CodexTarget),
        Arc::new(targets::opencode::OpencodeTarget),
        Arc::new(targets::hermes::HermesTarget),
        Arc::new(targets::antigravity::AntigravityTarget),
        Arc::new(targets::zcode::ZcodeTarget),
    ]
}

/// Alias của `registry()` — mọi target hiện đều hỗ trợ cả hai scope.
pub fn project_registry() -> Vec<Arc<dyn AgentTarget>> {
    registry()
}
