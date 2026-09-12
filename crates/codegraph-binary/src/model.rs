//! Các struct JSON lenient khi parse output r2.
//! Mọi field là `Option` vì schema r2 thay đổi theo version.

use serde::Deserialize;

/// Metadata binary từ lệnh `ij`.
#[derive(Debug, Deserialize, Default)]
pub struct BinInfo {
    pub core: Option<CoreInfo>,
    pub bin: Option<BinMeta>,
}

#[derive(Debug, Deserialize, Default)]
pub struct CoreInfo {
    pub format: Option<String>,
    pub arch: Option<String>,
    pub bits: Option<u32>,
    pub os: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct BinMeta {
    pub arch: Option<String>,
    pub bits: Option<u32>,
    pub os: Option<String>,
    pub lang: Option<String>,
    pub compiler: Option<String>,
    pub machine: Option<String>,
    pub libs: Option<Vec<String>>,
    pub imports: Option<u64>,
    pub symbols: Option<u64>,
    pub entries: Option<u64>,
    pub sections: Option<u64>,
}

/// Danh sách function từ `aflj`.
#[derive(Debug, Deserialize)]
pub struct FnEntry {
    /// r2 6.x trả `addr`; bản cũ trả `offset`.
    #[serde(alias = "offset")]
    pub addr: Option<u64>,
    pub name: Option<String>,
    pub size: Option<u64>,
    pub realsz: Option<u64>,
    pub nbbs: Option<u64>,
    pub edges: Option<u64>,
    pub cc: Option<f64>,
    pub calltype: Option<String>,
    pub signature: Option<String>,
    pub nargs: Option<u32>,
    pub nlocals: Option<u32>,
    pub ninstrs: Option<u32>,
    pub is_noreturn: Option<bool>,
}

/// Một xref từ `axtj` / `axfj`.
#[derive(Debug, Deserialize)]
pub struct Xref {
    pub from: Option<u64>,
    pub to: Option<u64>,
    #[serde(rename = "type")]
    pub type_: Option<String>,
    pub fcn_addr: Option<u64>,
    pub fcn_name: Option<String>,
    pub refname: Option<String>,
    pub flag: Option<String>,
    pub opcode: Option<String>,
}

/// Entry import từ `iij`.
#[derive(Debug, Deserialize)]
pub struct ImportEntry {
    /// r2 6.x trả `name`; bản cũ trả `import`.
    #[serde(default, rename = "name", alias = "import")]
    pub import: Option<String>,
    pub ordinal: Option<u64>,
    pub bind: Option<String>,
    #[serde(rename = "type")]
    pub type_: Option<String>,
    pub lib: Option<String>,
    pub plt: Option<u64>,
}

/// Symbol từ `isj`.
#[derive(Debug, Deserialize)]
pub struct SymEntry {
    pub name: Option<String>,
    pub demname: Option<String>,
    pub ordinal: Option<u64>,
    pub bind: Option<String>,
    #[serde(rename = "type")]
    pub type_: Option<String>,
    pub size: Option<u64>,
    pub addr: Option<u64>,
    pub is_imported: Option<bool>,
}

/// Symbol xuất khẩu từ `iEj`.
#[derive(Debug, Deserialize)]
pub struct ExportEntry {
    pub name: Option<String>,
    pub vaddr: Option<u64>,
    pub paddr: Option<u64>,
    pub size: Option<u64>,
    pub bind: Option<String>,
    #[serde(rename = "type")]
    pub type_: Option<String>,
}

/// Entry point từ `iej` (entry addresses của executable).
#[derive(Debug, Deserialize)]
pub struct EntryPoint {
    pub vaddr: Option<u64>,
    pub paddr: Option<u64>,
    pub name: Option<String>,
}

/// String từ `izj` / `izzj`.
#[derive(Debug, Deserialize)]
pub struct StrEntry {
    pub vaddr: Option<u64>,
    pub paddr: Option<u64>,
    pub size: Option<u64>,
    pub length: Option<u64>,
    pub section: Option<String>,
    #[serde(rename = "type")]
    pub type_: Option<String>,
    pub string: Option<String>,
}

/// Node call graph từ `agCj` (r2 6.x): mỗi function kèm danh sách callee theo tên.
#[derive(Debug, Deserialize)]
pub struct CallGraphNode {
    pub name: Option<String>,
    pub imports: Option<Vec<String>>,
}

/// Một lệnh disasm trong `pdfj.ops`.
#[derive(Debug, Deserialize)]
pub struct DisasmOp {
    /// r2 6.x trả `addr`; bản cũ trả `offset`.
    #[serde(alias = "offset")]
    pub addr: Option<u64>,
    pub size: Option<u64>,
    pub esil: Option<String>,
    pub bytes: Option<String>,
    #[serde(rename = "type")]
    pub type_: Option<String>,
    pub disasm: Option<String>,
    pub ptr: Option<u64>,
    pub val: Option<u64>,
    pub refptr: Option<u64>,
    pub reference: Option<u64>,
    pub jump: Option<u64>,
    pub fail: Option<u64>,
    pub flag: Option<String>,
    pub true_: Option<bool>,
    pub false_: Option<bool>,
}

/// JSON gốc dạng `Value` cho phép linh hoạt.
pub type Json = serde_json::Value;
