//! Chain → Cranelift structured-CFG lowering.
//!
//! Each group function's `FlowResult.chain` is a linear mix of markers (control
//! flow) and callee ids. This module lowers it into a real machine function:
//!
//! | chain marker                | lowered to                                   |
//! |-----------------------------|----------------------------------------------|
//! | `IF_TRUE`/`IF_FALSE`/`BRANCH_END` | `eval_condition` + `brif` + structured merge |
//! | `LOOP`/`LOOP_BACK`          | header condition + back edge (capped)        |
//! | `SWITCH_CASE`/`SWITCH_END`  | guarded case blocks, first-case policy       |
//! | `RETURN`                    | store result + jump to epilogue              |
//! | `BREAK`/`CONTINUE`/`THROW`  | jumps to innermost exit / header / epilogue  |
//! | callee id (in group)        | real call to the sibling compiled function   |
//! | callee id (outside/unresolved) | `mock_dispatch` (Rhai mock)               |
//!
//! Simplifications (documented, Piece-1 scope): condition side-effect calls
//! emitted right after `IF_TRUE`/`LOOP` run as the head of the taken branch /
//! loop body; recursion (`callee == self`) is mocked like any external callee so
//! runs always terminate.

use crate::abi;
use crate::group::{group_ids, GroupFunc};
use crate::runtime::{create_jit_module, SandboxModule};
use codegraph_core::{
    is_marker, Error, FlowResult, Result, SymbolId, MARKER_BRANCH_END, MARKER_BREAK,
    MARKER_CONTINUE, MARKER_IF_FALSE, MARKER_IF_TRUE, MARKER_LOOP, MARKER_LOOP_BACK,
    MARKER_REC_CALL, MARKER_RETURN, MARKER_SWITCH_CASE, MARKER_SWITCH_END, MARKER_THROW,
};
use cranelift_codegen::ir::{types, Block, FuncRef, InstBuilder, MemFlags, Value};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{Linkage, Module};
use std::collections::{HashMap, HashSet};

use crate::trace::CondKind;

/// One preprocessed chain element.
struct Item {
    tag: ItemTag,
    /// Chain position (diagnostics / arg lookup).
    #[allow(dead_code, reason = "kept for diagnostics on later pieces")]
    pos: usize,
    /// For `RETURN`: how many following Call items belong to the return expr.
    follow: usize,
}

enum ItemTag {
    Marker(u64),
    /// Call to a sibling compiled function.
    GroupCall {
        callee: SymbolId,
    },
    /// Call dispatched to a Rhai mock.
    MockCall {
        name: String,
        args: usize,
    },
}

/// Control-flow frame stack (matched against the marker nesting).
enum Frame {
    If {
        else_b: Block,
        merge_b: Block,
        seen_else: bool,
    },
    Loop {
        header: Block,
        exit: Block,
    },
    Switch {
        exit: Block,
        pending_next: Option<Block>,
        /// cond_idx of the first case. All cases of one statement share it so
        /// the runtime "first case taken" policy applies per statement.
        key: u64,
    },
}

/// Per-function lowering state.
///
/// All callee/import references are pre-imported into the function's IR before
/// the builder is created (the new `Module` API borrows the `Function` mutably),
/// so the walker only needs the pre-resolved `FuncRef`s.
struct Lower<'a> {
    fb: FunctionBuilder<'a>,
    /// In-group callee id → already-imported `FuncRef`.
    callee_refs: &'a HashMap<SymbolId, FuncRef>,
    mock_ref: FuncRef,
    cond_ref: FuncRef,
    name_table: &'a mut Vec<String>,
    name_idx: &'a mut HashMap<String, u64>,
    cond_table: &'a mut Vec<CondKind>,
    cond_counter: &'a mut u64,
    ctx_val: Value,
    /// Function-signature parameter, bound by the ABI. Not read in the body
    /// (args are consumed via the arena pointer) but part of the contract.
    #[allow(dead_code, reason = "ABI parameter; bound for signature completeness")]
    nargs_val: Value,
    args_val: Value,
    ret_val: Value,
    /// The function's "last expression result", tracked as a frontend variable
    /// so the frontend inserts phis wherever control flow merges (an epilogue
    /// store/return must work no matter which branch produced the value).
    last: Variable,
    epilogue: Block,
    current: Block,
    terminated: bool,
    all_blocks: Vec<Block>,
    terminated_blocks: HashSet<Block>,
    frames: Vec<Frame>,
    break_targets: Vec<Block>,
    continue_targets: Vec<Block>,
    items: Vec<Item>,
}

