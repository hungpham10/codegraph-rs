//! GraphQL API-level types (không mirror domain — domain types ở
//! `codegraph_core::semgraph` đã derive `async_graphql::SimpleObject`/`Enum`
//! gated behind feature `graphql`, nên GraphQL layer tái dùng trực tiếp).
//!
//! Ở đây chỉ định nghĩa:
//! - Các **wrapper** phân trang (shape response riêng của API, không có ở core).
//! - `ContextFormat` + `ContextRequestInput` (GraphQL-specific input cho
//!   `context`, map sang `codegraph_context::ContextRequest`).

use async_graphql::{Enum, InputObject, SimpleObject};
use codegraph_context::Format as CoreCtxFormat;
use codegraph_core::{CallSiteResult, SearchFlowResult, Symbol, SymbolKind, SymbolMatch};
use codegraph_docs::{Kind as DocKind, NodePayload, Scalar};

// ==================== Pagination wrappers ====================

#[derive(SimpleObject, Clone, Debug)]
pub struct SearchSymbolResult {
    pub symbols: Vec<Symbol>,
    pub total: u64,
    pub timed_out: bool,
    pub resume: Option<String>,
    pub index_version: u64,
}

#[derive(SimpleObject, Clone, Debug)]
pub struct ListResult {
    pub items: Vec<Symbol>,
    pub total: u64,
    pub has_more: bool,
}

#[derive(SimpleObject, Clone, Debug)]
pub struct AnnotationSearchResult {
    pub symbols: Vec<Symbol>,
    pub total: u64,
    pub has_more: bool,
}

#[derive(SimpleObject, Clone, Debug)]
pub struct ReferencesResult {
    pub results: Vec<CallSiteResult>,
    pub total: u64,
    pub has_more: bool,
}

#[derive(SimpleObject, Clone, Debug)]
pub struct FlowSearchResult {
    pub results: Vec<SearchFlowResult>,
    pub total: u64,
    pub has_more: bool,
}

// ==================== Context input ====================

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
#[graphql(rename_items = "SCREAMING_SNAKE_CASE")]
pub enum ContextFormat {
    Markdown,
    Json,
}

impl From<ContextFormat> for CoreCtxFormat {
    fn from(f: ContextFormat) -> Self {
        match f {
            ContextFormat::Markdown => CoreCtxFormat::Markdown,
            ContextFormat::Json => CoreCtxFormat::Json,
        }
    }
}

#[derive(InputObject)]
pub struct ContextRequestInput {
    pub query: String,
    pub depth: Option<i32>,
    pub include_source: Option<bool>,
    pub limit: Option<i32>,
    pub format: Option<ContextFormat>,
    pub strip_prefix: Option<String>,
}

impl From<ContextRequestInput> for codegraph_context::ContextRequest {
    fn from(i: ContextRequestInput) -> Self {
        codegraph_context::ContextRequest {
            query: i.query,
            depth: i.depth.unwrap_or(1).max(1) as u32,
            include_source: i.include_source.unwrap_or(false),
            limit: i.limit.unwrap_or(5).max(1) as u32,
            format: i
                .format
                .map(|f| f.into())
                .unwrap_or(CoreCtxFormat::Markdown),
            strip_prefix: i.strip_prefix,
        }
    }
}

// ==================== Search input ====================

/// Input cho `searchSymbol` — gom nhóm tham số tìm kiếm để tránh quá nhiều
/// argument (clippy::too_many_arguments) và dễ mở rộng về sau.
#[derive(InputObject)]
pub struct SearchSymbolInput {
    pub query: String,
    pub kind: Option<SymbolKind>,
    pub mode: Option<SymbolMatch>,
    pub limit: Option<i32>,
    pub offset: Option<i32>,
    pub resume: Option<String>,
    pub timeout_ms: Option<i64>,
}

// ==================== Type kind ====================

/// Kind cho resolver `types(kind, ...)` — gộp `list_classes` / `list_interfaces`
/// / `list_enums` thành 1 resolver duy nhất.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
#[graphql(rename_items = "SCREAMING_SNAKE_CASE")]
pub enum TypeKind {
    Class,
    Interface,
    Enum,
}

// ==================== Mermaid kind ====================

