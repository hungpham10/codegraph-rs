//! Source layer — đường **đọc** dữ liệu đầu vào cho mọi dịch vụ ingest.
//!
//! Đối xứng với [`codegraph_graph::storage::Storage`] (đường *ghi*): cùng hình
//! dạng (`#[async_trait]`, `dyn`, có impl mặc định + impl thật), nhưng phía
//! đọc. Trước crate này, phần đọc rải rác và gọi thẳng `std::fs` /
//! `ignore::WalkBuilder` ở 3 crate khác nhau — cấu hình traversal còn bị copy
//! y hệt 3 lần.
//!
//! # Ba method, ba lý do
//!
//! - [`Source::list`] — discovery. Chính sách lọc **diễn giải theo
//!   [`SourceKind`]**: `Code` lọc theo extension, `Document` lọc theo glob,
//!   `Binary` không lọc (magic bytes quyết định sau).
//! - [`Source::read`] — đọc in-memory, tối đa `limit` byte đầu.
//! - [`Source::materialize`] — bảo đảm có file **thật trên đĩa**. Chỉ dùng
//!   cho binary: radare2 là tiến trình ngoài và tự mở file bằng path hệ
//!   thống (`R2Pipe::spawn(path)`), không nhận bytes qua stdin — nên bước này
//!   không thể abstract hoá sau `read()`.
//!
//! # Quy ước quan trọng: `SourceEntry.path` luôn TƯƠNG ĐỐI so với root
//!
//! Điều này là điều kiện để đường đọc lúc *query* tái dựng được entry mà không
//! cần đổi schema persist. `Symbol.file` được lưu dạng `<root>/<rel>`; khi cần
//! đọc lại nội dung (vd `include_source` của context builder) thì tách `root`
//! là ra là có ngay `SourceEntry`. Nếu `path` là absolute thì thông tin source
//! bị mất vĩnh viễn và phải thêm cột `source_id` — nên quy ước này ràng buộc
//! từ đầu.
//!
//! # Tách nhiều tầng, không một trait to
//!
//! `Source` chỉ là trait **gộp**: `SourceInfo` (nhận diện + policy) →
//! `SourceListing` (discovery) / `SourceReader` (đọc bytes) /
//! `SourceMaterializer` (file thật cho binary). Provider mới chỉ implement
//! tầng nó cần, còn chỗ đã dùng `&dyn Source` thì không phải sửa. Cùng cách
//! `Storage` trong `codegraph-graph` tách theo nhóm thao tác.

use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use codegraph_core::{Error, Result};

mod disk;
mod registry;

pub use disk::{disk_walker, DiskSource};
pub use registry::{SourceConfig, SourceKind, SourceRegistry};

/// Một entry do Source liệt kê ra.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEntry {
    /// Đường dẫn **tương đối so với `Source::root()`** — xem module docs.
    pub path: Utf8PathBuf,
    /// Kích thước byte nếu provider biết trước (`None` = không biết, ví dụ
    /// khi listing từ xa mà API không trả size).
    pub size: Option<u64>,
    /// mtime (giây kể từ epoch) nếu provider biết trước. `None` = không biết
    /// (provider từ xa có thể không trả mtime) — caller tự chịu trách nhiệm về
    /// cache invalidation.
    ///
    /// Có sẵn **miễn phí**: `DiskSource::list_blocking` đã gọi `metadata` để lấy
    /// `size` nên mtime là syscall đi chung. Nhờ vậy `cache_path` của binary
    /// không phải tự gọi `fs::metadata` (I/O nằm ngoài trait), và 3 lần tra
    /// cache cũng không còn 3 syscall mỗi lần.
    pub mtime: Option<u64>,
}

impl SourceEntry {
    /// Dựng entry chỉ từ path tương đối — dùng ở đường *query* khi đã có
    /// `Symbol.file` và tách được `root`.
    pub fn new(path: impl Into<Utf8PathBuf>) -> Self {
        Self {
            path: path.into(),
            size: None,
            mtime: None,
        }
    }

