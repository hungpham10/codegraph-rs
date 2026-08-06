//! JIT runtime: owns the cranelift module, provides the two trampolines the
//! compiled code imports (`mock_dispatch`, `eval_condition`), and runs a group.
//!
//! A compiled function is `extern "C" fn(ctx, nargs, args, ret) -> i64` where:
//! - `ctx`  — `*mut RunContext` (per-run state; thread-confined to one run).
//! - `args` — pointer to `nargs` i64 slots; doubles as the shared scratch
//!   arena for nested call args (values are consumed synchronously).
//! - `ret`  — pointer to one i64 slot where the function stores its result.

use crate::rhai::{MockError, RhaiMockLib};
use crate::trace::{CondEvent, CondKind, MockEvent, Trace, TraceEvent};
use codegraph_core::{Error, Result};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, default_libcall_names};
use std::cell::RefCell;
use std::collections::HashMap;

/// How conditions are resolved at run time (deterministic for now; steerable
/// per test later).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchPolicy {
    /// Every `if` takes its then-branch; switches take their first case.
    IfTrue,
    /// Every `if` takes its else-branch (if the chain has one).
    IfFalse,
}

/// Scratch arena size (i64 slots). Bounded — nested calls reuse slots.
pub const ARENA_SLOTS: usize = 4096;

/// Everything the two trampolines need for one run. Mutable through the raw
/// `ctx` pointer; never shared between threads for a single run.
pub struct RunContext {
    pub mocks: RhaiMockLib,
    pub name_table: Vec<String>,
    pub cond_table: Vec<CondKind>,
    pub policy: BranchPolicy,
    pub loop_cap: usize,
    pub trace: RefCell<Trace>,
    /// Loop iteration counters (cond_idx → hits) so loops always terminate.
    pub loop_hits: HashMap<u64, usize>,
    /// Switch "first case taken" state per cond_idx.
    pub switch_taken: HashMap<u64, bool>,
}

impl RunContext {
    fn decide(&mut self, kind: CondKind, idx: u64) -> bool {
        match kind {
            CondKind::If => match self.policy {
                BranchPolicy::IfTrue => true,
                BranchPolicy::IfFalse => false,
            },
            CondKind::Loop => {
                let n = self.loop_hits.entry(idx).or_insert(0);
                *n += 1;
                *n <= self.loop_cap
            }
            CondKind::Switch => {
                let first = self.switch_taken.entry(idx).or_insert(true);
                let r = *first;
                *first = false;
                r
            }
        }
    }
}

/// The compiled group: machine code + run-time metadata (callee name table,
/// condition table, mock library, policy). `run` re-lends the mock library for
/// the duration of a run, so a module is used by one run at a time.
pub struct SandboxModule {
    pub(crate) jit: JITModule,
    /// In-group symbol id → compiled function.
    pub func_ids: HashMap<u64, FuncId>,
    /// The entry function for a run.
    pub entry: FuncId,
    /// `callee_idx` (embedded in code) → callee name for mock dispatch.
    pub name_table: Vec<String>,
    /// `cond_idx` (embedded in code) → condition kind.
    pub cond_table: Vec<CondKind>,
    pub mocks: RhaiMockLib,
    pub policy: BranchPolicy,
    pub loop_cap: usize,
}

impl SandboxModule {
    /// Run the entry function with abstract `args`. Returns the result value
    /// and the observed behavior trace.
    pub fn run(&mut self, args: &[i64]) -> (i64, Trace) {
        self.run_func(self.entry, args)
    }

