use crate::config::{self, ExtractConfig, HeaderLanguage};
use crate::LangParser;
use camino::{Utf8Path, Utf8PathBuf};
use codegraph_source::{DiskSource, SourceConfig, SourceEntry, SourceKind};
use std::collections::HashMap;
use std::sync::Arc;

pub struct FileMatch {
    pub path: Utf8PathBuf,
    pub parser: Arc<dyn LangParser>,
}

pub type ExtMap = HashMap<&'static str, Arc<dyn LangParser>>;

pub struct WalkOptions<'a> {
    pub config: &'a ExtractConfig,
    pub project_hint: Option<HeaderLanguage>,
    pub c_parser: Option<Arc<dyn LangParser>>,
    pub cpp_parser: Option<Arc<dyn LangParser>>,
    /// Đọc nội dung file (sniff header C/C++) — qua trait `Source` để đường
    /// ingest không tự gọi `std::fs`.
    pub source: &'a DiskSource,
}

pub fn build_ext_map(parsers: &[Arc<dyn LangParser>]) -> ExtMap {
    let mut ext_map: ExtMap = HashMap::new();
    for p in parsers {
        for e in p.extensions() {
            ext_map.insert(*e, p.clone());
        }
    }
    ext_map
}

fn find_parser<'a>(
    parsers: &'a [Arc<dyn LangParser>],
    lang: &str,
) -> Option<&'a Arc<dyn LangParser>> {
    parsers.iter().find(|p| p.name() == lang)
}

pub fn walk_options<'a>(
    parsers: &'a [Arc<dyn LangParser>],
    config: &'a ExtractConfig,
    source: &'a DiskSource,
) -> WalkOptions<'a> {
    let project_hint = if config.header_language == HeaderLanguage::Auto {
        config::detect_project_header_hint(source)
    } else {
        None
    };
    WalkOptions {
        config,
        project_hint,
        c_parser: find_parser(parsers, "c").cloned(),
        cpp_parser: find_parser(parsers, "cpp").cloned(),
        source,
    }
}

/// Traversal dùng chung policy ignore của `codegraph-source` — trước đây
/// `WalkBuilder` bị copy y hệt ở đây và `config::detect_project_header_hint`.
pub fn walk(
    root: &Utf8Path,
    parsers: &[Arc<dyn LangParser>],
    config: &ExtractConfig,
) -> Vec<FileMatch> {
    let ext_map = build_ext_map(parsers);
    let source = DiskSource::new(SourceConfig::for_kind(SourceKind::Code, root.to_path_buf()));
    // `list_blocking` đã lọc file + stat; ta chỉ cần extension để chọn parser.
    let Ok(entries) = source.list_blocking() else {
        return Vec::new();
    };
    let opts = walk_options(parsers, config, &source);

    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(ext) = entry.path.extension() else {
            continue;
        };
        let path = root.join(&entry.path);
        let parser = if ext == "h" {
            resolve_header_parser(&entry, &opts)
        } else {
            ext_map.get(ext).cloned()
        };
        let Some(parser) = parser else {
            continue;
        };
        out.push(FileMatch { path, parser });
    }
    out
}

fn resolve_header_parser(
    entry: &SourceEntry,
    opts: &WalkOptions<'_>,
) -> Option<Arc<dyn LangParser>> {
    let c = opts.c_parser.as_ref();
    let cpp = opts.cpp_parser.as_ref();

    match (c, cpp) {
        (None, None) => None,
        (Some(c), None) => Some(c.clone()),
        (None, Some(cpp)) => Some(cpp.clone()),
        (Some(c), Some(cpp)) => Some(resolve_header_with_both(entry, opts, c, cpp)),
    }
}

fn resolve_header_with_both(
    entry: &SourceEntry,
    opts: &WalkOptions<'_>,
    c: &Arc<dyn LangParser>,
    cpp: &Arc<dyn LangParser>,
) -> Arc<dyn LangParser> {
    match opts.config.header_language {
        HeaderLanguage::C => c.clone(),
        HeaderLanguage::Cpp => cpp.clone(),
        HeaderLanguage::Auto => {
            if let Some(hint) = opts.project_hint {
                return match hint {
                    HeaderLanguage::C => c.clone(),
                    HeaderLanguage::Cpp => cpp.clone(),
                    HeaderLanguage::Auto => unreachable!(),
                };
            }
            // Mixed C/C++ project: sniff file content.
            if header_looks_like_cpp(entry, opts.source) {
                cpp.clone()
            } else {
                c.clone()
            }
        }
    }
}

/// Sniff 8KB đầu header để đoán C++.
///
/// Đọc **đúng 8KB** qua `Source::read(Some(n))`, không phải `std::fs::read`
/// cả file rồi `&bytes[..min(8192)]` — trước đó file header lớn (header sinh
/// máy, header đệ quy kéo theo) bị nạp toàn bộ chỉ để xem 8KB.
const HEADER_SNIFF_BYTES: usize = 8192;

