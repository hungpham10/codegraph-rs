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

    /// Walk `root` → parse song song → trả về `(parsed, stats)`, KHÔNG ingest.
    ///
    /// Dùng cho benchmark để tách riêng thời gian của codegraph-extract (walk +
    /// parse) khỏi codegraph-graph (ingest). Đi xe cùng logic với `index_all` qua
    /// `parse_files`.
    pub fn parse_project(&self, root: &Utf8Path) -> Result<(Vec<ParseResult>, ExtractStats)> {
        let config = ExtractConfig::load(root);
        let files = walker::walk(root, &self.parsers, &config);
        let (parsed, skipped) = self.parse_files(&files, None);
        let stats = stats_of(&parsed, skipped);
        Ok((parsed, stats))
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

        // Create progress bar if requested (nếu progress `None` → invisible bar).
        let pb0 = if let Some(ref bar) = progress {
            bar.clone()
        } else {
            Arc::new(ProgressBar::hidden())
        };
        if progress.is_some() {
            pb0.set_length(files.len() as u64);
            pb0.set_style(
                ProgressStyle::default_bar()
                    .template("[{elapsed_precise}] [{wide_bar}] {pos}/{len} ({percent}%)")
                    .expect("valid progress bar template")
                    .progress_chars("#>-"),
            );
        }

        let (parsed, skipped) = self.parse_files(&files, progress.clone());

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

    /// Parse song song một danh sách file — trả về parsed + số file bị skip.
    fn parse_files(
        &self,
        files: &[walker::FileMatch],
        progress: Option<Arc<ProgressBar>>,
    ) -> (Vec<ParseResult>, u64) {
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
        (parsed, skipped)
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

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use std::io::Write;

    /// Tạo fixture repo temp với 2 file (rust + go) rồi chạy `parse_project`.
    #[test]
    fn parse_project_walks_and_parses_without_ingest() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(src.as_std_path()).unwrap();
        for (name, content) in [
            (
                "lib.rs",
                "pub fn add(a: i32, b: i32) -> i32 { a + b }\npub fn sub(a: i32, b: i32) -> i32 { a - b }\n",
            ),
            (
                "main.go",
                "package main\nfunc greet(name string) string { return \"hi \" + name }\n",
            ),
        ] {
            let mut f = std::fs::File::create(src.join(name).as_std_path()).unwrap();
            f.write_all(content.as_bytes()).unwrap();
        }

        let orch = Orchestrator::with_registry();
        let (parsed, stats) = orch.parse_project(&root).unwrap();

        // Cả 2 file được parse, không file nào bị skip.
        assert_eq!(parsed.len(), 2, "phải parse được cả lib.rs + main.go");
        assert_eq!(stats.files, 2);
        assert_eq!(stats.skipped, 0);
        assert!(parsed.iter().all(|p| !p.symbols.is_empty()), "mỗi file phải có symbol");
        assert!(stats.symbols > 0, "tổng symbol > 0");
        // stats khớp với chính parsed (không ingest thêm gì).
        assert_eq!(stats.symbols, parsed.iter().map(|p| p.symbols.len() as u64).sum::<u64>());
    }

    /// File không đọc được / quá lớn / không UTF-8 → bị đếm vào `skipped`.
    #[test]
    fn parse_project_counts_skipped_non_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(src.as_std_path()).unwrap();
        let mut f = std::fs::File::create(src.join("bin.rs").as_std_path()).unwrap();
        // Rust file chứa byte không hợp lệ UTF-8 nhưng đủ nhỏ → skip (không UTF-8).
        f.write_all(&[0xff, 0xfe, 0x00, 0x01, 0x02]).unwrap();

        let orch = Orchestrator::with_registry();
        let (parsed, stats) = orch.parse_project(&root).unwrap();
        assert_eq!(parsed.len(), 0);
        assert_eq!(stats.files, 0);
        assert_eq!(stats.skipped, 1);
    }
}
