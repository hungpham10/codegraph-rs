//! Core types shared across codegraph crates: NodeKind, EdgeKind, Node, Edge, errors.
//!
//! Model cũ (`Node`/`Edge`/`NodeKind`/`EdgeKind`) đang dần bị thay bằng model
//! semgraph (`semgraph` module) — wire breaking đã chốt ở plan.

mod error;
pub mod prof;
mod route;
mod semgraph;

pub use error::{Error, Result};
pub use route::StorageRoute;
pub use semgraph::{
    is_marker, marker_id, marker_name, Annotation, BranchLabel, CallRecord, CallSite,
    CallSiteResult, ClassInfo, DbStats as SemgraphStats, DependenciesReport, Dependency,
    EffectCallPattern, EffectRule, EffectType, FileInfo, FlowCall, FlowResult, FlowStep,
    FullFlowPath, FunctionScope, Marker, MemberInfo, PathEnding, ResolutionKind, ResolveResult,
    ScopeLevel, SearchFlowResult, Symbol, SymbolId, SymbolKind, SymbolMatch, TraceAction,
    MARKER_BRANCH_END, MARKER_BREAK, MARKER_CALL_ENTER, MARKER_CALL_EXIT, MARKER_CATCH_START,
    MARKER_CONTINUE, MARKER_DEFER, MARKER_GO, MARKER_IF_FALSE, MARKER_IF_TRUE, MARKER_LOOP,
    MARKER_LOOP_BACK, MARKER_REC_CALL, MARKER_RETURN, MARKER_STMT_END, MARKER_SWITCH_CASE,
    MARKER_SWITCH_CLOSE, MARKER_SWITCH_END, MARKER_SWITCH_START, MARKER_THROW, MARKER_TRY_END,
    MARKER_TRY_START, SYMBOL_BASE,
};