fn header_looks_like_cpp(entry: &SourceEntry, source: &DiskSource) -> bool {
    let Ok(bytes) = source.read_blocking(entry, Some(HEADER_SNIFF_BYTES)) else {
        return false;
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return false;
    };
    config::is_cpp_header(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry;
    use std::io::Write;

    fn write_file(dir: &Utf8Path, name: &str, content: &str) -> Utf8PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(path.as_std_path()).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn cpp_project_headers_use_cpp_parser() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        write_file(&root, "src/Foo.cpp", "class Foo {};\n");
        write_file(
            &root,
            "src/Foo.h",
            "#pragma once\nnamespace tnl { class Foo {}; }\n",
        );

        let parsers = registry();
        let config = ExtractConfig::default();
        let matches = walk(&root, &parsers, &config);
        let h = matches
            .iter()
            .find(|m| m.path.ends_with("Foo.h"))
            .expect("Foo.h should be indexed");
        assert_eq!(h.parser.name(), "cpp");
    }

    #[test]
    fn c_project_headers_use_c_parser() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        write_file(&root, "src/foo.c", "struct foo { int x; };\n");
        write_file(
            &root,
            "src/foo.h",
            "#ifndef FOO_H\n#define FOO_H\nstruct foo { int x; };\n#endif\n",
        );

        let parsers = registry();
        let config = ExtractConfig::default();
        let matches = walk(&root, &parsers, &config);
        let h = matches
            .iter()
            .find(|m| m.path.ends_with("foo.h"))
            .expect("foo.h should be indexed");
        assert_eq!(h.parser.name(), "c");
    }

    #[test]
    fn config_forces_cpp_headers() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        write_file(&root, "src/foo.c", "struct foo { int x; };\n");
        write_file(
            &root,
            "src/foo.h",
            "#ifndef FOO_H\n#define FOO_H\nstruct foo { int x; };\n#endif\n",
        );

        let parsers = registry();
        let config = ExtractConfig {
            header_language: HeaderLanguage::Cpp,
            effect_classifier: Default::default(),
            storage: Default::default(),
            embedding: Default::default(),
            ..Default::default()
        };
        let matches = walk(&root, &parsers, &config);
        let h = matches
            .iter()
            .find(|m| m.path.ends_with("foo.h"))
            .expect("foo.h should be indexed");
        assert_eq!(h.parser.name(), "cpp");
    }

    /// `detect_project_header_hint` giờ đọc qua `Source::list` — hành vi
    /// đếm `.c`/`.cpp` phải y hệt bản `WalkBuilder` trước đó.
    #[test]
    fn hint_nhận_diện_project_c_thuần_và_cpp_thuần() {
        use crate::config::detect_project_header_hint;

        let c_dir = tempfile::tempdir().unwrap();
        let c_root = Utf8PathBuf::from_path_buf(c_dir.path().to_path_buf()).unwrap();
        write_file(&c_root, "a.c", "int main(){}");
        write_file(&c_root, "b.c", "void f(){}");
        let c_src = DiskSource::new(SourceConfig::for_kind(SourceKind::Code, c_root));
        assert_eq!(detect_project_header_hint(&c_src), Some(HeaderLanguage::C));

        let cpp_dir = tempfile::tempdir().unwrap();
        let cpp_root = Utf8PathBuf::from_path_buf(cpp_dir.path().to_path_buf()).unwrap();
        write_file(&cpp_root, "a.cpp", "class A{};");
        write_file(&cpp_root, "b.hpp", "class B{};");
        let cpp_src = DiskSource::new(SourceConfig::for_kind(SourceKind::Code, cpp_root));
        assert_eq!(
            detect_project_header_hint(&cpp_src),
            Some(HeaderLanguage::Cpp)
        );

        // Mixed (cả .c và .cpp) → None, buộc phải sniff nội dung.
        let mixed_dir = tempfile::tempdir().unwrap();
        let mixed_root = Utf8PathBuf::from_path_buf(mixed_dir.path().to_path_buf()).unwrap();
        write_file(&mixed_root, "a.c", "int main(){}");
        write_file(&mixed_root, "b.cpp", "class B{};");
        let mixed_src = DiskSource::new(SourceConfig::for_kind(SourceKind::Code, mixed_root));
        assert_eq!(detect_project_header_hint(&mixed_src), None);
    }

    /// Sniff header phải đọc **đúng 8KB đầu**, không phải cả file: header lớn
    /// (>8KB) vẫn phải nhận diện đúng.
    #[test]
    fn sniff_chỉ_đọc_8kb_đầu_header_lớn() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        // >8KB: marker C++ nằm ở đầu, phần còn lại là comment filler.
        let mut big = String::from("namespace tnl { class Big {}; }\n");
        big.push_str(&"// filler padding padding padding padding padding\n".repeat(500));
        write_file(&root, "big.h", &big);
        assert!(
            big.len() > HEADER_SNIFF_BYTES,
            "fixture phải lớn hơn ngưỡng sniff, mới chứng minh được"
        );

        let src = DiskSource::new(SourceConfig::for_kind(SourceKind::Code, root.clone()));
        let entry = SourceEntry::new("big.h");
        assert!(
            header_looks_like_cpp(&entry, &src),
            "đọc 8KB đầu vẫn phải thấy marker ở đầu file"
        );
    }
}