/// Loại diagram Mermaid cho resolver `mermaid(id, kind, depth)` — render biến
/// thể hình ảnh của các query diagram (`flow` / `callers` / `callees` / `impact`).
///
/// - `FLOW`: logic của hàm — control-flow (nhánh if/else, vòng lặp, switch,
///   return) với mỗi call ghi rõ callee, số dòng, guard condition và effect.
/// - `CALLERS` / `CALLEES`: ai gọi hàm / hàm gọi ai (BFS `depth` hop).
/// - `IMPACT`: callers transitive (bán kính ảnh hưởng khi sửa hàm).
///
/// Chỉ hoạt động khi server bật `--mermaid`.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
#[graphql(rename_items = "SCREAMING_SNAKE_CASE")]
pub enum MermaidKind {
    Flow,
    Callers,
    Callees,
    Impact,
}

// ==================== Document types ====================

/// Định dạng tài liệu hỗ trợ (input cho ingest).
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
#[graphql(rename_items = "SCREAMING_SNAKE_CASE")]
pub enum DocFormat {
    Hcl,
    Json,
    Toml,
    Yaml,
}

/// Nhãn hiển thị của một `Kind` trong document graph.
fn kind_label(k: DocKind) -> String {
    format!("{k:?}").to_uppercase()
}

/// Chuỗi hiển thị của một scalar.
fn scalar_label(s: &Scalar) -> String {
    match s {
        Scalar::String(v) => v.clone(),
        Scalar::Number(n) => n.to_string(),
        Scalar::Bool(b) => b.to_string(),
        Scalar::Null => "null".to_string(),
    }
}

/// Node document đã hydrate — cây con.
#[derive(SimpleObject, Clone, Debug)]
pub struct DocNodePayload {
    pub id: u64,
    pub path: Vec<String>,
    pub kind: String,
    pub value: Option<String>,
    pub key: Option<String>,
    pub index: Option<u32>,
    pub doc: u64,
    pub children: Vec<DocNodePayload>,
}

impl From<NodePayload> for DocNodePayload {
    fn from(p: NodePayload) -> Self {
        Self {
            id: p.id,
            path: p.path,
            kind: kind_label(p.kind),
            value: p.value.as_ref().map(scalar_label),
            key: p.key,
            index: p.index,
            doc: p.doc,
            children: p.children.into_iter().map(Into::into).collect(),
        }
    }
}

/// Node document dạng phẳng (kết quả search) — không kèm children.
#[derive(SimpleObject, Clone, Debug)]
pub struct DocNodeView {
    pub id: u64,
    pub path: Vec<String>,
    pub kind: String,
    pub value: Option<String>,
    pub key: Option<String>,
    pub index: Option<u32>,
    pub doc: u64,
}

impl From<codegraph_docs::Node> for DocNodeView {
    fn from(n: codegraph_docs::Node) -> Self {
        // path không có sẵn trên Node thô — dựng từ key/index chain (rỗng ở đây,
        // caller có path từ hydrate khi cần).
        Self {
            id: n.id,
            path: Vec::new(),
            kind: kind_label(n.kind),
            value: n.value.as_ref().map(scalar_label),
            key: n.key,
            index: n.index,
            doc: n.doc,
        }
    }
}

/// Summary của một document.
#[derive(SimpleObject, Clone, Debug)]
pub struct DocInfoView {
    pub doc_id: u64,
    pub path: String,
    pub format: String,
    pub root_node_id: u64,
    pub nodes: usize,
}

impl From<codegraph_docs::graph::DocInfo> for DocInfoView {
    fn from(d: codegraph_docs::graph::DocInfo) -> Self {
        Self {
            doc_id: d.doc_id,
            path: d.path,
            format: d.format,
            root_node_id: d.root_node_id,
            nodes: d.nodes,
        }
    }
}

/// Summary của document graph.
#[derive(SimpleObject, Clone, Debug)]
pub struct DocStatsView {
    pub docs: usize,
    pub nodes: usize,
}

/// Một structural pattern đã mine.
#[derive(SimpleObject, Clone, Debug)]
pub struct DocPatternView {
    pub pattern_id: u64,
    pub tokens: Vec<String>,
    pub node_count: usize,
    pub doc_count: usize,
    pub doc_freq: f64,
}

impl From<codegraph_docs::graph::PatternEntry> for DocPatternView {
    fn from(p: codegraph_docs::graph::PatternEntry) -> Self {
        Self {
            pattern_id: p.pattern_id,
            tokens: p.tokens,
            node_count: p.node_count,
            doc_count: p.doc_count,
            doc_freq: p.doc_freq,
        }
    }
}

/// Kết quả ingest hàng loạt document.
#[derive(SimpleObject, Clone, Debug)]
pub struct DocIngestSummary {
    pub requested: usize,
    pub ingested: usize,
    pub failed: usize,
}
