//! Semantic-graph model (semgraph-style) — id-space, symbols, chains, calls.
//!
//! Kiến trúc đích: mọi symbol được gán một unique id (global registry, bắt đầu
//! từ `SYMBOL_BASE`); call chain của một hàm là chuỗi `u64` gồm **marker** (mô
//! tả luồng điều khiển, id < `SYMBOL_BASE`) và **symbol id** của callee. Edge
//! `(caller, callee)` suy từ chain: mỗi symbol id trong chain là một callee.
//!
//! Model này thay thế `Node`/`Edge`/`NodeKind`/`EdgeKind` cũ (wire breaking —
//! đã chốt). Query surface (search/callers/callees/flow/...) nằm ở
//! `codegraph-graph`; ở đây chỉ là các kiểu dữ liệu + id-space.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ==================== Id-space ====================

/// Id bắt đầu cho symbol. Mọi id `< SYMBOL_BASE` là marker reserved.
pub const SYMBOL_BASE: u64 = 100;

/// Marker: bắt đầu loop body.
pub const MARKER_LOOP: u64 = 1;

/// Marker: recursive call (gọi lại chính function đang xét) — dự trữ.
pub const MARKER_REC_CALL: u64 = 2;

/// Marker: nhánh khi điều kiện đúng.
pub const MARKER_IF_TRUE: u64 = 3;

/// Marker: nhánh khi điều kiện sai.
pub const MARKER_IF_FALSE: u64 = 4;

/// Marker: kết thúc một nhánh if/else.
pub const MARKER_BRANCH_END: u64 = 5;

/// Marker: return statement.
pub const MARKER_RETURN: u64 = 6;

/// Marker: loop back edge (quay lại đầu loop).
pub const MARKER_LOOP_BACK: u64 = 7;

/// Marker: case trong switch.
pub const MARKER_SWITCH_CASE: u64 = 8;

/// Marker: kết thúc switch.
pub const MARKER_SWITCH_END: u64 = 9;

/// Marker: break statement.
pub const MARKER_BREAK: u64 = 10;

/// Marker: continue statement.
pub const MARKER_CONTINUE: u64 = 11;

/// Marker: throw/raise exception.
pub const MARKER_THROW: u64 = 12;

/// `true` nếu `id` là một fixed marker (nằm trong vùng reserved).
#[inline]
pub fn is_marker(id: u64) -> bool {
    id > 0 && id < SYMBOL_BASE
}

/// Tên người đọc được của marker — `None` nếu `id` không phải marker.
pub fn marker_name(id: u64) -> Option<&'static str> {
    Some(match id {
        MARKER_LOOP => "LOOP",
        MARKER_REC_CALL => "RECURSIVE_CALL",
        MARKER_IF_TRUE => "IF_TRUE",
        MARKER_IF_FALSE => "IF_FALSE",
        MARKER_BRANCH_END => "BRANCH_END",
        MARKER_RETURN => "RETURN",
        MARKER_LOOP_BACK => "LOOP_BACK",
        MARKER_SWITCH_CASE => "SWITCH_CASE",
        MARKER_SWITCH_END => "SWITCH_END",
        MARKER_BREAK => "BREAK",
        MARKER_CONTINUE => "CONTINUE",
        MARKER_THROW => "THROW",
        _ => return None,
    })
}

/// Id của marker theo tên (đảo của `marker_name`) — `None` nếu không khớp.
/// Dùng cho pattern của `search_flow` (VD `"LOOP, save"`).
pub fn marker_id(name: &str) -> Option<u64> {
    Some(match name {
        "LOOP" => MARKER_LOOP,
        "RECURSIVE_CALL" => MARKER_REC_CALL,
        "IF_TRUE" => MARKER_IF_TRUE,
        "IF_FALSE" => MARKER_IF_FALSE,
        "BRANCH_END" => MARKER_BRANCH_END,
        "RETURN" => MARKER_RETURN,
        "LOOP_BACK" => MARKER_LOOP_BACK,
        "SWITCH_CASE" => MARKER_SWITCH_CASE,
        "SWITCH_END" => MARKER_SWITCH_END,
        "BREAK" => MARKER_BREAK,
        "CONTINUE" => MARKER_CONTINUE,
        "THROW" => MARKER_THROW,
        _ => return None,
    })
}

// ==================== Kinds ====================

