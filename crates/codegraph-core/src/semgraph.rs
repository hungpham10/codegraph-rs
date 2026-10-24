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

// ==================== Marker Enum ====================

/// Marker luồng điều khiển trong graph (thay thế MARKER_* constants cũ).
/// Same semantics as MARKER_* but type-safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "graphql", derive(async_graphql::Enum))]
#[cfg_attr(feature = "graphql", graphql(rename_items = "SCREAMING_SNAKE_CASE"))]
#[allow(dead_code)]
pub enum Marker {
    Loop = 1,
    RecCall = 2,
    IfTrue = 3,
    IfFalse = 4,
    BranchEnd = 5,
    Return = 6,
    LoopBack = 7,
    SwitchCase = 8,
    SwitchEnd = 9,
    Break = 10,
    Continue = 11,
    Throw = 12,
    CallEnter = 17,
    CallExit = 18,
    SwitchClose = 19,
    SwitchStart = 20,
    StmtEnd = 21,
    TryStart = 22,
    TryEnd = 23,
    CatchStart = 24,
}

impl Marker {
    /// All variant values (các hằng MARKER_*).
    pub const ALL: [Marker; 20] = [
        Marker::Loop,
        Marker::RecCall,
        Marker::IfTrue,
        Marker::IfFalse,
        Marker::BranchEnd,
        Marker::Return,
        Marker::LoopBack,
        Marker::SwitchCase,
        Marker::SwitchEnd,
        Marker::Break,
        Marker::Continue,
        Marker::Throw,
        Marker::CallEnter,
        Marker::CallExit,
        Marker::SwitchClose,
        Marker::SwitchStart,
        Marker::StmtEnd,
        Marker::TryStart,
        Marker::TryEnd,
        Marker::CatchStart,
    ];

    /// Giá trị u64 tương ứng (cho tương thích với chain cũ).
    #[inline]
    pub fn id(self) -> u64 {
        self as u64
    }

    /// Chuyển từ u64 raw sang Marker — `None` nếu id không phải marker.
    #[inline]
    pub fn from_id(id: u64) -> Option<Self> {
        match id {
            1 => Some(Marker::Loop),
            2 => Some(Marker::RecCall),
            3 => Some(Marker::IfTrue),
            4 => Some(Marker::IfFalse),
            5 => Some(Marker::BranchEnd),
            6 => Some(Marker::Return),
            7 => Some(Marker::LoopBack),
            8 => Some(Marker::SwitchCase),
            9 => Some(Marker::SwitchEnd),
            10 => Some(Marker::Break),
            11 => Some(Marker::Continue),
            12 => Some(Marker::Throw),
            17 => Some(Marker::CallEnter),
            18 => Some(Marker::CallExit),
            19 => Some(Marker::SwitchClose),
            20 => Some(Marker::SwitchStart),
            21 => Some(Marker::StmtEnd),
            22 => Some(Marker::TryStart),
            23 => Some(Marker::TryEnd),
            24 => Some(Marker::CatchStart),
            _ => None,
        }
    }

    /// Tên đọc được cho UI / logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Marker::Loop => "LOOP",
            Marker::RecCall => "RECURSIVE_CALL",
            Marker::IfTrue => "IF_TRUE",
            Marker::IfFalse => "IF_FALSE",
            Marker::BranchEnd => "BRANCH_END",
            Marker::Return => "RETURN",
            Marker::LoopBack => "LOOP_BACK",
            Marker::SwitchCase => "SWITCH_CASE",
            Marker::SwitchEnd => "SWITCH_END",
            Marker::Break => "BREAK",
            Marker::Continue => "CONTINUE",
            Marker::Throw => "THROW",
            Marker::CallEnter => "CALL_ENTER",
            Marker::CallExit => "CALL_EXIT",
            Marker::SwitchClose => "SWITCH_CLOSE",
            Marker::SwitchStart => "SWITCH_START",
            Marker::StmtEnd => "STMT_END",
            Marker::TryStart => "TRY_START",
            Marker::TryEnd => "TRY_END",
            Marker::CatchStart => "CATCH_START",
        }
    }

    /// Parse từ tên (case‑insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        let lower = s.to_lowercase();
        match lower.as_str() {
            "loop" => Some(Marker::Loop),
            "recursice_call" | "recursive_call" => Some(Marker::RecCall),
            "if_true" => Some(Marker::IfTrue),
            "if_false" => Some(Marker::IfFalse),
            "branch_end" => Some(Marker::BranchEnd),
            "return" => Some(Marker::Return),
            "loop_back" => Some(Marker::LoopBack),
            "switch_case" => Some(Marker::SwitchCase),
            "switch_end" => Some(Marker::SwitchEnd),
            "break" => Some(Marker::Break),
            "continue" => Some(Marker::Continue),
            "throw" => Some(Marker::Throw),
            "call_enter" => Some(Marker::CallEnter),
            "call_exit" => Some(Marker::CallExit),
            "switch_close" => Some(Marker::SwitchClose),
            "switch_start" => Some(Marker::SwitchStart),
            "stmt_end" => Some(Marker::StmtEnd),
            "try_start" => Some(Marker::TryStart),
            "try_end" => Some(Marker::TryEnd),
            "catch_start" => Some(Marker::CatchStart),
            _ => None,
        }
    }
}

