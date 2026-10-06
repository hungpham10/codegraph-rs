//! Phân tích binary bằng radare2 — chuyển functions/imports/strings/call graph
//! thành `ParseResult` để `GraphIndex::ingest` nạp vào semantic graph.
//!
//! ## Cài đặt
//! Yêu cầu `radare2` trong PATH (check bằng `codegraph doctor`).
//!
//! ## Ví dụ
//! ```rust,no_run
//! use codegraph_binary::{extract_binary, config::AnalysisDepth};
//! use std::path::Path;
//!
//! let result = extract_binary(Path::new("/bin/ls"), AnalysisDepth::Aaa, true)?;
//! // `result` nạp thẳng vào GraphIndex::ingest
//! # Ok::<_, codegraph_core::Error>(())
//! ```

pub mod cache;
pub mod config;
pub mod extract;
pub mod model;
pub mod r2;
pub mod scan;

pub use crate::extract::extract_binary;
use camino::Utf8Path;
use codegraph_graph::ParseResult;
use codegraph_source::{DiskSource, SourceConfig, SourceKind};
use tracing::warn;

/// Duyệt các file binary trong workspace, phân tích từng file → `ParseResult`.
/// Gọi 1 lần ở orchestrator.
pub fn collect_binaries(
    root: &Utf8Path,
    cfg: &BinaryConfig,
) -> (Vec<ParseResult>, u64 /* skipped */) {
    if !cfg.enabled {
        return (Vec::new(), 0);
    }
    if !r2_available() {
        warn!(
            "radare2 không có trong PATH — bỏ qua phân tích binary. Cài: brew install radare2 / apt install radare2"
        );
        return (Vec::new(), 0);
    }
    let src = DiskSource::new(SourceConfig::for_kind(
        SourceKind::Binary,
        root.to_path_buf(),
    ));
    let mut results = Vec::new();
    let mut skipped = 0u64;
    for entry in scan::find_binaries_in(&src) {
        // `r2` cần path hệ thống → materialize (disk: trả chính path gốc).
        let path = match src.materialize_blocking(&entry) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("không materialize được {}: {e}", entry.path);
                skipped += 1;
                continue;
            }
        };
        // Cache key tính **một lần** — trước đây `cache_path` gọi 2×
        // `fs::metadata` và được gọi tới 3 lần cho mỗi binary.
        let cache_p = cache::cache_path(root, path.as_std_path(), &entry);
        if cache::is_cached(root, path.as_std_path(), &entry, cfg) {
            if let Some(cached) = cache::load(&cache_p) {
                results.push(cached);
                continue;
            }
        }
        match extract_binary(path.as_std_path(), cfg.depth, cfg.cfg_markers) {
            Ok(res) => {
                if cfg.cache {
                    let _ = cache::store(&cache_p, &res);
                }
                results.push(res);
            }
            Err(e) => {
                tracing::warn!("không phân tích được {}: {e}", path);
                skipped += 1;
            }
        }
    }
    (results, skipped)
}

pub use config::{AnalysisDepth, BinaryConfig};
pub use r2::{r2_available, r2_version};
