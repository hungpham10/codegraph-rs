//! Core types shared across codegraph crates: NodeKind, EdgeKind, Node, Edge, errors.
//!
//! Model cũ (`Node`/`Edge`/`NodeKind`/`EdgeKind`) đang dần bị thay bằng model
//! semgraph (`semgraph` module) — wire breaking đã chốt ở plan.

pub mod drafts;
pub mod error;
pub mod kinds;
pub mod model;
pub mod semgraph;

pub use drafts::{DbStats, EdgeDraft, FileRow, NodeDraft};
pub use error::{Error, Result};
pub use kinds::{EdgeKind, InvalidKind, NodeKind};
pub use model::{Edge, Node, NodeId};
pub use semgraph::{
    is_marker, marker_id, marker_name, Annotation, CallRecord, CallSite, CallSiteResult,
    ClassInfo, DbStats as SemgraphStats, Dependency, DependenciesReport, EdgeMeta, EffectType,
    FileInfo, FlowCall, FlowResult, FunctionScope, MemberInfo, ResolveResult, ScopeLevel,
    SearchFlowResult, Symbol, SymbolId, SymbolKind, SymbolMatch, MARKER_BRANCH_END, MARKER_BREAK,
    MARKER_CONTINUE, MARKER_IF_FALSE, MARKER_IF_TRUE, MARKER_LOOP, MARKER_LOOP_BACK, MARKER_REC_CALL,
    MARKER_RETURN, MARKER_SWITCH_CASE, MARKER_SWITCH_END, MARKER_THROW, SYMBOL_BASE,
};