    /// Run an arbitrary compiled function in this module.
    pub fn run_func(&mut self, func: FuncId, args: &[i64]) -> (i64, Trace) {
        let f: unsafe extern "C" fn(*mut RunContext, u64, *mut i64, *mut i64) -> i64 =
            unsafe { std::mem::transmute::<*const u8, _>(self.jit.get_finalized_function(func)) };

        let mut arena = vec![0i64; ARENA_SLOTS];
        for (i, a) in args.iter().take(ARENA_SLOTS).enumerate() {
            arena[i] = *a;
        }
        let mut ret: i64 = 0;
        let mut rc = RunContext {
            mocks: std::mem::take(&mut self.mocks),
            name_table: self.name_table.clone(),
            cond_table: self.cond_table.clone(),
            policy: self.policy,
            loop_cap: self.loop_cap,
            trace: RefCell::new(Trace::default()),
            loop_hits: HashMap::new(),
            switch_taken: HashMap::new(),
        };
        let result = unsafe {
            f(
                &mut rc as *mut RunContext,
                args.len() as u64,
                arena.as_mut_ptr(),
                &mut ret,
            )
        };
        let trace = rc.trace.into_inner();
        self.mocks = std::mem::take(&mut rc.mocks);
        (result, trace)
    }
}

/// Build a JIT module with the two runtime imports wired to this module's
/// trampolines. The trampoline symbols are global (process-wide), so a module
/// is bound to them by name; the per-run state travels via `ctx`.
///
/// The native ISA is built with `is_pic = false`: cranelift-jit's PIC path
/// allocates a PLT entry per declared function, which is x86-only, and the host
/// here is arm64.
pub fn create_jit_module() -> Result<JITModule> {
    let mut flag_builder = settings::builder();
    flag_builder
        .set("is_pic", "false")
        .map_err(|e| Error::Other(e.to_string()))?;
    flag_builder
        .set("use_colocated_libcalls", "false")
        .map_err(|e| Error::Other(e.to_string()))?;
    let isa_builder = cranelift_native::builder().map_err(|e| Error::Other(e.to_string()))?;
    let isa = isa_builder
        .finish(settings::Flags::new(flag_builder))
        .map_err(|e| Error::Other(e.to_string()))?;
    let mut builder = JITBuilder::with_isa(isa, default_libcall_names());
    builder.symbol("mock_dispatch", mock_dispatch_trampoline as *const u8);
    builder.symbol("eval_condition", eval_condition_trampoline as *const u8);
    Ok(JITModule::new(builder))
}

/// `(ctx, callee_idx, nargs, args, ret) -> i64` — dispatch one callee to its
/// Rhai mock and record it in the trace.
unsafe extern "C" fn mock_dispatch_trampoline(
    ctx: *mut RunContext,
    callee_idx: u64,
    nargs: u64,
    args: *mut i64,
    ret: *mut i64,
) -> i64 {
    let rc = &mut *ctx;
    let name = rc
        .name_table
        .get(callee_idx as usize)
        .cloned()
        .unwrap_or_else(|| format!("unknown({callee_idx})"));
    let arg_count = (nargs as usize).min(64);
    let mut argvals = Vec::with_capacity(arg_count);
    for i in 0..arg_count {
        argvals.push(*args.add(i));
    }
    let result = match rc.mocks.call(&name, &argvals) {
        Ok(v) => v,
        Err(MockError::NotFound(_)) => {
            // No file or inline mock — record the miss so the caller sees what
            // still needs mocking (the run itself returns the `0` fallback).
            rc.trace.borrow_mut().missing.push(name.clone());
            0
        }
        Err(_) => 0,
    };
    *ret = result;
    let event = MockEvent {
        callee: name,
        args: argvals,
        result,
    };
    rc.trace.borrow_mut().mocks.push(event.clone());
    rc.trace.borrow_mut().events.push(TraceEvent::Mock(event));
    result
}

/// `(ctx, cond_idx, rec_depth) -> i64` — resolve one control-flow condition
/// from the policy and record the decision.
unsafe extern "C" fn eval_condition_trampoline(
    ctx: *mut RunContext,
    cond_idx: u64,
    _rec_depth: u64,
) -> i64 {
    let rc = &mut *ctx;
    let kind = rc
        .cond_table
        .get(cond_idx as usize)
        .copied()
        .unwrap_or(CondKind::If);
    let result = rc.decide(kind, cond_idx);
    let event = CondEvent {
        kind,
        idx: cond_idx,
        result,
    };
    rc.trace.borrow_mut().conds.push(event.clone());
    rc.trace.borrow_mut().events.push(TraceEvent::Cond(event));
    i64::from(result)
}
