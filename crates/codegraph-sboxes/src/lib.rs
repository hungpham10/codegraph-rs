//! codegraph-sboxes — Behavior Verification Sandbox (Piece 1).
//!
//! Compile a *group of functions* from the semantic graph (`GraphIndex::flow`)
//! into real machine code via **Cranelift JIT**, with the callees they call
//! bound to **Rhai mocks**. Each compiled function is `extern "C" fn`:
//!
//! ```text
//! fn(ctx: *mut Ctx, nargs: i64, args: *mut i64, ret: *mut i64) -> i64
//! ```
//!
//! Two imported trampolines provided by the runtime:
//! - `mock_dispatch(ctx, callee_idx, nargs, args, ret) -> i64` — run a Rhai mock.
//! - `eval_condition(ctx, cond_idx, rec_depth) -> i64` — resolve IF/LOOP/SWITCH
//!   conditions from a deterministic `BranchPolicy` (termination via `loop_cap`).
//!
//! See `codegen` for the chain-marker → structured-CFG lowering and `runtime`
//! for the JIT module wiring.

pub mod abi;
pub mod codegen;
pub mod config;
pub mod group;
pub mod rhai;
pub mod runtime;
pub mod trace;

pub use config::{SboxConfig, SboxConfigError};
pub use group::{GroupFunc, load_group};
pub use rhai::{MockError, MockResult, RhaiMockLib};
pub use runtime::{BranchPolicy, RunContext, SandboxModule};
pub use trace::{CondEvent, CondKind, MockEvent, Trace, TraceEvent};

use codegraph_core::Result;
use codegraph_graph::GraphIndex;

/// Compile a group of symbols into a sandbox module (machine code) ready to run.
///
/// `ids` are the in-group symbol ids: calls between them are linked as real
/// compiled functions; every other callee (external or unresolved) is dispatched
/// to a Rhai mock at run time. A module runs one sandbox run at a time
/// (`SandboxModule::run`).
pub async fn compile(index: &GraphIndex, ids: &[u64], config: &SboxConfig) -> Result<SandboxModule> {
    compile_with_mocks(index, ids, config, &[]).await
}

/// Compile with per-call inline mock overrides (`name → rhai source`, either a
/// body or a full `fn <name>(args) { … }` script). Inline mocks win over mocks
/// loaded from `config.mock_dirs` — lets a caller mock specific functions (e.g.
/// from MCP args) instead of hitting a missing-mock fallback.
pub async fn compile_with_mocks(
    index: &GraphIndex,
    ids: &[u64],
    config: &SboxConfig,
    mocks: &[(String, String)],
) -> Result<SandboxModule> {
    let group = load_group(index, ids).await?;
    codegen::compile_group(&group, config, mocks)
}
