//! `DiskSource` — provider mặc định, đọc từ filesystem local.
//!
//! Đây là chỗ duy nhất trong crate biết tới `ignore::WalkBuilder`. Trước đó
//! cấu hình traversal bị copy **y hệt** ở 3 nơi: `extract/src/walker.rs`,
//! `extract/src/config.rs` (`detect_project_header_hint`) và
//! `binary/src/scan.rs` (`find_binaries`) — mỗi chỗ một `WalkBuilder` với cùng
//! 5 option. Đổi chính sách ignore giờ chỉ sửa một file.
//!
//! # Vì sao core là sync
//!
//! `DiskSource` phục vụ *local disk*, và mọi đường ingest hiện tại đã **block**
//! sẵn: `parse_files` dùng rayon (blocking pool), `collect_binaries` spawn
//! process `r2` (blocking tới vài giây). Bọc thêm `spawn_blocking` chỉ tốn
//! một `SourceConfig` clone mỗi lần gọi mà **không** thay đổi gì quan sát
//! được — nên core là sync, các method `async` của trait chỉ là bọc mỏng.
//!
//! Provider từ xa (git host, object store) sẽ implement trait với I/O thật sự
//! bất đồng bộ — đó là lý do trait vẫn là `#[async_trait]`.

use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use codegraph_core::{Error, Result};
use ignore::WalkBuilder;
use std::io::Read as _;

use crate::{Source, SourceConfig, SourceEntry, SourceKind};

/// Traversal policy dùng chung — **một nguồn sự thật duy nhất** cho cả 3 dịch
/// vụ. Xuất ra `pub` để caller blocking (binary scanner) dùng chung thay vì tự
/// dựng `WalkBuilder` lần nữa.
pub fn disk_walker(config: &SourceConfig) -> ignore::Walk {
    let mut wb = WalkBuilder::new(config.root.as_std_path());
    wb.hidden(config.skip_hidden)
        .git_ignore(config.respect_ignore)
        .git_exclude(config.respect_ignore)
        .parents(config.respect_ignore)
        .add_custom_ignore_filename(&config.ignore_filename);
    wb.build()
}

/// Nguồn đọc từ đĩa local. `materialize` là no-op (path đã tồn tại sẵn).
pub struct DiskSource {
    config: SourceConfig,
}

impl DiskSource {
    pub fn new(config: SourceConfig) -> Self {
        Self { config }
    }

    /// Nguồn mặc định cho `kind`; `extensions` chỉ dùng cho
    /// [`SourceKind::Code`].
    pub fn of_kind(kind: SourceKind, root: Utf8PathBuf, extensions: &[&str]) -> Self {
        let mut config = SourceConfig::for_kind(kind, root);
        config.include = extensions.iter().map(|e| e.to_string()).collect();
        Self::new(config)
    }

    /// Source cho **đường query** (`include_source` của context builder).
    ///
    /// Khác `of_kind` ở 2 điểm, cả hai đều vì đường query khác đường ingest:
    ///
    /// - **Không lọc `include`** — ta đi từ `Symbol.file` (đã biết) ra entry,
    ///   không cần discovery nên không cần danh sách extension.
    /// - **`max_bytes = None`** — trước đây đọc `std::fs::read_to_string` không
    ///   trần, file 8 MiB vẫn hiện source. Đặt trần ở đây sẽ âm thầm cắt
    ///   output của một tính năng đang chạy tốt.
    pub fn for_query(root: Utf8PathBuf) -> Self {
        let mut config = SourceConfig::for_kind(SourceKind::Code, root);
        config.max_bytes = None;
        Self::new(config)
    }

    /// `true` nếi entry được chấp nhận theo `include` của `kind`.
    ///
    /// - `Code` — khớp danh sách extension (không phân biệt hoa thường).
    /// - `Document` — khớp ít nhất một glob pattern.
    /// - `Binary` — không lọc theo tên; magic bytes quyết định ở bước đọc.
    pub fn accepts(&self, rel: &Utf8Path) -> bool {
        if self.config.is_excluded(rel.as_str()) {
            return false;
        }
        match self.config.kind {
            SourceKind::Binary => true,
            SourceKind::Code => {
                let exts = self.config.extensions();
                if exts.is_empty() {
                    return true;
                }
                match rel.extension().and_then(|e| e.to_str()) {
                    Some(e) => {
                        let e = e.to_ascii_lowercase();
                        exts.iter().any(|x| *x == e)
                    }
                    None => false,
                }
            }
            SourceKind::Document => {
                if self.config.include.is_empty() {
                    return true;
                }
                let s = rel.as_str();
                self.config
                    .include
                    .iter()
                    .any(|pat| glob::Pattern::new(pat).is_ok_and(|g| g.matches(s)))
            }
        }
    }