impl<'a> Lower<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        mut fb: FunctionBuilder<'a>,
        callee_refs: &'a HashMap<SymbolId, FuncRef>,
        mock_ref: FuncRef,
        cond_ref: FuncRef,
        name_table: &'a mut Vec<String>,
        name_idx: &'a mut HashMap<String, u64>,
        cond_table: &'a mut Vec<CondKind>,
        cond_counter: &'a mut u64,
        items: Vec<Item>,
    ) -> Self {
        let entry = fb.create_block();
        fb.switch_to_block(entry);
        fb.append_block_params_for_function_params(entry);
        let params = fb.block_params(entry);
        let ctx_val = params[abi::PARAM_CTX];
        let nargs_val = params[abi::PARAM_NARGS];
        let args_val = params[abi::PARAM_ARGS];
        let ret_val = params[abi::PARAM_RET];
        let epilogue = fb.create_block();
        let zero = fb.ins().iconst(types::I64, 0);
        let last = Variable::from_u32(0);
        fb.declare_var(last, types::I64);
        fb.def_var(last, zero);
        Self {
            fb,
            callee_refs,
            mock_ref,
            cond_ref,
            name_table,
            name_idx,
            cond_table,
            cond_counter,
            ctx_val,
            nargs_val,
            args_val,
            ret_val,
            last,
            epilogue,
            current: entry,
            terminated: false,
            all_blocks: vec![entry, epilogue],
            terminated_blocks: HashSet::new(),
            frames: Vec::new(),
            break_targets: Vec::new(),
            continue_targets: Vec::new(),
            items,
        }
    }

    fn build(&mut self) -> Result<()> {
        let items = std::mem::take(&mut self.items);
        let mut i = 0usize;
        while i < items.len() {
            let is_case = matches!(items[i].tag, ItemTag::Marker(m) if m == MARKER_SWITCH_CASE);
            self.maybe_close_switch(is_case);
            match &items[i].tag {
                ItemTag::Marker(m) => match *m {
                    MARKER_IF_TRUE => self.emit_if_true(),
                    MARKER_IF_FALSE => self.emit_if_false(),
                    MARKER_BRANCH_END => self.emit_branch_end(),
                    MARKER_LOOP => self.emit_loop(),
                    MARKER_LOOP_BACK => self.emit_loop_back(),
                    MARKER_SWITCH_CASE => self.emit_switch_case(),
                    MARKER_SWITCH_END => self.emit_switch_end(),
                    MARKER_BREAK => self.emit_break(),
                    MARKER_CONTINUE => self.emit_continue(),
                    MARKER_THROW => {
                        let n = items[i].follow;
                        i += 1;
                        for _ in 0..n {
                            if i < items.len() {
                                self.emit_call(&items[i]);
                                i += 1;
                            }
                        }
                        self.emit_throw();
                        continue;
                    }
                    MARKER_RETURN => {
                        let n = items[i].follow;
                        i += 1;
                        for _ in 0..n {
                            if i < items.len() {
                                self.emit_call(&items[i]);
                                i += 1;
                            }
                        }
                        self.emit_return_tail();
                        continue;
                    }
                    MARKER_REC_CALL => { /* recursion is mocked; nothing to do */ }
                    _ => {}
                },
                ItemTag::GroupCall { .. } | ItemTag::MockCall { .. } => self.emit_call(&items[i]),
            }
            i += 1;
        }
        self.maybe_close_switch(false);
        self.finish();
        Ok(())
    }

    // ---- helpers ----

    fn iconst(&mut self, v: i64) -> Value {
        self.fb.ins().iconst(types::I64, v)
    }

    fn jump(&mut self, target: Block) {
        self.fb.ins().jump(target, &[]);
        self.terminated_blocks.insert(self.current);
        self.terminated = true;
    }

    fn begin_block(&mut self, b: Block) {
        self.fb.switch_to_block(b);
        self.current = b;
        self.terminated = false;
    }

    fn ensure_alive(&mut self) {
        if self.terminated {
            let b = self.fb.create_block();
            self.all_blocks.push(b);
            self.begin_block(b);
        }
    }

    fn jump_epilogue(&mut self) {
        self.jump(self.epilogue);
    }

    fn eval_condition(&mut self, kind: CondKind) -> (u64, Value) {
        let idx = *self.cond_counter;
        *self.cond_counter += 1;
        self.cond_table.push(kind);
        let idx_v = self.iconst(idx as i64);
        let depth_v = self.iconst(0);
        let inst = self
            .fb
            .ins()
            .call(self.cond_ref, &[self.ctx_val, idx_v, depth_v]);
        (idx, self.fb.inst_results(inst)[0])
    }

    /// Evaluate a condition with a *specific* cond_idx. Used by subsequent
    /// switch cases so they share the first case's key (and thus the runtime
    /// decides them per statement, not per case).
    fn eval_condition_at(&mut self, idx: u64) -> Value {
        let idx_v = self.iconst(idx as i64);
        let depth_v = self.iconst(0);
        let inst = self
            .fb
            .ins()
            .call(self.cond_ref, &[self.ctx_val, idx_v, depth_v]);
        self.fb.inst_results(inst)[0]
    }

    fn name_idx(&mut self, name: &str) -> u64 {
        if let Some(&i) = self.name_idx.get(name) {
            return i;
        }
        let i = self.name_table.len() as u64;
        self.name_table.push(name.to_string());
        self.name_idx.insert(name.to_string(), i);
        i
    }

    // ---- control flow ----

    fn emit_if_true(&mut self) {
        self.ensure_alive();
        let then_b = self.fb.create_block();
        let else_b = self.fb.create_block();
        let merge_b = self.fb.create_block();
        self.all_blocks.extend([then_b, else_b, merge_b]);
        let (_idx, c) = self.eval_condition(CondKind::If);
        self.fb.ins().brif(c, then_b, &[], else_b, &[]);
        self.terminated_blocks.insert(self.current);
        self.terminated = true;
        self.frames.push(Frame::If {
            else_b,
            merge_b,
            seen_else: false,
        });
        self.begin_block(then_b);
    }

    fn emit_if_false(&mut self) {
        // Copy the blocks out first so we don't hold a borrow on `self.frames`
        // while also mutating `self` (jump/begin_block).
        let pending = match self.frames.last() {
            Some(Frame::If {
                else_b,
                merge_b,
                seen_else: false,
            }) => Some((*else_b, *merge_b)),
            _ => None,
        };
        if let Some((else_b, merge_b)) = pending {
            if let Some(Frame::If { seen_else, .. }) = self.frames.last_mut() {
                *seen_else = true;
            }
            if !self.terminated {
                self.jump(merge_b);
            }
            self.begin_block(else_b);
        }
    }

    fn emit_branch_end(&mut self) {
        if let Some(Frame::If {
            else_b,
            merge_b,
            seen_else,
        }) = self.frames.pop()
        {
            if !self.terminated {
                self.jump(merge_b);
            }
            if seen_else {
                self.begin_block(merge_b);
            } else {
                self.begin_block(else_b);
                self.jump(merge_b);
                self.begin_block(merge_b);
            }
        }
    }

    fn emit_loop(&mut self) {
        self.ensure_alive();
        let header = self.fb.create_block();
        let body = self.fb.create_block();
        let exit = self.fb.create_block();
        self.all_blocks.extend([header, body, exit]);
        self.jump(header);
        self.begin_block(header);
        let (_idx, c) = self.eval_condition(CondKind::Loop);
        self.fb.ins().brif(c, body, &[], exit, &[]);
        self.terminated_blocks.insert(self.current);
        self.terminated = true;
        self.frames.push(Frame::Loop { header, exit });
        self.break_targets.push(exit);
        self.continue_targets.push(header);
        self.begin_block(body);
    }

    fn emit_loop_back(&mut self) {
        if let Some(Frame::Loop { header, exit }) = self.frames.pop() {
            self.break_targets.pop();
            self.continue_targets.pop();
            if !self.terminated {
                self.jump(header);
            }
            self.begin_block(exit);
        }
    }

    fn emit_switch_case(&mut self) {
        self.ensure_alive();
        let has_open_switch = matches!(self.frames.last(), Some(Frame::Switch { .. }));
        if has_open_switch {
            // Subsequent case: dispatch from the transition block left by the
            // previous `SWITCH_END` (pending_next was `None` until now).
            let case_b = self.fb.create_block();
            let next_b = self.fb.create_block();
            self.all_blocks.extend([case_b, next_b]);
            let key = match self.frames.last() {
                Some(Frame::Switch { key, .. }) => *key,
                _ => unreachable!(),
            };
            let c = self.eval_condition_at(key);
            self.fb.ins().brif(c, case_b, &[], next_b, &[]);
            self.terminated_blocks.insert(self.current);
            self.terminated = true;
            if let Some(Frame::Switch { pending_next, .. }) = self.frames.last_mut() {
                *pending_next = Some(next_b);
            }
            self.begin_block(case_b);
        } else {
            // First case — create the switch frame.
            let exit = self.fb.create_block();
            let case_b = self.fb.create_block();
            let next_b = self.fb.create_block();
            self.all_blocks.extend([exit, case_b, next_b]);
            let (key, c) = self.eval_condition(CondKind::Switch);
            self.fb.ins().brif(c, case_b, &[], next_b, &[]);
            self.terminated_blocks.insert(self.current);
            self.terminated = true;
            self.frames.push(Frame::Switch {
                exit,
                pending_next: Some(next_b),
                key,
            });
            self.break_targets.push(exit);
            self.begin_block(case_b);
        }
    }

    fn emit_switch_end(&mut self) {
        if let Some(Frame::Switch { pending_next, .. }) = self.frames.last_mut() {
            if let Some(next_b) = pending_next.take() {
                if !self.terminated {
                    self.jump(next_b);
                }
                self.begin_block(next_b);
            }
        }
    }

    fn emit_break(&mut self) {
        self.ensure_alive();
        if let Some(&target) = self.break_targets.last() {
            self.jump(target);
        }
    }

    fn emit_continue(&mut self) {
        self.ensure_alive();
        if let Some(&target) = self.continue_targets.last() {
            self.jump(target);
        }
    }

    fn emit_return_tail(&mut self) {
        self.ensure_alive();
        self.jump_epilogue();
    }

    fn emit_throw(&mut self) {
        self.ensure_alive();
        let minus_one = self.iconst(-1);
        self.fb.def_var(self.last, minus_one);
        self.jump_epilogue();
    }

    fn emit_call(&mut self, item: &Item) {
        self.ensure_alive();
        let n = match &item.tag {
            ItemTag::GroupCall { .. } => 0,
            ItemTag::MockCall { args, .. } => *args,
            ItemTag::Marker(_) => return,
        };
        // The callee receives `nargs` abstract args from the shared arena.
        // The arena is preloaded by the runtime with the *entry* args, so a
        // callee called from the entry actually sees the caller's values; the
        // abstract-value model collapses any deeper expressions, but the slots
        // are deterministic (i = arg i). Pass the arena pointer as-is.
        let nargs_c = self.iconst(n as i64);
        let inst = match &item.tag {
            ItemTag::GroupCall { callee } => {
                let fid = *callee;
                let fref = self.callee_refs[&fid];
                self.fb
                    .ins()
                    .call(fref, &[self.ctx_val, nargs_c, self.args_val, self.ret_val])
            }
            ItemTag::MockCall { name, .. } => {
                let idx = self.name_idx(name);
                let idx_c = self.iconst(idx as i64);
                self.fb.ins().call(
                    self.mock_ref,
                    &[self.ctx_val, idx_c, nargs_c, self.args_val, self.ret_val],
                )
            }
            ItemTag::Marker(_) => unreachable!(),
        };
        let result = self.fb.inst_results(inst)[0];
        self.fb.def_var(self.last, result);
    }

    fn maybe_close_switch(&mut self, next_is_case: bool) {
        if next_is_case {
            return;
        }
        while let Some(Frame::Switch {
            exit,
            pending_next: None,
            ..
        }) = self.frames.last()
        {
            let exit = *exit;
            self.frames.pop();
            self.break_targets.pop();
            let cont = self.current;
            self.begin_block(exit);
            self.jump(cont);
            self.begin_block(cont);
        }
    }

    fn finish(&mut self) {
        if !self.terminated {
            self.jump_epilogue();
        }
        // Epilogue: store `last` to *ret and return it. `use_var` pulls the
        // value through the phis the frontend inserted at merge points.
        self.begin_block(self.epilogue);
        let last = self.fb.use_var(self.last);
        self.fb.ins().store(MemFlags::new(), last, self.ret_val, 0);
        self.fb.ins().return_(&[last]);
        self.terminated_blocks.insert(self.epilogue);
        self.terminated = true;
        // Terminate any block left dangling (dead switch exit, empty branches…).
        for &b in &self.all_blocks.clone() {
            if !self.terminated_blocks.contains(&b) {
                self.begin_block(b);
                self.jump_epilogue();
            }
        }
    }
}

