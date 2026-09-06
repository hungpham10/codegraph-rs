//! Cache kết quả phân tích binary theo (path, mtime, size).
use crate::config::BinaryConfig;
use camino::Utf8Path;
use codegraph_graph::ParseResult;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

pub fn cache_path(root: &Utf8Path, path: &Path) -> camino::Utf8PathBuf {
    let key = format!("{}|{}|{}", path.display(), mtime(path), size(path));
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

pub fn is_cached(root: &Utf8Path, path: &Path, cfg: &BinaryConfig) -> bool {
    if !cfg.cache {
        return false;
    }
    let p = cache_path(root, path);
    p.exists()
}

fn mtime(path: &Path) -> u64 {
    fs::metadata(path)
        .map(|m| {
            m.modified()
                .map(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                })
                .unwrap_or(0)
        })
        .unwrap_or(0)
}
fn size(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
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
        let p = cache_path(&root, std::path::Path::new("/bin/ls"));
        store(&p, &result).unwrap();
        let loaded = load(&p).unwrap();
        assert_eq!(loaded.path, result.path);
        assert_eq!(loaded.bytes, result.bytes);
    }
}