    /// Liệt kê entry (blocking) — xem module docs về vì sao core là sync.
    pub fn list_blocking(&self) -> Result<Vec<SourceEntry>> {
        let root = self.config.root.clone();
        let mut out = Vec::new();
        for entry in disk_walker(&self.config).flatten() {
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let Ok(abs) = Utf8PathBuf::from_path_buf(entry.path().to_path_buf()) else {
                continue;
            };
            let Ok(rel) = abs.strip_prefix(&root) else {
                continue;
            };
            if !self.accepts(rel) {
                continue;
            }
            // Stat lỗi (permission, symlink đứt) → bỏ qua, không fail cả list.
            let size = std::fs::metadata(abs.as_std_path()).ok().map(|m| m.len());
            out.push(SourceEntry::with_size(rel.to_path_buf(), size));
        }
        Ok(out)
    }

    /// Đọc tối đa `limit` byte đầu (blocking). `None` = đọc hết.
    ///
    /// Nếu vượt `config().max_bytes` thì trả `Err` **trước khi** đọc — caller
    /// tự quyết định skip hay báo lỗi, luật nghiệp vụ ở tầng service chứ không
    /// nhét vào trait.
    pub fn read_blocking(&self, entry: &SourceEntry, limit: Option<usize>) -> Result<Vec<u8>> {
        let abs = self.config.root.join(&entry.path);

        let exceeds_cap = match (self.config.max_bytes, limit) {
            (Some(cap), Some(l)) => l > cap,
            (Some(cap), None) => match std::fs::metadata(abs.as_std_path()) {
                Ok(md) => md.len() > cap as u64,
                Err(_) => false,
            },
            (None, _) => false,
        };
        if exceeds_cap {
            let cap = self.config.max_bytes.unwrap_or(0);
            return Err(Error::Invalid(format!(
                "file vượt trần {} bytes: {}",
                cap, entry.path
            )));
        }

        match limit {
            // Chỉ đọc N byte đầu — magic bytes binary cần đúng 4 byte, không
            // phải nạp cả file (file nhị phân có thể hàng trăm MB).
            Some(n) => {
                let mut f = std::fs::File::open(abs.as_std_path())?;
                let mut buf = vec![0u8; n];
                let mut filled = 0usize;
                while filled < n {
                    match f.read(&mut buf[filled..]) {
                        Ok(0) => break, // EOF
                        Ok(k) => filled += k,
                        Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(Error::Io(e)),
                    }
                }
                buf.truncate(filled);
                Ok(buf)
            }
            None => Ok(std::fs::read(abs.as_std_path())?),
        }
    }

    /// Bảo đảm có file thật trên đĩa (blocking). Đĩa local thì đường dẫn gốc đã
    /// là path thật — chỉ cần kiểm tra tồn tại.
    pub fn materialize_blocking(&self, entry: &SourceEntry) -> Result<Utf8PathBuf> {
        let abs = self.config.root.join(&entry.path);
        if abs.is_file() {
            Ok(abs)
        } else {
            Err(Error::Invalid(format!("không phải file: {}", abs)))
        }
    }
}

#[async_trait]
impl Source for DiskSource {
    fn kind(&self) -> SourceKind {
        self.config.kind
    }

    fn root(&self) -> &Utf8Path {
        &self.config.root
    }

    fn config(&self) -> &SourceConfig {
        &self.config
    }

    async fn list(&self) -> Result<Vec<SourceEntry>> {
        self.list_blocking()
    }

    async fn read(&self, entry: &SourceEntry, limit: Option<usize>) -> Result<Vec<u8>> {
        self.read_blocking(entry, limit)
    }

    async fn materialize(&self, entry: &SourceEntry) -> Result<Utf8PathBuf> {
        self.materialize_blocking(entry)
    }
}