    /// Entry đầy đủ — dùng khi provider biết cả size và mtime (`DiskSource`).
    pub fn with_stat(path: impl Into<Utf8PathBuf>, size: Option<u64>, mtime: Option<u64>) -> Self {
        Self {
            path: path.into(),
            size,
            mtime,
        }
    }

    /// Path tương đối dạng `&str` — `ParseResult.path` / `FileInfo.path` dùng
    /// `String` nên đây là cầu nối.
    pub fn path_str(&self) -> &str {
        self.path.as_str()
    }
}

/// Nhận diện + cấu hình — tầng nền, mọi tầng đọc đều cần để biết mình phục vụ
/// cái gì và policy ra sao.
pub trait SourceInfo {
    /// Dịch vụ nguồn này phục vụ.
    fn kind(&self) -> SourceKind;

    /// Root của nguồn — tiền tố của mọi `SourceEntry.path`.
    fn root(&self) -> &Utf8Path;

    /// Cấu hình đã resolve (sau khi áp default + override).
    fn config(&self) -> &SourceConfig;
}

/// Tầng **discovery** — liệt kê ra entry để xử lý.
#[allow(clippy::double_must_use)] // async_trait sinh `must_use` trùng (clippy 1.99)
#[async_trait]
pub trait SourceListing: SourceInfo {
    /// Liệt kê entry, đã áp filter theo `kind()`. Thứ tự không đảm bảo.
    async fn list(&self) -> Result<Vec<SourceEntry>>;
}

/// Tầng **đọc nội dung**.
#[allow(clippy::double_must_use)] // async_trait sinh `must_use` trùng (clippy 1.99)
#[async_trait]
pub trait SourceReader: SourceInfo {
    /// Đọc nội dung: tối đa `limit` byte đầu, `None` = đọc hết.
    ///
    /// Nếu vượt `config().max_bytes` thì trả `Err` **trước khi** đọc (stat
    /// trước) — caller tự quyết định skip hay báo lỗi. Việc này giữ luật
    /// "file quá lớn → skip" ở tầng service, không nhét vào trait.
    async fn read(&self, entry: &SourceEntry, limit: Option<usize>) -> Result<Vec<u8>>;

    /// Đọc file **có thể không tồn tại**: `Ok(None)` = không có file.
    ///
    /// Tách khỏi [`SourceReader::read`] vì config là đường đọc *tuỳ chọn* —
    /// thiếu `.codegraph/config.toml` là chuyện bình thường. Nếu bắt caller tự
    /// `match Err(_)` thì cùng một thói quen nuốt lỗi bị lặp ở mọi nơi.
    ///
    /// Chỉ `NotFound` mới thành `None`; lỗi I/O khác (permission, disk lỗi) vẫn
    /// là `Err` để không giấu sự cố thật.
    async fn read_optional(
        &self,
        entry: &SourceEntry,
        limit: Option<usize>,
    ) -> Result<Option<Vec<u8>>> {
        match self.read(entry, limit).await {
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            other => other.map(Some),
        }
    }
}

/// Tầng **file thật trên đĩa** — chỉ binary mới cần (radare2 là tiến trình
/// ngoài, tự mở file bằng path hệ thống nên không abstract hoá được sau `read`).
#[allow(clippy::double_must_use)] // async_trait sinh `must_use` trùng (clippy 1.99)
#[async_trait]
pub trait SourceMaterializer: SourceInfo {
    /// Bảo đảm có file thật trên đĩa, trả path cục bộ để đưa cho công cụ ngoài
    /// (radare2). `DiskSource` trả chính path gốc — không tốn gì.
    ///
    /// Mặc định: lỗi. Provider không phục vụ `Binary` không cần override.
    async fn materialize(&self, entry: &SourceEntry) -> Result<Utf8PathBuf> {
        let _ = entry;
        Err(Error::Invalid(format!(
            "source {:?} không hỗ trợ materialize — cần file thật trên đĩa",
            self.kind()
        )))
    }
}

