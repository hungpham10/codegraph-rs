//! Scanner file nhị phân trong workspace (dựa vào magic bytes).
//!
//! Traversal dùng chung `DiskSource` (`SourceKind::Binary`) nên không còn bản
//! `WalkBuilder` thứ ba cùng cấu hình — trước đây chính sách ignore bị copy ở
//! `extract/src/walker.rs`, `extract/src/config.rs` và đây.
use camino::Utf8Path;
use codegraph_source::{DiskSource, SourceConfig, SourceEntry, SourceKind};

/// Các magic bytes nhận diện binary: ELF, PE (MZ), Mach-O, fat Mach-O.
const MAGICS: &[&[u8]] = &[
    b"\x7fELF",          // ELF
    b"MZ",               // PE / DOS
    b"\xfe\xed\xfa\xce", // Mach-O little
    b"\xcf\xfa\xed\xfe", // Mach-O big
    b"\xca\xfe\xba\xbe", // fat Mach-O
];

/// Duyệt `root` (cùng ignore rules với walker) trả về các file nhị phân.
///
/// Chỉ đọc **4 byte đầu** mỗi file để check magic bytes. Trước đây gọi
/// `std::fs::read` — tức đọc và cấp phát **cả file** chỉ để so 4 byte, lãng phí
/// hàng trăm MB trên binary lớn.
pub fn find_binaries(root: &Utf8Path) -> Vec<SourceEntry> {
    let src = DiskSource::new(SourceConfig::for_kind(
        SourceKind::Binary,
        root.to_path_buf(),
    ));
    find_binaries_in(&src)
}

/// Như [`find_binaries`] nhưng đọc từ một `DiskSource` cho sẵn — dùng khi
/// caller đã dựng sẵn source (chia sẻ config/cache) thay vì dựng lại.
pub fn find_binaries_in(src: &DiskSource) -> Vec<SourceEntry> {
    let Ok(entries) = src.list_blocking() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries {
        if is_binary_magic(src, &entry) {
            // Giữ nguyên `SourceEntry` (kèm size/mtime) — `cache_path` dùng mtime
            // làm key nên trả về `Utf8PathBuf` sẽ vứt mất thông tin này.
            out.push(entry);
        }
    }
    out
}

/// 4 byte đầu có khớp magic nào không.
fn is_binary_magic(src: &DiskSource, entry: &SourceEntry) -> bool {
    let head = match src.read_blocking(entry, Some(4)) {
        Ok(b) if b.len() >= 4 => b,
        _ => return false,
    };
    MAGICS.iter().any(|m| head.starts_with(m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use std::io::Write;

    #[test]
    fn skips_source_and_finds_elf() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let mut f = std::fs::File::create(root.join("src.rs")).unwrap();
        f.write_all(b"fn main() {}").unwrap();
        let mut elf = std::fs::File::create(root.join("app")).unwrap();
        elf.write_all(b"\x7fELF\x02\x01\x01\x00").unwrap();
        let found = find_binaries(&root);
        assert_eq!(found.len(), 1);
        // `SourceEntry.path` tương đối so với root.
        assert_eq!(found[0].path.as_str(), "app");
    }

    /// `list_blocking` phải lấp sẵn stat (size + mtime) từ syscall `metadata`
    /// mà nó vốn đã gọi — đây là nguồn của cache key ở `cache.rs`.
    #[test]
    fn entry_có_sẵn_size_và_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let mut elf = std::fs::File::create(root.join("app")).unwrap();
        elf.write_all(b"\x7fELF\x02\x01\x01\x00").unwrap();
        drop(elf);

        let found = find_binaries(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].size, Some(8), "stat lấy đúng size");
        assert!(found[0].mtime.is_some(), "stat phải kèm mtime");
    }
}
