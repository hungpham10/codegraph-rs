//! Tree-sitter extractor — parse từng file ra `ParseResult` cho `GraphIndex::ingest`.
//!
//! Mọi ngôn ngữ chạy chung một generic engine (`languages::common::run_spec`)
//! được khai báo qua `LangSpec` (declaration nodes, call rules, marker rules).
//! Pipeline 2 pass: symbol pass (id local ≥ `SYMBOL_BASE`, scope stack) → chain
//! pass (marker + placeholder 0 + CallRecord). Resolve call được `GraphIndex`
//! làm sau khi `ingest` gom toàn bộ file.

pub mod config;
pub mod languages;
mod orchestrator;
mod walker;

pub use orchestrator::{ExtractStats, Orchestrator};
pub use config::{ExtractConfig, HeaderLanguage, DEFAULT_CONFIG_TOML};

use codegraph_core::{Error, Result};
use codegraph_graph::ParseResult;
use std::sync::Arc;

/// Parser một ngôn ngữ — mỗi ngôn ngữ là một `LangSpec` + wrap struct.
pub trait LangParser: Send + Sync {
    fn name(&self) -> &'static str;
    fn extensions(&self) -> &'static [&'static str];
    fn ts_language(&self) -> tree_sitter::Language;
    fn parse_file(&self, path: &str, source: &str) -> Result<ParseResult>;
}

/// Registry toàn bộ parser theo feature flags.
pub fn registry() -> Vec<Arc<dyn LangParser>> {
    let mut v: Vec<Arc<dyn LangParser>> = Vec::new();
    #[cfg(feature = "lang-typescript")]
    {
        v.push(Arc::new(languages::typescript::TypeScriptParser::new()));
        v.push(Arc::new(languages::typescript::TsxParser::new()));
    }
    #[cfg(feature = "lang-javascript")]
    v.push(Arc::new(languages::javascript::JavaScriptParser::new()));
    #[cfg(feature = "lang-python")]
    v.push(Arc::new(languages::python::PythonParser::new()));
    #[cfg(feature = "lang-rust")]
    v.push(Arc::new(languages::rust::RustParser::new()));
    #[cfg(feature = "lang-go")]
    v.push(Arc::new(languages::go::GoParser::new()));
    #[cfg(feature = "lang-java")]
    v.push(Arc::new(languages::java::JavaParser::new()));
    #[cfg(feature = "lang-c")]
    v.push(Arc::new(languages::c::CParser::new()));
    #[cfg(feature = "lang-cpp")]
    v.push(Arc::new(languages::cpp::CppParser::new()));
    #[cfg(feature = "lang-csharp")]
    v.push(Arc::new(languages::csharp::CSharpParser::new()));
    #[cfg(feature = "lang-ruby")]
    v.push(Arc::new(languages::ruby::RubyParser::new()));
    #[cfg(feature = "lang-php")]
    v.push(Arc::new(languages::php::PhpParser::new()));
    #[cfg(feature = "lang-scala")]
    v.push(Arc::new(languages::scala::ScalaParser::new()));
    #[cfg(feature = "lang-swift")]
    v.push(Arc::new(languages::swift::SwiftParser::new()));
    #[cfg(feature = "lang-lua")]
    v.push(Arc::new(languages::lua::LuaParser::new()));
    v
}

/// Tạo một `LangParser` wrapper quanh một `&'static LangSpec`.
///
/// Dạng 1 đối số: lấy name/extensions/ts_language từ chính `SPEC`. Dạng 5 đối số
/// cho phép override (VD TSX: cùng SPEC nhưng tên `tsx`, extension `.tsx`).
#[macro_export]
macro_rules! lang_parser {
    ($ty:ident, $spec:expr) => {
        $crate::lang_parser!(
            $ty,
            $spec,
            $spec.language_name,
            $spec.extensions,
            $spec.ts_language
        );
    };
    ($ty:ident, $spec:expr, $name:expr, $ext:expr, $ts:expr) => {
        #[derive(Clone, Copy)]
        pub struct $ty;

        impl $ty {
            pub fn new() -> Self {
                Self
            }
        }

        impl Default for $ty {
            fn default() -> Self {
                Self
            }
        }

        impl $crate::LangParser for $ty {
            fn name(&self) -> &'static str {
                $name
            }
            fn extensions(&self) -> &'static [&'static str] {
                $ext
            }
            fn ts_language(&self) -> tree_sitter::Language {
                ($ts)()
            }
            fn parse_file(&self, path: &str, source: &str) -> codegraph_core::Result<codegraph_graph::ParseResult> {
                $crate::languages::common::run_spec(&$spec, path, $name, source)
            }
        }
    };
}

pub(crate) fn parse_err(s: impl Into<String>) -> Error {
    Error::Parse(s.into())
}
