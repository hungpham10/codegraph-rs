//! Local HTTP server + embedded web UI for graph visualization.

pub mod api;
mod assets;
mod server;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    pub depth: u32,
}

#[derive(Debug, Clone)]
pub struct VizConfig {
    pub port: u16,
    pub open_browser: bool,
    pub boot: BootConfig,
}

/// Serve UI trên index đã persist tại `db_path` (`.codegraph/db.sqlite`).
pub async fn run(db_path: PathBuf, config: VizConfig) -> anyhow::Result<()> {
    server::serve(db_path, config).await
}