// ==================== Id-space ====================

/// Id bắt đầu cho symbol. Mọi id `< SYMBOL_BASE` là marker reserved.
pub const SYMBOL_BASE: u64 = 100;

/// Helper: chuyển từ raw id sang Marker (dùng trong code graph).
#[inline]
pub fn is_marker(id: u64) -> bool {
    id > 0 && id < SYMBOL_BASE
}

/// Backward compatibility: MARKER_* constants (hiện đã được thay thế bằng enum Marker).
pub const MARKER_LOOP: u64 = 1;
pub const MARKER_REC_CALL: u64 = 2;
pub const MARKER_IF_TRUE: u64 = 3;
pub const MARKER_IF_FALSE: u64 = 4;
pub const MARKER_BRANCH_END: u64 = 5;
pub const MARKER_RETURN: u64 = 6;
pub const MARKER_LOOP_BACK: u64 = 7;
pub const MARKER_SWITCH_CASE: u64 = 8;
pub const MARKER_SWITCH_END: u64 = 9;
pub const MARKER_BREAK: u64 = 10;
pub const MARKER_CONTINUE: u64 = 11;
pub const MARKER_THROW: u64 = 12;
pub const MARKER_CALL_ENTER: u64 = 17;
pub const MARKER_CALL_EXIT: u64 = 18;
pub const MARKER_SWITCH_CLOSE: u64 = 19;
pub const MARKER_SWITCH_START: u64 = 20;
pub const MARKER_STMT_END: u64 = 21;
pub const MARKER_TRY_START: u64 = 22;
pub const MARKER_TRY_END: u64 = 23;
pub const MARKER_CATCH_START: u64 = 24;

/// Helper để đọc chain sang danh sách marker — tiện cho code đọc.
#[allow(dead_code)]
#[inline]
pub fn parse_markers(chain: &[u64]) -> Vec<Marker> {
    chain.iter().filter_map(|&id| Marker::from_id(id)).collect()
}

/// Helper để đọc chain sang danh sách symbol calls (skip marker, 0, self).
#[allow(dead_code)]
#[inline]
pub fn extract_calls(chain: &[u64], func_id: u64) -> Vec<u64> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for &e in chain {
        if is_marker(e) || e == 0 || e == func_id || !seen.insert(e) {
            continue;
        }
        out.push(e);
    }
    out
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
    /// Biến gán kết quả call này (VD: `let cli = Cli::parse()` → `into_var = Some("cli")`).
    #[serde(default)]
    pub into_var: Option<String>,
    /// `true` nếu assignment tạo biến mới, `false` nếu update biến đã có.
    #[serde(default)]
    pub is_new_var: bool,
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
    /// Nhãn trigger của marker nhánh (if/loop/switch) theo vị trí trong `chain`
    /// — cho UI render `IF_TRUE: x > 0`, `LOOP: i < n`, `SWITCH_CASE: Cmd::Init`.
    #[serde(default)]
    pub branch_labels: Vec<BranchLabel>,
}

