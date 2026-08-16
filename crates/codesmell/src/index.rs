//! Build an in-memory CodeGraph from a repository root.
//!
//! CodeSmell never relies on a pre-built `.codegraph` index: each run walks the
//! repo (honoring `.gitignore` via the extractor's walker) and parses it fresh
//! into a `GraphIndex::in_memory()`. CodeGraph stays the understanding layer;
//! only the persistent storage layer is dropped.

use camino::Utf8Path;
use codegraph_extract::Orchestrator;
use codegraph_graph::GraphIndex;

/// Parse `root` (whole repo) and return an in-memory graph.
pub async fn build_index(root: &Utf8Path) -> anyhow::Result<GraphIndex> {
    let orch = Orchestrator::with_registry();
    let (parsed, _stats) = orch.parse_project(root)?;
    let mut index = GraphIndex::in_memory();
    index.ingest(&parsed).await?;
    Ok(index)
}