/// Trait gộp — phần lớn nơi chỉ cần cái này.
///
/// Tách 4 tầng ở trên (thay vì một trait 6 method) là để **giảm áp lực khi
/// thêm provider**: provider tài liệu chỉ cần `SourceInfo + SourceListing +
/// SourceReader`, không phải implement `materialize`. Cùng cách `Storage` tách
/// `CategoryStorage` / `NodeMetaStorage` / … trong `codegraph-graph`.
pub trait Source: SourceListing + SourceReader + SourceMaterializer + Send + Sync {}

/// Đường dẫn **quy ước** của file config dự án, tương đối so với root.
///
/// Nằm ở đây vì cả `codegraph-extract` và `codegraph-sboxes` đều đọc đúng một
/// file này — quy ước phải có một chỗ duy nhất, không phải hai chuỗi rời.
pub const CONFIG_REL_PATH: &str = ".codegraph/config.toml";

/// Trần đọc config (8 MiB) — lớn hơn mọi config hợp lý, nhỏ hơn nhiều so với
/// RAM một tiến trình server. Trước đây `fs::read_to_string` không trần.
pub const CONFIG_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Nối 1 entry với provider cụ thể — dùng ở đường *query* khi đã biết
/// `SourceEntry` nhưng cần truy cập nó.
pub async fn read_entry(
    source: &dyn Source,
    entry: &SourceEntry,
    limit: Option<usize>,
) -> Result<Vec<u8>> {
    source.read(entry, limit).await
}

