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