/// Loại symbol — bộ kinds của semgraph (gọn hơn NodeKind cũ).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "graphql", derive(async_graphql::Enum))]
#[cfg_attr(feature = "graphql", graphql(rename_items = "SCREAMING_SNAKE_CASE"))]
pub enum SymbolKind {
    /// Hàm tự do (không thuộc class).
    Function,
    /// Method của class/object.
    Method,
    Class,
    Interface,
    Enum,
    Variable,
    Constant,
    Parameter,
    Field,
    /// Module/namespace/package.
    Module,
    /// File đứng độc (1 symbol đại diện cho cả file).
    File,
    Config,
}

impl SymbolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Class => "class",
            Self::Interface => "interface",
            Self::Enum => "enum",
            Self::Variable => "variable",
            Self::Constant => "constant",
            Self::Parameter => "parameter",
            Self::Field => "field",
            Self::Module => "module",
            Self::File => "file",
            Self::Config => "config",
        }
    }

    /// Parse từ chuỗi — `None` nếu không khớp kind nào.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "function" => Self::Function,
            "method" => Self::Method,
            "class" => Self::Class,
            "interface" => Self::Interface,
            "enum" => Self::Enum,
            "variable" => Self::Variable,
            "constant" => Self::Constant,
            "parameter" => Self::Parameter,
            "field" => Self::Field,
            "module" => Self::Module,
            "file" => Self::File,
            "config" => Self::Config,
            _ => return None,
        })
    }
}

/// Mức scope của symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "graphql", derive(async_graphql::Enum))]
#[cfg_attr(feature = "graphql", graphql(rename_items = "SCREAMING_SNAKE_CASE"))]
pub enum ScopeLevel {
    /// Global (top-level).
    Global,
    /// Field/method của một object/class.
    ObjectField,
    /// Biến local trong function.
    Local,
    /// Tham số.
    Parameter,
}

impl ScopeLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::ObjectField => "object_field",
            Self::Local => "local",
            Self::Parameter => "parameter",
        }
    }

    /// Parse từ chuỗi (`as_str()` ngược lại) — `None` nếu không khớp.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "global" => Self::Global,
            "object_field" => Self::ObjectField,
            "local" => Self::Local,
            "parameter" => Self::Parameter,
            _ => return None,
        })
    }
}

/// Phân loại tác động bên ngoài của một call (để impact/report).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "graphql", derive(async_graphql::Enum))]
#[cfg_attr(feature = "graphql", graphql(rename_items = "SCREAMING_SNAKE_CASE"))]
pub enum EffectType {
    #[default]
    None,
    SqlQuery,
    SqlWrite,
    CacheRead,
    CacheWrite,
    HttpCall,
    EventEmit,
    FileRead,
    FileWrite,
    Log,
}

impl EffectType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::SqlQuery => "sql_query",
            Self::SqlWrite => "sql_write",
            Self::CacheRead => "cache_read",
            Self::CacheWrite => "cache_write",
            Self::HttpCall => "http_call",
            Self::EventEmit => "event_emit",
            Self::FileRead => "file_read",
            Self::FileWrite => "file_write",
            Self::Log => "log",
        }
    }

    /// Parse snake_case string (case-insensitive) ngược lại thành `EffectType`.
    /// Trùng giá trị `as_str()` của từng variant; chuỗi không biết → `None`.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "none" => Self::None,
            "sql_query" => Self::SqlQuery,
            "sql_write" => Self::SqlWrite,
            "cache_read" => Self::CacheRead,
            "cache_write" => Self::CacheWrite,
            "http_call" => Self::HttpCall,
            "event_emit" => Self::EventEmit,
            "file_read" => Self::FileRead,
            "file_write" => Self::FileWrite,
            "log" => Self::Log,
            _ => return None,
        })
    }
}

/// Match pattern của một effect rule — schema chung cho `config.toml`
/// (`[[effect_rules]]`) dùng bởi cả `codegraph-extract` (classify lúc parse)
/// và `codegraph-sboxes` (Piece 3: state delta theo effect).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EffectCallPattern {
    /// Tên call bắt đầu bằng chuỗi (`call = { prefix = "db." }`).
    Prefix { prefix: String },
    /// Tên call chứa chuỗi ở bất kỳ đâu.
    Contains { contains: String },
    /// Tên call khớp chính xác (case-sensitive).
    Exact { exact: String },
}

