//! Per-language parser modules.

pub mod common;
pub mod effects;
pub mod flows;

/// Parser hybrid cho ngôn ngữ host + script nhúng (Svelte, Astro) — grammar
/// host chỉ parse markup, script block re-parse bằng JS/TS spec.
pub mod embedded;

#[cfg(feature = "lang-astro")]
pub mod astro;

#[cfg(feature = "lang-c")]
pub mod c;
#[cfg(feature = "lang-cpp")]
pub mod cpp;
#[cfg(feature = "lang-csharp")]
pub mod csharp;
#[cfg(feature = "lang-go")]
pub mod go;
#[cfg(feature = "lang-java")]
pub mod java;
#[cfg(feature = "lang-javascript")]
pub mod javascript;
#[cfg(feature = "lang-lua")]
pub mod lua;
#[cfg(feature = "lang-php")]
pub mod php;
#[cfg(feature = "lang-python")]
pub mod python;
#[cfg(feature = "lang-ruby")]
pub mod ruby;
#[cfg(feature = "lang-rust")]
pub mod rust;
#[cfg(feature = "lang-scala")]
pub mod scala;
#[cfg(feature = "lang-svelte")]
pub mod svelte;
#[cfg(feature = "lang-swift")]
pub mod swift;
#[cfg(feature = "lang-typescript")]
pub mod typescript;
