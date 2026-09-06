//! Scanner file nhị phân trong workspace (dựa vào magic bytes).
use camino::{Utf8Path, Utf8PathBuf};
use ignore::WalkBuilder;

/// Các magic bytes nhận diện binary: ELF, PE (MZ), Mach-O, fat Mach-O.
const MAGICS: &[&[u8]] = &[
    b"\x7fELF",          // ELF
    b"MZ",               // PE / DOS
    b"\xfe\xed\xfa\xce", // Mach-O little
    b"\xcf\xfa\xed\xfe", // Mach-O big
    b"\xca\xfe\xba\xbe", // fat Mach-O
];

/// Duyệt `root` (cùng ignore rules với walker) trả về các file nhị phân.
pub fn find_binaries(root: &Utf8Path) -> Vec<Utf8PathBuf> {
    let mut out = Vec::new();
    let walker = WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .parents(true)
        .add_custom_ignore_filename(".codegraphignore")
        .build();
    for entry in walker.flatten() {
        let path = entry.path();
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let bytes = match std::fs::read(path) {
            Ok(b) if b.len() >= 4 => b,
            _ => continue,
        };
        if MAGICS.iter().any(|m| bytes.starts_with(m)) {
            out.push(Utf8PathBuf::from_path_buf(path.to_path_buf()).unwrap());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(found[0].ends_with("app"));
    }
}
