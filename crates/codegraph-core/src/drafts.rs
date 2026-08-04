//! Draft types + stats written to/read from the persistent graph store.
//!
//! Moved here from the removed `codegraph-db` crate so extraction (writers),
//! resolution, and CLI tooling can construct/index rows without depending on a
//! specific storage backend. The `Db` implementation that persists these lives
//! in `codegraph-graph::db`.

use crate::{EdgeKind, NodeKind};
use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

/// A file row as stored in the graph store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRow {
    pub id: Option<i64>,
    pub path: Utf8PathBuf,
    pub language: String,
    pub sha256: String,
    pub size: u64,
    pub mtime: i64,
    pub indexed_at: i64,
}

/// A node to be inserted — id is assigned by the store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeDraft {
    pub kind: NodeKind,
    pub name: String,
    pub qualified_name: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    pub signature: Option<String>,
    pub docstring: Option<String>,
    pub language: String,
}

/// An edge to be inserted — endpoints are existing node ids.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeDraft {
    pub from_id: i64,
    pub to_id: i64,
    pub kind: EdgeKind,
    pub file_id: Option<i64>,
    pub line: Option<u32>,
    pub source: Option<String>, // e.g. "framework:express", "resolver:imports"
}

/// Aggregate counts reported by the store (`/api/status`, `codegraph status`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbStats {
    pub files: u64,
    pub nodes: u64,
    pub edges: u64,
    pub size_bytes: u64,
    pub schema_version: u32,
}