/// Render a `ModuleError` to a string, expanding verifier errors so the real
/// cause (not just "Verifier errors") surfaces in diagnostics.
fn describe_module_error(e: &cranelift_module::ModuleError) -> String {
    use cranelift_module::ModuleError;
    match e {
        ModuleError::Compilation(cranelift_codegen::CodegenError::Verifier(errs)) => {
            let detail: Vec<String> = errs.0.iter().map(|e| e.to_string()).collect();
            format!("Compilation error (verifier): {}", detail.join("; "))
        }
        ModuleError::Compilation(other) => format!("Compilation error: {other}"),
        other => other.to_string(),
    }
}

/// Compile a group of functions into a runnable sandbox module.
///
/// `inline_mocks` (name → rhai source) are registered per-call, overriding file
/// mocks of the same name — used so a caller can mock specific functions.
pub fn compile_group(
    group: &[GroupFunc],
    config: &crate::config::SboxConfig,
    inline_mocks: &[(String, String)],
) -> Result<SandboxModule> {
    let mut module = create_jit_module()?;
    let ids = group_ids(group);
    let merr = |e: cranelift_module::ModuleError| Error::Other(describe_module_error(&e));

    // Pass 0 — link-time mock validation: load the mock library (file + inline)
    // up front and fail BEFORE generating any code if a callee that will be
    // mock-dispatched has no mock configured. The caller gets the exact list of
    // functions to mock instead of silently running a `0` fallback.
    let mocks = crate::rhai::RhaiMockLib::load_with_mocks(
        config.root.as_std_path(),
        &config.mock_dirs,
        inline_mocks,
    );
    let mut missing = Vec::new();
    let mut seen_names = HashSet::new();
    for f in group {
        for it in build_items(f, &ids) {
            if let ItemTag::MockCall { name, .. } = &it.tag {
                if seen_names.insert(name.clone()) && !mocks.has(name) {
                    missing.push(name.clone());
                }
            }
        }
    }
    missing.sort_unstable();
    if !missing.is_empty() {
        return Err(Error::MissingMocks(missing));
    }

    // Pass 1 — declare all group functions so sibling calls can link, plus the
    // two runtime imports (resolved by name to the trampolines in `runtime`).
    let mut func_ids = HashMap::new();
    for f in group {
        let fid = module
            .declare_function(&func_name(f), Linkage::Local, &abi::host_signature())
            .map_err(merr)?;
        func_ids.insert(f.id, fid);
    }
    let mock_func = module
        .declare_function("mock_dispatch", Linkage::Import, &abi::mock_signature())
        .map_err(merr)?;
    let cond_func = module
        .declare_function("eval_condition", Linkage::Import, &abi::cond_signature())
        .map_err(merr)?;

    let mut name_table = Vec::new();
    let mut name_idx = HashMap::new();
    let mut cond_table = Vec::new();
    let mut cond_counter = 0u64;
    let mut entry = None;

    // Pass 2 — build each function body.
    for f in group {
        let items = build_items(f, &ids);
        let mut ctx = module.make_context();
        // The entry block params mirror the declared host signature, so the
        // IR function's signature must be populated before we build its body.
        ctx.func.signature.params = abi::host_signature().params;
        ctx.func.signature.returns = abi::host_signature().returns;

        // Pre-import every referenced callee + the two trampolines into this
        // function's IR (the `Module` API borrows the `Function` mutably, so it
        // must happen before the `FunctionBuilder` is created).
        let mut callee_refs = HashMap::new();
        for it in &items {
            if let ItemTag::GroupCall { callee } = &it.tag {
                let fid = func_ids[callee];
                let fref = module.declare_func_in_func(fid, &mut ctx.func);
                callee_refs.insert(*callee, fref);
            }
        }
        let mock_ref = module.declare_func_in_func(mock_func, &mut ctx.func);
        let cond_ref = module.declare_func_in_func(cond_func, &mut ctx.func);

        let mut fbc = FunctionBuilderContext::new();
        let mut fb = FunctionBuilder::new(&mut ctx.func, &mut fbc);
        {
            let mut lower = Lower::new(
                fb,
                &callee_refs,
                mock_ref,
                cond_ref,
                &mut name_table,
                &mut name_idx,
                &mut cond_table,
                &mut cond_counter,
                items,
            );
            lower.build()?;
            fb = lower.fb;
            fb.seal_all_blocks();
            fb.finalize();
        }
        drop(fbc);
        let fid = func_ids[&f.id];
        module.define_function(fid, &mut ctx).map_err(merr)?;
        if entry.is_none() {
            entry = Some(fid);
        }
    }

    module.finalize_definitions().map_err(merr)?;

    Ok(SandboxModule {
        jit: module,
        func_ids,
        entry: entry.expect("group must not be empty"),
        name_table,
        cond_table,
        mocks,
        policy: config.branch_policy,
        loop_cap: config.loop_cap,
    })
}