/// Một effect rule từ `[[effect_rules]]` trong config.toml.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectRule {
    #[serde(rename = "call")]
    pub call: EffectCallPattern,
    pub effect: EffectType,
}

// ==================== Entities ====================

/// Id global của symbol (u64, bắt đầu từ `SYMBOL_BASE`).
pub type SymbolId = u64;

/// Một symbol (function/class/variable/...) trong graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "graphql", derive(async_graphql::SimpleObject))]
pub struct Symbol {
    pub id: SymbolId,
    pub name: String,
    pub kind: SymbolKind,
    pub scope: ScopeLevel,
    /// Id của scope chứa (class của method, func của param/local); `0` = global.
    pub scope_id: SymbolId,
    /// Id của kiểu khai báo (nếu xác định được); `0` = không có.
    pub type_ref: SymbolId,
    /// Chuỗi kiểu thô (VD `"orderservice.OrderService"`).
    pub type_name: Option<String>,
    pub file: String,
    pub line: u32,
    pub end_line: u32,
    /// Signature (dòng khai báo đầu tiên).
    pub signature: Option<String>,
    pub doc: Option<String>,
    #[serde(default)]
    pub annotations: Vec<Annotation>,
    pub language: String,
}

/// Annotation (VD `@Override`, `@Cacheable`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "graphql", derive(async_graphql::SimpleObject))]
pub struct Annotation {
    pub name: String,
    #[serde(default)]
    pub args: HashMap<String, String>,
    pub line: u32,
}

/// Metadata của 1 call edge — serialized thành edge data (edge stream).
///
/// `(caller_id, callee_id)` là chiều chuẩn; `position` = index của callee trong
/// chain của caller (để nối với CallRecord khi render flow).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeMeta {
    pub caller_id: SymbolId,
    pub callee_id: SymbolId,
    /// Index trong chain của caller mà callee xuất hiện.
    pub position: usize,
    /// Guard text của if bao quanh (nếu có).
    pub condition: Option<String>,
    pub effect: EffectType,
    pub effect_desc: Option<String>,
    #[serde(default)]
    pub arg_ids: Vec<SymbolId>,
    pub is_loop_body: bool,
    pub is_recursive: bool,
}

/// Call record thô — persist để render flow khi call không resolve được.
///
/// Vị trí `0` trong chain là placeholder, được thay bằng id thật khi resolve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallRecord {
    pub caller_id: SymbolId,
    /// Tên call đầy đủ (VD `fmt.Println`, `requests.get`).
    pub call_name: String,
    /// Index trong chain của caller.
    pub position: usize,
    #[serde(default)]
    pub arg_exprs: Vec<String>,
    pub line: u32,
    pub condition: Option<String>,
    pub is_loop_body: bool,
    pub effect: EffectType,
    pub effect_desc: Option<String>,
    /// Gợi ý structural khi resolve (VD Java class literal).
    pub target_class: Option<String>,
    pub target_method: Option<String>,
}

/// Giá trị của inverted index `call name → call sites` (dùng cho query
/// "callers của library call" — không cần resolve được mới hiện).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "graphql", derive(async_graphql::SimpleObject))]
pub struct CallSite {
    pub caller_id: SymbolId,
    pub call_name: String,
    pub line: u32,
    pub condition: Option<String>,
    pub is_loop_body: bool,
    #[serde(default)]
    pub arg_exprs: Vec<String>,
}

/// Thông tin file trong graph (không lưu content — dùng cho files/status).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "graphql", derive(async_graphql::SimpleObject))]
pub struct FileInfo {
    pub path: String,
    pub language: String,
    pub bytes: u64,
    pub lines: u32,
}

// ==================== Query results ====================

/// Flow của một hàm — chain render ra (marker name / symbol name / call thô).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "graphql", derive(async_graphql::SimpleObject))]
pub struct FlowResult {
    /// Symbol chủ (hàm có flow này).
    pub symbol: Symbol,
    /// Chain raw (u64 ids).
    pub chain: Vec<u64>,
    /// Mô tả từng element trong chain, index-aligned với `chain`.
    pub chain_desc: Vec<String>,
    /// Danh sách call-site (kể cả unresolved — hiện tên thô).
    pub calls: Vec<FlowCall>,
}

