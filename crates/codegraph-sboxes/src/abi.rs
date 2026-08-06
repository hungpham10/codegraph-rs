//! The sandbox ABI: every value is an `i64`, and every compiled function and
//! runtime trampoline is `extern "C"` so the JIT can call in and out cleanly.
//!
//! ```text
//! host fn:         (ctx, nargs, args, ret) -> i64
//! mock_dispatch:   (ctx, callee_idx, nargs, args, ret) -> i64
//! eval_condition:  (ctx, cond_idx, rec_depth) -> i64
//! ```
//!
//! - `ctx` — opaque pointer to the [`crate::runtime::RunContext`] (per-run state).
//! - `args` — pointer to `nargs` i64 slots (abstract values, `i` for arg i).
//! - `ret`  — pointer to a single i64 slot (the function's return value).

use cranelift_codegen::ir::{AbiParam, Signature, types};
use cranelift_codegen::isa::CallConv;

fn i64() -> AbiParam {
    AbiParam::new(types::I64)
}

/// Native calling convention for the host triple.
fn call_conv() -> CallConv {
    CallConv::triple_default(&target_lexicon::Triple::host())
}

/// Signature of a compiled host function:
/// `(ctx, nargs, args, ret) -> i64`.
pub fn host_signature() -> Signature {
    let mut sig = Signature::new(call_conv());
    sig.params.push(i64()); // ctx
    sig.params.push(i64()); // nargs
    sig.params.push(i64()); // args
    sig.params.push(i64()); // ret
    sig.returns.push(i64());
    sig
}

/// Signature of the `mock_dispatch` import:
/// `(ctx, callee_idx, nargs, args, ret) -> i64`.
pub fn mock_signature() -> Signature {
    let mut sig = Signature::new(call_conv());
    sig.params.push(i64()); // ctx
    sig.params.push(i64()); // callee_idx
    sig.params.push(i64()); // nargs
    sig.params.push(i64()); // args
    sig.params.push(i64()); // ret
    sig.returns.push(i64());
    sig
}

/// Signature of the `eval_condition` import:
/// `(ctx, cond_idx, rec_depth) -> i64`.
pub fn cond_signature() -> Signature {
    let mut sig = Signature::new(call_conv());
    sig.params.push(i64()); // ctx
    sig.params.push(i64()); // cond_idx
    sig.params.push(i64()); // rec_depth
    sig.returns.push(i64());
    sig
}

/// Index of the `ctx` parameter in a host signature.
pub const PARAM_CTX: usize = 0;
/// Index of the `nargs` parameter in a host signature.
pub const PARAM_NARGS: usize = 1;
/// Index of the `args` pointer parameter in a host signature.
pub const PARAM_ARGS: usize = 2;
/// Index of the `ret` pointer parameter in a host signature.
pub const PARAM_RET: usize = 3;