/// Unique module-local name for a group function.
fn func_name(f: &GroupFunc) -> String {
    format!("fn_{}", f.id)
}

/// Preprocess a flow's chain into walkable items.
fn build_items(f: &GroupFunc, ids: &HashSet<SymbolId>) -> Vec<Item> {
    let flow: &FlowResult = &f.flow;
    let mut pos_args = HashMap::new();
    for c in &flow.calls {
        pos_args.insert(c.position, c.args.len());
    }
    let mut items = Vec::new();
    for (i, &e) in flow.chain.iter().enumerate() {
        if i == 0 {
            continue; // position 0 is the function itself
        }
        if is_marker(e) {
            items.push(Item {
                tag: ItemTag::Marker(e),
                pos: i,
                follow: 0,
            });
        } else if e == f.id {
            // Recursion: mocked, like any external callee (termination guard).
            items.push(Item {
                tag: ItemTag::MockCall {
                    name: flow
                        .chain_desc
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| e.to_string()),
                    args: pos_args.get(&i).copied().unwrap_or(0),
                },
                pos: i,
                follow: 0,
            });
        } else if ids.contains(&e) {
            items.push(Item {
                tag: ItemTag::GroupCall { callee: e },
                pos: i,
                follow: 0,
            });
        } else {
            items.push(Item {
                tag: ItemTag::MockCall {
                    name: flow
                        .chain_desc
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| e.to_string()),
                    args: pos_args.get(&i).copied().unwrap_or(0),
                },
                pos: i,
                follow: 0,
            });
        }
    }
    // RETURN/THROW expr-lookahead: count consecutive Call items after each
    // jump marker (the expression is evaluated before the jump happens).
    for i in 0..items.len() {
        if let ItemTag::Marker(m) = items[i].tag {
            if m == MARKER_RETURN || m == MARKER_THROW {
                let mut n = 0;
                let mut j = i + 1;
                while j < items.len()
                    && matches!(
                        items[j].tag,
                        ItemTag::GroupCall { .. } | ItemTag::MockCall { .. }
                    )
                {
                    n += 1;
                    j += 1;
                }
                items[i].follow = n;
            }
        }
    }
    items
}