/// Dựng `SourceEntry` từ `Symbol.file` + `root` mà không cần biết provider.
///
/// Đây là hàm mấu chốt của đường query: `file` lưu dạng `<root>/<rel>`, nên tách
/// `root` là ra là có entry. Trả `None` nếu `file` không nằm dưới `root` — khi
/// đó index được tạo từ nguồn khác và caller phải xử lý riêng.
pub fn entry_from_symbol_file(file: &str, root: &Utf8Path) -> Option<SourceEntry> {
    let path = Utf8Path::new(file);
    let rel = path.strip_prefix(root).ok()?;
    if rel.as_str().is_empty() {
        return None;
    }
    Some(SourceEntry::new(rel))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn write(root: &Utf8Path, rel: &str, bytes: &[u8]) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent.as_std_path()).unwrap();
        }
        let mut f = std::fs::File::create(p.as_std_path()).unwrap();
        f.write_all(bytes).unwrap();
    }

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn root_of(d: &tempfile::TempDir) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(d.path().to_path_buf()).unwrap()
    }

    #[tokio::test]
    async fn code_source_filters_by_extension() {
        let d = tmp();
        let root = root_of(&d);
        write(&root, "a.rs", b"fn a() {}");
        write(&root, "b.go", b"package main");
        write(&root, "notes.md", b"# hi");
        // .gitignore phải được tôn trọng (khớp hành vi `walk` cũ).
        // `ignore` chỉ áp `.gitignore` bên trong git repo (`require_git` mặc
        // định = true) — nên phải tạo `.git` trước.
        std::fs::create_dir(root.join(".git").as_std_path()).unwrap();
        write(&root, ".gitignore", b"ignored.rs\n");
        write(&root, "ignored.rs", b"fn ignored() {}");

        let src = DiskSource::of_kind(SourceKind::Code, root.clone(), &["rs", "go"]);
        let mut paths: Vec<String> = src
            .list()
            .await
            .unwrap()
            .iter()
            .map(|e| e.path_str().to_string())
            .collect();
        paths.sort();
        assert_eq!(paths, vec!["a.rs", "b.go"], "phải lọc ext + .gitignore");
    }

    #[tokio::test]
    async fn read_prefix_chỉ_đọc_n_byte_đầu() {
        let d = tmp();
        let root = root_of(&d);
        // 1 MB payload, magic bytes ở 4 byte đầu.
        let mut payload = b"\x7fELF".to_vec();
        // `resize` thay vì `repeat().take()` — `repeat_n` cần Rust 1.82, repo
        // khai MSRV 1.80.
        payload.resize(1024 * 1024, b'x');
        write(&root, "app", &payload);

        let src = DiskSource::new(SourceConfig::for_kind(SourceKind::Binary, root.clone()));
        let entry = SourceEntry::new("app");
        let head = src.read(&entry, Some(4)).await.unwrap();
        assert_eq!(head, b"\x7fELF", "phải đúng 4 byte đầu");
        assert_eq!(head.len(), 4);
    }

    #[tokio::test]
    async fn read_cap_chặn_trước_khi_nạp_hết_file() {
        let d = tmp();
        let root = root_of(&d);
        write(&root, "big.txt", &vec![b'x'; 4096]);

        let mut cfg = SourceConfig::for_kind(SourceKind::Document, root.clone());
        cfg.max_bytes = Some(1024);
        let src = DiskSource::new(cfg);
        let err = src
            .read(&SourceEntry::new("big.txt"), None)
            .await
            .unwrap_err();
        assert!(
            format!("{err}").contains("vượt trần"),
            "phải báo vượt trần, không phải lỗi khác: {err}"
        );
    }

    #[tokio::test]
    async fn document_source_dùng_glob_include() {
        let d = tmp();
        let root = root_of(&d);
        write(&root, "cfg/app.yaml", b"a: 1");
        write(&root, "cfg/app.json", b"{\"a\":1}");
        write(&root, "cfg/skip.txt", b"nope");

        let mut cfg = SourceConfig::for_kind(SourceKind::Document, root.clone());
        cfg.include = vec!["**/*.json".to_string()];
        let src = DiskSource::new(cfg);
        let mut paths: Vec<String> = src
            .list()
            .await
            .unwrap()
            .iter()
            .map(|e| e.path_str().to_string())
            .collect();
        paths.sort();
        assert_eq!(paths, vec!["cfg/app.json"]);
    }

    #[tokio::test]
    async fn exclude_thắng_include() {
        let d = tmp();
        let root = root_of(&d);
        write(&root, "keep.rs", b"fn a() {}");
        write(&root, "vendor/lib.rs", b"fn b() {}");

        let mut cfg = SourceConfig::for_kind(SourceKind::Code, root.clone());
        cfg.include = vec!["rs".to_string()];
        cfg.exclude = vec!["vendor/**".to_string()];
        let src = DiskSource::new(cfg);
        let mut paths: Vec<String> = src
            .list()
            .await
            .unwrap()
            .iter()
            .map(|e| e.path_str().to_string())
            .collect();
        paths.sort();
        assert_eq!(paths, vec!["keep.rs"]);
    }

    #[tokio::test]
    async fn materialize_trả_path_thật_và_không_tốn_gì() {
        let d = tmp();
        let root = root_of(&d);
        write(&root, "app", b"\x7fELF\x02\x01\x01\x00");

        let src = DiskSource::new(SourceConfig::for_kind(SourceKind::Binary, root.clone()));
        let entry = SourceEntry::new("app");
        let local = src.materialize(&entry).await.unwrap();
        assert!(local.is_file());
        assert!(local.starts_with(&root), "phải là path tuyệt đối trên đĩa");
        // radare2 cần đúng path này.
        assert_eq!(std::fs::read(local.as_std_path()).unwrap().len(), 8);
    }

    #[tokio::test]
    async fn materialize_lỗi_khi_file_không_tồn_tại() {
        let d = tmp();
        let src = DiskSource::new(SourceConfig::for_kind(SourceKind::Binary, root_of(&d)));
        assert!(src.materialize(&SourceEntry::new("nope")).await.is_err());
    }

    // ── Đường query: dựng lại entry từ `Symbol.file` ──────────────────

    #[test]
    fn entry_from_symbol_file_tách_root() {
        let root = Utf8Path::new("/repo");
        let e = entry_from_symbol_file("/repo/src/main.rs", root).unwrap();
        assert_eq!(e.path_str(), "src/main.rs");
    }

    #[test]
    fn entry_from_symbol_file_trả_none_khi_ngoài_root() {
        let root = Utf8Path::new("/repo");
        assert!(entry_from_symbol_file("/elsewhere/x.rs", root).is_none());
        assert!(
            entry_from_symbol_file("/repo", root).is_none(),
            "root trần không phải file"
        );
    }

    // ── Tầng config: đọc file optional ────────────────────────────────

    #[test]
    fn config_thiếu_thì_trả_none_không_phải_lỗi() {
        let d = tmp();
        let src = DiskSource::for_config(root_of(&d));
        assert_eq!(src.read_config_blocking().unwrap(), None);
    }

    #[test]
    fn config_đọc_đúng_đường_dẫn_quy_ước_kể_cả_khi_hidden() {
        let d = tmp();
        let root = root_of(&d);
        // `.codegraph/` bị `list()` bỏ qua (hidden + gitignore) nhưng config
        // vẫn phải đọc được — đó là lý do đường này không qua discovery.
        write(&root, CONFIG_REL_PATH, b"[sandbox]\nloop_cap = 3\n");
        let src = DiskSource::for_config(root);
        assert!(src.list_blocking().unwrap().is_empty(), "list phải bỏ qua");

        let bytes = src.read_config_blocking().unwrap().expect("phải đọc được");
        assert!(String::from_utf8(bytes).unwrap().contains("loop_cap = 3"));
    }

    #[tokio::test]
    async fn read_optional_trả_some_và_dispatch_được_thông_trait_object() {
        let d = tmp();
        let root = root_of(&d);
        write(&root, "a.toml", b"k = v\n");
        let src = DiskSource::for_config(root);
        let dyn_src: &dyn Source = &src;

        let got = dyn_src
            .read_optional(&SourceEntry::new("a.toml"), None)
            .await
            .unwrap();
        assert_eq!(got.as_deref(), Some(&b"k = v\n"[..]));
        // Method của `SourceInfo` cũng phải gọi được qua `dyn Source` — đường
        // query (`context`) phụ thuộc đúng điều này.
        assert_eq!(dyn_src.kind(), SourceKind::Code);
        let missing = dyn_src
            .read_optional(&SourceEntry::new("missing"), None)
            .await
            .unwrap();
        assert_eq!(missing, None);
    }

    // ── Registry ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn registry_tra_theo_kind() {
        let d = tmp();
        let root = root_of(&d);
        let mut reg = SourceRegistry::new();
        reg.register(
            "repo",
            std::sync::Arc::new(DiskSource::of_kind(SourceKind::Code, root.clone(), &["rs"])),
        );
        reg.register(
            "docs",
            std::sync::Arc::new(DiskSource::new(SourceConfig::for_kind(
                SourceKind::Document,
                root.clone(),
            ))),
        );

        assert_eq!(
            reg.source_for(SourceKind::Code).map(|s| s.kind()),
            Some(SourceKind::Code)
        );
        assert_eq!(
            reg.source_for(SourceKind::Document).map(|s| s.kind()),
            Some(SourceKind::Document)
        );
        assert!(
            reg.source_for(SourceKind::Binary).is_none(),
            "không có thì None"
        );
        assert!(reg.by_name("docs").is_some());
        assert_eq!(reg.iter().count(), 2);
    }

    #[test]
    fn source_kind_parse_roundtrip() {
        for k in [SourceKind::Code, SourceKind::Binary, SourceKind::Document] {
            assert_eq!(SourceKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(SourceKind::parse("nope"), None);
    }
}
