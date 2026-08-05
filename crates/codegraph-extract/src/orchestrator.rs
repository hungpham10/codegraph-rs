//! Orchestrator: walk project tree → parse từng file (rayon) → `GraphIndex::ingest`.
//!
//! Full re-index (đã chốt — bỏ incremental): mọi lần `index_all` reset toàn bộ
//! index rồi ingest lại (register + remap + resolve + persist + bump version).

use crate::config::ExtractConfig;
use crate::{walker, LangParser};
use camino::Utf8Path;
use codegraph_core::Result;
use codegraph_graph::{GraphIndex, IngestProgress, ParseResult};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::sync::Arc;

#[derive(Debug, Default, Clone)]
pub struct ExtractStats {
    pub files: u64,
    pub symbols: u64,
    pub chains: u64,
    pub calls: u64,
    pub skipped: u64,
}

pub struct Orchestrator {
    parsers: Vec<Arc<dyn LangParser>>,
}

impl Orchestrator {
    pub fn new(parsers: Vec<Arc<dyn LangParser>>) -> Self {
        Self { parsers }
    }

    pub fn with_registry() -> Self {
        Self::new(crate::registry())
    }

    /// Walk `root` → parse song song → ingest (full re-index).
    pub async fn index_all(
        &self,
        root: &Utf8Path,
        index: &mut GraphIndex,
        progress: Option<Arc<ProgressBar>>,
    ) -> Result<ExtractStats> {
        let config = ExtractConfig::load(root);
        let files = walker::walk(root, &self.parsers, &config);

        // Create progress bar if requested.
        let pb = if let Some(ref bar) = progress {
            bar.clone()
        } else {
            // Dummy hidden bar when no progress requested – we just skip.
            // Use a zero-length bar to avoid allocations.
            Arc::new(ProgressBar::hidden())
        };
        // Set total length for real bar.
        if progress.is_some() {
            pb.set_length(files.len() as u64);
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("[{elapsed_precise}] [{wide_bar}] {pos}/{len} ({percent}%)")
                    .expect("valid progress bar template")
                    .progress_chars("#>-"),
            );
        }

        // Use a clone of the progress bar for thread-safe updates.
        let progress_opt = progress.clone();
        let results: Vec<_> = files
            .par_iter()
            .map(|fm| {
                let res = parse_one(fm);
                if let Some(ref bar) = progress_opt {
                    bar.inc(1);
                    bar.set_message(fm.path.to_string());
                }
                res
            })
            .collect();
        let mut parsed = Vec::new();
        let mut skipped = 0u64;
        for r in results {
            match r {
                Ok(Some(p)) => parsed.push(p),
                Ok(None) => skipped += 1,
                Err(_) => {}
            }
        }

        // Đưa ProgressBar vào ingest (register → edges → files → engines) — phase
        // index chiếm phần lớn thời gian, không thể để im trong lúc `GraphIndex`
        // ghi sqlite.
        let ingest_progress: Option<Arc<dyn IngestProgress>> = progress
            .as_ref()
            .map(|bar| Arc::new(IngestBar(bar.clone())) as Arc<dyn IngestProgress>);
        index.ingest_with_progress(&parsed, ingest_progress).await?;
        // Finish the progress bar on success.
        if let Some(bar) = progress {
            bar.finish_with_message("Indexing complete");
        }
        Ok(stats_of(&parsed, skipped))
    }
}

/// Nối `IngestProgress` (graph crate) vào `indicatif::ProgressBar` của CLI:
/// `phase` reset bar về 0 + set length theo số đơn vị phase (không hiện chữ —
/// template chỉ `pos/len/percent`), `advance` tăng pos.
struct IngestBar(Arc<ProgressBar>);

impl IngestProgress for IngestBar {
    fn phase(&self, _name: &'static str, total: usize) {
        if total > 0 {
            self.0.set_length(total as u64);
            self.0.set_position(0);
        }
    }

    fn advance(&self, n: usize) {
        self.0.inc(n as u64);
    }
}

fn stats_of(parsed: &[ParseResult], skipped: u64) -> ExtractStats {
    ExtractStats {
        files: parsed.len() as u64,
        symbols: parsed.iter().map(|p| p.symbols.len() as u64).sum(),
        chains: parsed.iter().map(|p| p.chains.len() as u64).sum(),
        calls: parsed.iter().map(|p| p.calls.len() as u64).sum(),
        skipped,
    }
}

/// Parse một file — bỏ qua binary/quá lớn/không phải UTF-8.
fn parse_one(fm: &walker::FileMatch) -> Result<Option<ParseResult>> {
    let bytes = match std::fs::read(fm.path.as_std_path()) {
        Ok(b) if b.len() < 4 * 1024 * 1024 => b,
        _ => return Ok(None),
    };
    let source = match std::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };
    fm.parser.parse_file(fm.path.as_str(), source).map(Some)
}