/// Một call-site trong flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "graphql", derive(async_graphql::SimpleObject))]
pub struct FlowCall {
    pub position: usize,
    /// Tên call (tên symbol nếu resolve được, không thì tên thô).
    pub to_name: String,
    /// Id callee — `None` nếu chưa resolve (placeholder 0).
    pub to_id: Option<SymbolId>,
    pub line: u32,
    pub condition: Option<String>,
    pub effect: EffectType,
    pub effect_desc: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
}

/// Kết quả resolve symbol theo id/name — `ambiguous=true` khi name trùng nhiều
/// symbol (MCP layer bảo LLM retry với `symbol_id`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "graphql", derive(async_graphql::SimpleObject))]
pub struct ResolveResult {
    /// Symbol khớp duy nhất (nếu không ambiguous và tìm thấy).
    pub symbol: Option<Symbol>,
    /// Toàn bộ ứng viên trùng name.
    pub matches: Vec<Symbol>,
    pub ambiguous: bool,
}

/// Số liệu tổng hợp (`/api/status`, `codegraph status`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "graphql", derive(async_graphql::SimpleObject))]
pub struct DbStats {
    pub symbols: u64,
    pub chains: u64,
    pub edges: u64,
    pub files: u64,
    pub next_id: u64,
}

/// Kết quả `search_flow` — hàm có chain chứa pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "graphql", derive(async_graphql::SimpleObject))]
pub struct SearchFlowResult {
    pub function_id: SymbolId,
    pub function_name: String,
    /// Chain đầy đủ của hàm (đã resolve).
    pub chain: Vec<u64>,
    /// Số lần pattern khớp trong chain (engine trả record dedup — luôn 1).
    pub match_count: u32,
}

/// Kết quả `callers_by_call_name` — gom call site theo caller function.
///
/// Trả về mọi function gọi một library call có tên chứa `query` (kể cả call
/// không resolve được thành symbol — đây là cửa sổ ra "thế giới ngoài repo").
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "graphql", derive(async_graphql::SimpleObject))]
pub struct CallSiteResult {
    pub func_id: SymbolId,
    pub func_name: String,
    pub file: String,
    #[serde(default)]
    pub call_sites: Vec<CallSite>,
}

// ==================== Search / class queries ====================

/// Match mode khi search symbol theo tên (nâng cấp của `search_symbol`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "graphql", derive(async_graphql::Enum))]
#[cfg_attr(feature = "graphql", graphql(rename_items = "SCREAMING_SNAKE_CASE"))]
pub enum SymbolMatch {
    /// Substring bất kỳ (mặc định).
    Contains,
    /// Tên bắt đầu bằng query.
    Prefix,
    /// Tên kết thúc bằng query.
    Suffix,
    /// Tên trùng chính xác (case-insensitive).
    Exact,
    /// Semantic (vector): query → embedding → KNN over symbol embeddings —
    /// tìm symbol **tên tương tự / cùng ý nghĩa** kể cả khi không khớp substring.
    Semantic,
    /// Hybrid: chạy cả `Contains` (lexical) lẫn `Semantic` (vector), gộp kết
    /// quả bằng Reciprocal Rank Fusion (RRF).
    Hybrid,
}

impl SymbolMatch {
    /// Parse từ chuỗi (khớp tên Go tool `semgraph_search_symbol`).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "contains" => Self::Contains,
            "prefix" => Self::Prefix,
            "suffix" => Self::Suffix,
            "exact" => Self::Exact,
            "semantic" => Self::Semantic,
            "hybrid" => Self::Hybrid,
            _ => return None,
        })
    }
}

/// Projection gọn của một member (method/field) trong class — bỏ doc/signature
/// dài để giảm payload cho LLM (tương ứng `compact` của `semgraph_get_class_methods`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "graphql", derive(async_graphql::SimpleObject))]
pub struct MemberInfo {
    pub id: SymbolId,
    pub name: String,
    pub kind: SymbolKind,
    pub line: u32,

