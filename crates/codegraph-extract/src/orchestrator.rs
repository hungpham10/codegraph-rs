//! Orchestrator: walk project tree → parse từng file (rayon) → `GraphIndex::ingest`.
//!
//! Full re-index (đã chốt — bỏ incremental): mọi lần `index_all` reset toàn bộ
//! index rồi ingest lại (register + remap + resolve + persist + bump version).

use crate::config::ExtractConfig;
use crate::{walker, LangParser};
use camino::Utf8Path;
use codegraph_core::Result;
use codegraph_graph::{GraphIndex, ParseResult};
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
    pub async fn index_all(&self, root: &Utf8Path, index: &mut GraphIndex) -> Result<ExtractStats> {
        let config = ExtractConfig::load(root);
        let files = walker::walk(root, &self.parsers, &config);

        let results: Vec<_> = files.par_iter().map(parse_one).collect();
        let mut parsed = Vec::new();
        let mut skipped = 0u64;
        for r in results {
            match r {
                Ok(Some(p)) => parsed.push(p),
                Ok(None) => skipped += 1,
                Err(_) => {}
            }
        }

        index.ingest(&parsed).await?;
        Ok(stats_of(&parsed, skipped))
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
