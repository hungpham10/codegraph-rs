//! Cache kết quả phân tích binary theo (path, mtime, size).
//!
//! mtime/size lấy từ [`SourceEntry`] mà `DiskSource::list_blocking` đã lấp sẵn
//! — không tự gọi `fs::metadata` ở đây. Nhờ vậy I/O nằm trong crate
//! `codegraph-source` và provider không có stat cục bộ vẫn dùng được.
use crate::config::BinaryConfig;
use camino::Utf8Path;
use codegraph_graph::ParseResult;
use codegraph_source::SourceEntry;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

/// Bump khi format output extract thay đổi (vd: sửa mapping field r2) để
/// cache cũ từ bản binary trước tự vô hiệu thay vì được nạp lại nguyên si.
pub const EXTRACT_VERSION: &str = "2";

pub fn cache_path(root: &Utf8Path, path: &Path, entry: &SourceEntry) -> camino::Utf8PathBuf {
    let key = format!(
        "{EXTRACT_VERSION}|{}|{}|{}",
        path.display(),
        entry.mtime.unwrap_or(0),
        entry.size.unwrap_or(0),
    );
    let hash = Sha256::digest(key.as_bytes());
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    root.join(".codegraph")
        .join("binary-cache")
        .join(format!("{hex}.json"))
}

pub fn load(path: &camino::Utf8Path) -> Option<ParseResult> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn store(path: &camino::Utf8Path, result: &ParseResult) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string(result).unwrap())
}

pub fn is_cached(
    root: &Utf8Path,
    path: &Path,
    entry: &SourceEntry,
    cfg: &BinaryConfig,
) -> bool {
    if !cfg.cache {
        return false;
    }
    let p = cache_path(root, path, entry);
    p.exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let result = ParseResult {
            path: "/bin/ls".to_string(),
            language: "binary".to_string(),
            bytes: 1_000_000,
            lines: 0,
            symbols: vec![],
            chains: Default::default(),
            calls: vec![],
        };
        let entry = SourceEntry::with_stat("bin/ls", Some(1_000_000), Some(12345));
        let p = cache_path(&root, std::path::Path::new("/bin/ls"), &entry);
        store(&p, &result).unwrap();
        let loaded = load(&p).unwrap();
        assert_eq!(loaded.path, result.path);
        assert_eq!(loaded.bytes, result.bytes);
    }

    /// mtime/size phải thực sự đổi key — nếu không, binary bị sửa mà cache vẫn
    /// hit thì trả kết quả cũ (đây là lý do stat nằm trong key).
    #[test]
    fn key_đổi_khi_mtime_hoặc_size_đổi() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let path = std::path::Path::new("/bin/ls");
        let a = SourceEntry::with_stat("bin/ls", Some(100), Some(1));
        let b = SourceEntry::with_stat("bin/ls", Some(100), Some(2));
        let c = SourceEntry::with_stat("bin/ls", Some(200), Some(1));
        assert_ne!(
            cache_path(&root, path, &a),
            cache_path(&root, path, &b),
            "mtime đổi phải đổi cache key"
        );
        assert_ne!(
            cache_path(&root, path, &a),
            cache_path(&root, path, &c),
            "size đổi phải đổi cache key"
        );
    }
}