/// Nhãn trigger tại một vị trí marker trong chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "graphql", derive(async_graphql::SimpleObject))]
pub struct BranchLabel {
    /// Index trong `FlowResult.chain` (vị trí marker).
    pub position: usize,
    /// Text trigger: điều kiện `if`/`while`, giá trị `case`/`match arm`.
    pub label: String,
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
    /// Biến gán kết quả call này (VD: `let cli = Cli::parse()` → `into_var = Some("cli")`).
    #[serde(default)]
    pub into_var: Option<String>,
    /// `true` nếu assignment tạo biến mới, `false` nếu update biến đã có.
    #[serde(default)]
    pub is_new_var: bool,
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

// ==================== Full-flow trace (debug-style call/return path) ====================

/// Hành động tại một bước trace trong flow path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "graphql", derive(async_graphql::Enum))]
#[cfg_attr(feature = "graphql", graphql(rename_items = "SCREAMING_SNAKE_CASE"))]
#[allow(dead_code)]
pub enum TraceAction {
    /// Gọi xuống hàm khác (push vào call stack).
    Call,
    /// Return về hàm gọi trước (pop khỏi call stack).
    Return,
    /// Ném exception / bail out (không return bình thường).
    Throw,
    /// Assert fail (xử lý tương tự throw).
    Assert,
    /// Nhánh if/else/switch (điểm phân nhánh).
    Branch,
    /// Vào vòng lặp.
    Loop,
    /// Câu lệnh thông thường.
    Statement,
}

/// Kết thúc của một full-flow path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "graphql", derive(async_graphql::Enum))]
#[cfg_attr(feature = "graphql", graphql(rename_items = "SCREAMING_SNAKE_CASE"))]
#[allow(dead_code)]
pub enum PathEnding {
    /// Return bình thường về caller.
    Returned,
    /// Throw exception / bail out.
    Threw,
    /// Assert fail.
    Asserted,
    /// Đạt giới hạn depth (cắt ngắn).
    DepthLimit,
    /// Đạt giới hạn số paths (cắt ngắn).
    PathLimit,
}

// Converter from Marker to TraceAction
impl From<Marker> for TraceAction {
    fn from(marker: Marker) -> Self {
        match marker {
            Marker::CallEnter | Marker::RecCall => Self::Call,
            Marker::CallExit | Marker::Return => Self::Return,
            Marker::Throw => Self::Throw,
            Marker::IfTrue
            | Marker::IfFalse
            | Marker::SwitchCase
            | Marker::SwitchEnd
            | Marker::SwitchClose
            | Marker::SwitchStart
            | Marker::BranchEnd => Self::Branch,
            Marker::Loop | Marker::LoopBack => Self::Loop,
            Marker::Break | Marker::Continue | Marker::StmtEnd => Self::Statement,
            Marker::TryStart | Marker::TryEnd | Marker::CatchStart => Self::Statement,
        }
    }
}

/// Tên người đọc được của marker — `None` nếu `id` không phải marker (backward compat).
#[inline]
pub fn marker_name(id: u64) -> Option<&'static str> {
    Marker::from_id(id).map(|m| m.as_str())
}

/// Id của marker theo tên (đảo của `marker_name`) — `None` nếu không khớp (backward compat).
#[inline]
pub fn marker_id(name: &str) -> Option<u64> {
    Marker::parse(name).map(|m| m.id())
}

/// Một bước trong full-flow path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "graphql", derive(async_graphql::SimpleObject))]
#[allow(dead_code)]
pub struct FlowStep {
    /// Id của symbol tại bước này.
    pub symbol_id: u64,
    /// Tên symbol.
    pub symbol_name: String,
    /// File chứa symbol.
    pub file: String,
    /// Số dòng (nếu có).
    pub line: u32,
    /// Hành động tại bước này.
    pub action: TraceAction,
    /// Độ深度 của call stack tại thời điểm này.
    pub stack_depth: usize,
    /// Điều kiện nhánh / điều kiện throw (nếu có).
    #[serde(default)]
    pub condition: Option<String>,
}

/// Một full-flow path hoàn chỉnh từ hàm start đến hàm target.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "graphql", derive(async_graphql::SimpleObject))]
#[allow(dead_code)]
pub struct FullFlowPath {
    /// Các bước đi trong path.
    pub steps: Vec<FlowStep>,
    /// Path kết thúc như thế nào.
    pub ended_with: PathEnding,
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