    /// Dòng khai báo đầu tiên (VD `getOrders(userId int) (Order, error)`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

impl MemberInfo {
    pub fn from_symbol(s: &Symbol) -> Self {
        Self {
            id: s.id,
            name: s.name.clone(),
            kind: s.kind,
            line: s.line,
            signature: s.signature.clone(),
        }
    }
}

/// Thông tin class: symbol class + fields và methods tách riêng (tương ứng
/// `semgraph_get_class`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "graphql", derive(async_graphql::SimpleObject))]
pub struct ClassInfo {
    pub class: Symbol,
    pub fields: Vec<MemberInfo>,
    pub methods: Vec<MemberInfo>,
}

/// Scope của function: parameters + local variables (tương ứng
/// `semgraph_get_function_scope`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "graphql", derive(async_graphql::SimpleObject))]
pub struct FunctionScope {
    pub function: Symbol,
    pub parameters: Vec<Symbol>,
    pub locals: Vec<Symbol>,
}

/// Một dependency (module/package prefix rút từ call names).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "graphql", derive(async_graphql::SimpleObject))]
pub struct Dependency {
    pub name: String,
    /// Số call sites tham chiếu tới module này.
    pub count: usize,
}

/// Báo cáo dependencies của repo (tương ứng `semgraph_get_dependencies`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "graphql", derive(async_graphql::SimpleObject))]
pub struct DependenciesReport {
    pub internal: Vec<Dependency>,
    pub external: Vec<Dependency>,
    pub total: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_reserved_below_symbol_base() {
        // Mọi marker định nghĩa phải nằm dưới SYMBOL_BASE và có tên đọc được;
        // vùng 13..=99 là reserved (chưa có marker định nghĩa).
        for id in [
            MARKER_LOOP,
            MARKER_REC_CALL,
            MARKER_IF_TRUE,
            MARKER_IF_FALSE,
            MARKER_BRANCH_END,
            MARKER_RETURN,
            MARKER_LOOP_BACK,
            MARKER_SWITCH_CASE,
            MARKER_SWITCH_END,
            MARKER_BREAK,
            MARKER_CONTINUE,
            MARKER_THROW,
        ] {
            assert!(is_marker(id), "id {id} phải là marker");
            assert!(marker_name(id).is_some(), "marker {id} phải có tên");
        }
        assert!(!is_marker(0));
        assert!(!is_marker(SYMBOL_BASE));
        assert_eq!(marker_name(MARKER_LOOP), Some("LOOP"));
        assert_eq!(marker_name(99), None);
        assert_eq!(marker_name(13), None, "13 chưa có marker định nghĩa");
        // marker_id là đảo của marker_name.
        for id in [
            MARKER_LOOP,
            MARKER_REC_CALL,
            MARKER_IF_TRUE,
            MARKER_IF_FALSE,
            MARKER_BRANCH_END,
            MARKER_RETURN,
            MARKER_LOOP_BACK,
            MARKER_SWITCH_CASE,
            MARKER_SWITCH_END,
            MARKER_BREAK,
            MARKER_CONTINUE,
            MARKER_THROW,
        ] {
            assert_eq!(marker_id(marker_name(id).unwrap()), Some(id));
        }
        assert_eq!(marker_id("LOOP"), Some(MARKER_LOOP));
        assert_eq!(marker_id("bogus"), None);
    }

    #[test]
    fn symbol_kind_roundtrip() {
        for kind in [
            SymbolKind::Function,
            SymbolKind::Method,
            SymbolKind::Class,
            SymbolKind::Interface,
            SymbolKind::Enum,
            SymbolKind::Variable,
            SymbolKind::Constant,
            SymbolKind::Parameter,
            SymbolKind::Field,
            SymbolKind::Module,
            SymbolKind::File,
            SymbolKind::Config,
        ] {
            assert_eq!(SymbolKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(SymbolKind::parse("bogus"), None);
    }

    #[test]
    fn effect_type_default_is_none() {
        assert_eq!(EffectType::default(), EffectType::None);
    }

    #[test]
    fn effect_type_parse_round_trips_as_str() {
        for e in [
            EffectType::None,
            EffectType::SqlQuery,
            EffectType::SqlWrite,
            EffectType::CacheRead,
            EffectType::CacheWrite,
            EffectType::HttpCall,
            EffectType::EventEmit,
            EffectType::FileRead,
            EffectType::FileWrite,
            EffectType::Log,
        ] {
            assert_eq!(EffectType::parse(e.as_str()), Some(e));
        }
        // Case-insensitive + trim.
        assert_eq!(
            EffectType::parse("  SQL_QUERY "),
            Some(EffectType::SqlQuery)
        );
        assert_eq!(EffectType::parse("sql_query"), Some(EffectType::SqlQuery));
        assert_eq!(EffectType::parse("bogus"), None);
    }
}
