//! Core types shared across codegraph crates: NodeKind, EdgeKind, Node, Edge, errors.
//!
//! Model cũ (`Node`/`Edge`/`NodeKind`/`EdgeKind`) đang dần bị thay bằng model
//! semgraph (`semgraph` module) — wire breaking đã chốt ở plan.

mod error;
mod semgraph;

pub use error::{Error, Result};
pub use semgraph::{
    is_marker, marker_id, marker_name, Annotation, CallRecord, CallSite, CallSiteResult, ClassInfo,
    DbStats as SemgraphStats, DependenciesReport, Dependency, EdgeMeta, EffectType, FileInfo,
    FlowCall, FlowResult, FunctionScope, MemberInfo, ResolveResult, ScopeLevel, SearchFlowResult,
    Symbol, SymbolId, SymbolKind, SymbolMatch, MARKER_BRANCH_END, MARKER_BREAK, MARKER_CONTINUE,
    MARKER_IF_FALSE, MARKER_IF_TRUE, MARKER_LOOP, MARKER_LOOP_BACK, MARKER_REC_CALL, MARKER_RETURN,
    MARKER_SWITCH_CASE, MARKER_SWITCH_END, MARKER_THROW, SYMBOL_BASE,
};
