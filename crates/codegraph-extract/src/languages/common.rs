//! Generic semgraph-style parser engine.
//!
//! Một `LangSpec` mô tả cú pháp một ngôn ngữ (declaration nodes, call rules,
//! marker rules) và `run_spec` chạy pipeline 2 pass trên tree-sitter tree:
//!
//! 1. **Symbol pass** — collect symbol declarations với id local (≥ `SYMBOL_BASE`),
//!    scope stack (class → ObjectField, function → Local/Parameter), type_name cho
//!    variable/field/param + resolve `type_ref` trong cùng file.
//! 2. **Chain pass** — với mỗi function/method, walk body phát marker
//!    (`IF_TRUE`/`IF_FALSE`/`BRANCH_END`, `LOOP`/`LOOP_BACK`, `SWITCH_CASE`/
//!    `SWITCH_END`, `RETURN`/`BREAK`/`CONTINUE`/`THROW`) + placeholder `0` cho
//!    call site (kèm `CallRecord`) — tầng `GraphIndex::ingest` resolve sau.
//!
//! Kết quả là `codegraph_graph::ParseResult` — input của pipeline 2 phase.

use crate::languages::effects::classify_effect;
use codegraph_core::{
    Annotation, CallRecord, Result, ScopeLevel, Symbol, SymbolKind, MARKER_BRANCH_END,
    MARKER_BREAK, MARKER_CONTINUE, MARKER_IF_FALSE, MARKER_IF_TRUE, MARKER_LOOP, MARKER_LOOP_BACK,
    MARKER_RETURN, MARKER_SWITCH_CASE, MARKER_SWITCH_END, MARKER_THROW, SYMBOL_BASE,
};
use codegraph_graph::ParseResult;
use std::collections::HashMap;
use tree_sitter::{Node, Parser, Tree};

/// Custom call-name extractor: `(call node, src) -> tên callee đầy đủ`.
pub type CallNameFn = fn(&Node, &[u8]) -> Option<String>;

/// Structural target hint: `(call node, src) -> (class, method)` — VD Java class
/// literal `Foo.class.bar()` → `("Foo", "bar")`. Trả `(None, None)` nếu không có.
pub type TargetFn = fn(&Node, &[u8]) -> (Option<String>, Option<String>);

/// Post-process class symbol: `(class node, src) -> type_name` (VD TS heritage).
pub type ClassTypeFn = fn(&Node, &[u8]) -> Option<String>;

/// Một call-site rule: node kind nào là call + callee field + cách lấy tên.
#[derive(Clone, Copy)]
pub struct CallRule {
    pub kind: &'static str,
    /// Field chứa callee expression. Chuỗi rỗng = dùng named child đầu tiên.
    pub callee_field: &'static str,
    /// Field chứa argument list.
    pub arguments_field: &'static str,
    pub name_fn: Option<CallNameFn>,
    pub target_fn: Option<TargetFn>,
}

/// Declarative spec của một ngôn ngữ.
#[allow(clippy::struct_excessive_bools)]
pub struct LangSpec {
    pub language_name: &'static str,
    pub extensions: &'static [&'static str],
    pub ts_language: fn() -> tree_sitter::Language,
    /// (node kind, SymbolKind) — declaration nodes.
    pub decls: &'static [(&'static str, SymbolKind)],
    /// Node kinds có body function — chain được build cho từng decl này.
    pub func_kinds: &'static [&'static str],
    /// Class-like node kinds — children thuộc ObjectField scope.
    pub class_kinds: &'static [&'static str],
    /// Parameter node kinds — scope Parameter, scope_id = function chứa.
    pub param_kinds: &'static [&'static str],
    /// Annotation node kinds (VD Java `annotation`/`marker_annotation`).
    pub annotation_kinds: &'static [&'static str],
    /// Call rules.
    pub calls: &'static [CallRule],
    /// Lấy type_name cho class symbol (TS `extends`/`implements` heritage).
    pub class_type_name: Option<ClassTypeFn>,
    /// Cho phép dùng field `type` làm tên khi thiếu name/declarator (VD Rust
    /// `impl Foo` — tên nằm ở `type`). Bật cho ngôn ngữ không có node kind
    /// xung đột (C# `variable_declaration{type}` phải để false).
    pub name_type_fallback: bool,
    // ── marker rules ──
    pub if_kinds: &'static [&'static str],
    pub elif_kinds: &'static [&'static str],
    /// Block-like node kinds — fallback consequence/alternative khi thiếu field
    /// (VD Swift `if` dùng node `statements` trần, không có consequence field).
    pub if_block_kinds: &'static [&'static str],
    pub loop_kinds: &'static [&'static str],
    pub switch_kinds: &'static [&'static str],
    /// Wrapper node quanh case children (VD Java `switch_block`).
    pub switch_block_kinds: &'static [&'static str],
    pub switch_case_kinds: &'static [&'static str],
    pub switch_default_kinds: &'static [&'static str],
    pub return_kinds: &'static [&'static str],
    pub break_kinds: &'static [&'static str],
    pub continue_kinds: &'static [&'static str],
    pub throw_kinds: &'static [&'static str],
    pub try_kinds: &'static [&'static str],
    pub except_kinds: &'static [&'static str],
    pub try_else_kinds: &'static [&'static str],
    pub finally_kinds: &'static [&'static str],
    // ── field names ──
    pub if_cond_field: &'static str,
    pub if_cons_field: &'static str,
    pub if_alt_field: &'static str,
    pub body_field: &'static str,
}

// ==================== Pipeline ====================

/// Chạy pipeline đầy đủ cho một file → `ParseResult` (input của `GraphIndex::ingest`).
pub fn run_spec(
    spec: &'static LangSpec,
    path: &str,
    language: &str,
    source: &str,
) -> Result<ParseResult> {
    let tree = parse_tree(spec, source)?;
    let root = tree.root_node();
    let src = source.as_bytes();

    // ── Pass 1: symbols ──
    let mut ctx = SymbolCtx {
        src,
        file: path,
        language,
        symbols: Vec::new(),
        next_id: SYMBOL_BASE,
        scope_stack: Vec::new(),
    };
    collect_symbols(&root, &mut ctx, spec);
    let mut symbols = ctx.symbols;
    resolve_type_refs(&mut symbols);

    // func_index: (name, line) → id — overload-safe (method trùng tên khác line).
    let func_index: HashMap<(String, u32), u64> = symbols
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
        .map(|s| ((s.name.clone(), s.line), s.id))
        .collect();

    // class_index: (name, line) → id — cho chain tối thiểu của class-like node.
    let class_index: HashMap<(String, u32), u64> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum | SymbolKind::Module
            )
        })
        .map(|s| ((s.name.clone(), s.line), s.id))
        .collect();

    // ── Pass 2: chains ──
    let mut chains: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut calls: Vec<CallRecord> = Vec::new();
    collect_chains(
        &root,
        src,
        spec,
        &func_index,
        &class_index,
        &mut chains,
        &mut calls,
    );

    Ok(ParseResult {
        path: path.to_string(),
        language: language.to_string(),
        bytes: source.len() as u64,
        lines: source.lines().count() as u32,
        symbols,
        chains,
        calls,
    })
}

fn parse_tree(spec: &'static LangSpec, source: &str) -> Result<Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&(spec.ts_language)())
        .map_err(|e| crate::parse_err(format!("set_language: {e}")))?;
    parser
        .parse(source, None)
        .ok_or_else(|| crate::parse_err("parse failed"))
}

// ==================== Pass 1: symbols ====================

struct SymbolCtx<'a> {
    src: &'a [u8],
    file: &'a str,
    language: &'a str,
    symbols: Vec<Symbol>,
    next_id: u64,
    /// Scope owner stack: `(id, is_class_like)` — innermost scope của node đang xét.
    scope_stack: Vec<(u64, bool)>,
}

fn collect_symbols(node: &Node, ctx: &mut SymbolCtx, spec: &'static LangSpec) {
    let k = node.kind();
    if let Some(&(_, skind)) = spec.decls.iter().find(|(s, _)| *s == k) {
        if let Some(id) = push_symbol(ctx, node, spec, skind, k) {
            let is_class = spec.class_kinds.contains(&k);
            let is_scope = is_class || spec.func_kinds.contains(&k);
            if is_scope {
                ctx.scope_stack.push((id, is_class));
            }
            for ch in named_children(node) {
                collect_symbols(&ch, ctx, spec);
            }
            if is_scope {
                ctx.scope_stack.pop();
            }
            return;
        }
    }
    for ch in named_children(node) {
        collect_symbols(&ch, ctx, spec);
    }
}

fn push_symbol(
    ctx: &mut SymbolCtx,
    node: &Node,
    spec: &'static LangSpec,
    kind: SymbolKind,
    node_kind: &str,
) -> Option<u64> {
    // C/C++: macro attribute trước qualified ctor (`_CUSTOM_ATTRIBUTE
    // CustomWidget<T>::CustomWidget(...)`) làm tree-sitter đánh ERROR — field
    // `declarator` chỉ vào init_declarator sai; tên ctor nằm trong function_declarator
    // của ERROR child.
    let from_error_ctor = node_kind == "declaration" && error_ctor_name(node).is_some();
    let name_node = if from_error_ctor {
        error_ctor_name(node)
    } else {
        node.child_by_field_name("name")
            .or_else(|| name_from_declarator(node))
            .or_else(|| {
                if spec.name_type_fallback {
                    node.child_by_field_name("type")
                } else {
                    None
                }
            })
            .or_else(|| {
                // Anonymous function/class (JS `export default function() {}`,
                // C anonymous struct) — first_identifier trong body là nhiễu, bỏ qua.
                if spec.func_kinds.contains(&node_kind) || spec.class_kinds.contains(&node_kind) {
                    None
                } else {
                    first_identifier(node)
                }
            })
    }?;
    let name = text(&name_node, ctx.src)?;
    if name.is_empty() {
        return None;
    }
    let id = ctx.next_id;
    ctx.next_id += 1;

    let (scope, scope_id) = if spec.param_kinds.contains(&node_kind) {
        (
            ScopeLevel::Parameter,
            ctx.scope_stack.last().map(|(i, _)| *i).unwrap_or(0),
        )
    } else if let Some(&(sid, is_class)) = ctx.scope_stack.last() {
        if is_class {
            (ScopeLevel::ObjectField, sid)
        } else {
            (ScopeLevel::Local, sid)
        }
    } else {
        (ScopeLevel::Global, 0)
    };

    // C/C++: `Foo();` / `~Foo();` (khai báo constructor/destructor không body)
    // parse thành `declaration`/`field_declaration` với function_declarator —
    // reclassify Function/Method thay vì Variable/Field (giống khai báo hàm thường).
    let kind = if (node_kind == "declaration" || node_kind == "field_declaration")
        && (from_error_ctor
            || node.child_by_field_name("declarator").map(|d| d.kind())
                == Some("function_declarator"))
    {
        if scope == ScopeLevel::ObjectField {
            SymbolKind::Method
        } else {
            SymbolKind::Function
        }
    } else {
        kind
    };

    // Function nằm trong class/impl (Rust `fn` trong impl, Python def trong class,
    // Go method...) — reclassify thành Method.
    let kind = if kind == SymbolKind::Function && scope == ScopeLevel::ObjectField {
        SymbolKind::Method
    } else {
        kind
    };

    let type_name = match kind {
        SymbolKind::Variable | SymbolKind::Constant | SymbolKind::Field | SymbolKind::Parameter => {
            node.child_by_field_name("type")
                .and_then(|t| text(&t, ctx.src))
        }
        SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum | SymbolKind::Module => {
            spec.class_type_name.and_then(|f| f(node, ctx.src))
        }
        _ => None,
    };

    let line = name_node.start_position().row as u32 + 1;
    let end_line = node
        .child_by_field_name(spec.body_field)
        .map(|b| b.end_position().row as u32 + 1)
        .unwrap_or_else(|| node.end_position().row as u32 + 1);
    let signature = extract_signature(node, ctx.src, spec.body_field);
    let annotations = extract_annotations(node, ctx.src, spec.annotation_kinds);

    ctx.symbols.push(Symbol {
        id,
        name,
        kind,
        scope,
        scope_id,
        type_ref: 0,
        type_name,
        file: ctx.file.to_string(),
        line,
        end_line,
        signature,
        doc: None,
        annotations,
        language: ctx.language.to_string(),
    });
    Some(id)
}

/// Resolve `type_ref` trong cùng file: base name của `type_name` khớp symbol
/// class-like nào thì trỏ tới id của nó.
fn resolve_type_refs(symbols: &mut [Symbol]) {
    let mut by_name: HashMap<String, u64> = HashMap::new();
    for s in symbols.iter() {
        if matches!(
            s.kind,
            SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum
        ) {
            by_name.entry(s.name.clone()).or_insert(s.id);
        }
    }
    for s in symbols.iter_mut() {
        if s.type_ref != 0 {
            continue;
        }
        let Some(tn) = s.type_name.clone() else {
            continue;
        };
        if let Some(&tid) = by_name.get(&base_type_name(&tn)) {
            s.type_ref = tid;
        }
    }
}

/// Rút base name từ type string: `Foo<T>` → `Foo`, `*Foo`/`&Foo` → `Foo`,
/// `pkg.Foo`/`ns::Foo` → `Foo`.
fn base_type_name(tn: &str) -> String {
    let s = match tn.find('<') {
        Some(idx) => &tn[..idx],
        None => tn,
    };
    let s = s.trim().trim_start_matches(['*', '&']);
    s.rsplit(['.', ':']).next().unwrap_or(s).trim().to_string()
}

fn extract_annotations(node: &Node, src: &[u8], kinds: &'static [&'static str]) -> Vec<Annotation> {
    if kinds.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for ch in named_children(node) {
        collect_annotation(&ch, src, kinds, &mut out);
    }
    out
}

fn collect_annotation(
    node: &Node,
    src: &[u8],
    kinds: &'static [&'static str],
    out: &mut Vec<Annotation>,
) {
    if kinds.contains(&node.kind()) {
        let name = node
            .child_by_field_name("name")
            .and_then(|n| text(&n, src))
            .or_else(|| first_identifier_text(node, src))
            .unwrap_or_default();
        let args = annotation_args(node, src);
        let line = node.start_position().row as u32 + 1;
        out.push(Annotation { name, args, line });
    }
    // Wrapper node: Java modifiers, TS decorators, C# attributes...
    if matches!(node.kind(), "modifiers" | "decorators" | "attributes") {
        for ch in named_children(node) {
            collect_annotation(&ch, src, kinds, out);
        }
    }
}

fn annotation_args(node: &Node, src: &[u8]) -> HashMap<String, String> {
    let mut args = HashMap::new();
    for ch in named_children(node) {
        if ch.kind() != "annotation_argument_list" {
            continue;
        }
        for (i, arg) in named_children(&ch).into_iter().enumerate() {
            if arg.kind() == "element_value_pair" {
                let key = arg
                    .child_by_field_name("key")
                    .and_then(|k| text(&k, src))
                    .unwrap_or_default();
                let value = arg
                    .child_by_field_name("value")
                    .and_then(|v| text(&v, src))
                    .unwrap_or_default();
                args.insert(key, value);
            } else {
                args.insert(i.to_string(), text(&arg, src).unwrap_or_default());
            }
        }
    }
    args
}

// ==================== Pass 2: chains ====================

fn collect_chains(
    root: &Node,
    src: &[u8],
    spec: &'static LangSpec,
    func_index: &HashMap<(String, u32), u64>,
    class_index: &HashMap<(String, u32), u64>,
    chains: &mut HashMap<u64, Vec<u64>>,
    calls: &mut Vec<CallRecord>,
) {
    if spec.func_kinds.contains(&root.kind()) {
        if let Some(id) = func_id_of(root, src, func_index) {
            let (chain, mut cs) = build_chain(root, src, spec, id);
            chains.insert(id, chain);
            calls.append(&mut cs);
        }
    } else if spec.class_kinds.contains(&root.kind()) {
        // Class không có chain (chỉ có edge function→class từ phía caller).
        // Build chain tối thiểu `[class_id]` để `flow`/`search_flow` không bị
        // "chain not found" — methods của class vẫn có chain riêng của chúng.
        if let Some(id) = class_id_of(root, src, class_index) {
            chains.entry(id).or_insert_with(|| vec![id]);
        }
    }
    for ch in named_children(root) {
        collect_chains(&ch, src, spec, func_index, class_index, chains, calls);
    }
}

fn func_id_of(node: &Node, src: &[u8], func_index: &HashMap<(String, u32), u64>) -> Option<u64> {
    let name_node = node
        .child_by_field_name("name")
        .or_else(|| name_from_declarator(node))
        .or_else(|| first_identifier(node))?;
    let name = text(&name_node, src)?;
    let line = name_node.start_position().row as u32 + 1;
    func_index.get(&(name, line)).copied()
}

fn class_id_of(node: &Node, src: &[u8], class_index: &HashMap<(String, u32), u64>) -> Option<u64> {
    let name_node = node
        .child_by_field_name("name")
        .or_else(|| first_identifier(node))?;
    let name = text(&name_node, src)?;
    let line = name_node.start_position().row as u32 + 1;
    class_index.get(&(name, line)).copied()
}

/// Build chain của một function: `[func_id, marker/call, ...]`.
pub fn build_chain(
    node: &Node,
    src: &[u8],
    spec: &'static LangSpec,
    func_id: u64,
) -> (Vec<u64>, Vec<CallRecord>) {
    let mut ctx = ChainCtx {
        src,
        spec,
        func_id,
        chain: vec![func_id],
        calls: Vec::new(),
    };
    if let Some(body) = node.child_by_field_name(spec.body_field) {
        walk_chain(&mut ctx, &body, 0, 0, None);
    } else {
        walk_chain(&mut ctx, node, 0, 0, None);
    }
    (ctx.chain, ctx.calls)
}

struct ChainCtx<'a> {
    src: &'a [u8],
    spec: &'static LangSpec,
    func_id: u64,
    chain: Vec<u64>,
    calls: Vec<CallRecord>,
}

fn walk_chain(
    ctx: &mut ChainCtx,
    node: &Node,
    depth: u32,
    in_loop: u32,
    condition: Option<String>,
) {
    if depth > 200 {
        return;
    }
    let k = node.kind();

    // 1. Call sites.
    if let Some(rule) = ctx.spec.calls.iter().find(|r| r.kind == k) {
        emit_call(ctx, node, rule, in_loop, condition);
        for ch in named_children(node) {
            walk_chain(ctx, &ch, depth + 1, in_loop, None);
        }
        return;
    }

    // 2. If / ternary.
    if ctx.spec.if_kinds.contains(&k) {
        chain_push(ctx, MARKER_IF_TRUE);
        let cond_node = node.child_by_field_name(ctx.spec.if_cond_field);
        let cond_text = cond_node
            .and_then(|c| text(&c, ctx.src))
            .unwrap_or_else(|| condition.clone().unwrap_or_default());
        let if_cond = if cond_text.is_empty() {
            condition.clone()
        } else {
            Some(cond_text)
        };
        // Calls TRONG condition (`if (a() && b(c()))`) được emit ngay sau IF_TRUE
        // — trước đây rớt khỏi chain (không tìm/search được).
        if let Some(cn) = cond_node {
            walk_chain(ctx, &cn, depth + 1, in_loop, if_cond.clone());
        }
        let cons = node
            .child_by_field_name(ctx.spec.if_cons_field)
            .or_else(|| first_blockish(node, ctx.spec));
        if let Some(cons) = cons {
            walk_chain(ctx, &cons, depth + 1, in_loop, if_cond.clone());
            let alt = node
                .child_by_field_name(ctx.spec.if_alt_field)
                .or_else(|| alternative_after_else(node, ctx.spec));
            if let Some(alt) = alt {
                if ctx.spec.elif_kinds.contains(&alt.kind()) {
                    walk_alternative(ctx, &alt, depth, in_loop, if_cond);
                } else {
                    chain_push(ctx, MARKER_IF_FALSE);
                    walk_alternative(ctx, &alt, depth, in_loop, negate_cond(if_cond));
                }
            }
        } else {
            // Không có consequence field — walk toàn node (degrade, vẫn bắt calls).
            walk_block(ctx, node, depth + 1, in_loop, condition);
        }
        chain_push(ctx, MARKER_BRANCH_END);
        return;
    }

    // 3. Loops.
    if ctx.spec.loop_kinds.contains(&k) {
        chain_push(ctx, MARKER_LOOP);
        // Condition của loop (while/for/do): emit calls trong condition + giữ
        // text làm metadata — trước đây loop mất cả calls lẫn text.
        let cond_node = loop_condition_node(node, ctx.spec);
        let loop_cond = cond_node
            .and_then(|c| text(&c, ctx.src))
            .filter(|t| !t.is_empty())
            .or_else(|| condition.clone());
        // do-while/repeat: condition chạy SAU body → emit sau.
        let is_do_while =
            k.contains("do") || k == "repeat_statement" || k == "repeat_while_statement";
        if !is_do_while {
            if let Some(cn) = cond_node {
                walk_chain(ctx, &cn, depth + 1, in_loop + 1, loop_cond.clone());
            }
        }
        if let Some(body) = node.child_by_field_name(ctx.spec.body_field) {
            walk_chain(ctx, &body, depth + 1, in_loop + 1, loop_cond.clone());
        } else {
            walk_block(ctx, node, depth + 1, in_loop + 1, loop_cond.clone());
        }
        if is_do_while {
            if let Some(cn) = cond_node {
                walk_chain(ctx, &cn, depth + 1, in_loop + 1, loop_cond.clone());
            }
        }
        chain_push(ctx, MARKER_LOOP_BACK);
        return;
    }

    // 4. Switch.
    if ctx.spec.switch_kinds.contains(&k) {
        // Discriminant (`switch (getType(x))`) — emit calls trước các case.
        if let Some(cn) = node.child_by_field_name(ctx.spec.if_cond_field) {
            walk_chain(ctx, &cn, depth + 1, in_loop, condition.clone());
        }
        for case in switch_cases(node, ctx.spec) {
            chain_push(ctx, MARKER_SWITCH_CASE);
            // String-literal case label (`case 'optimize_text':`) — dispatch key
            // không phải identifier call; emit call-name ảo để search_by_call
            // tìm được function chứa switch.
            emit_case_label_call(ctx, &case, in_loop, condition.clone());
            walk_block(ctx, &case, depth + 1, in_loop, condition.clone());
            chain_push(ctx, MARKER_SWITCH_END);
        }
        return;
    }

    // 5. Return.
    if ctx.spec.return_kinds.contains(&k) {
        chain_push(ctx, MARKER_RETURN);
        for ch in named_children(node) {
            walk_chain(ctx, &ch, depth + 1, in_loop, condition.clone());
        }
        return;
    }

    // Swift: return/break/continue/throw gộp trong `control_transfer_statement`
    // (keyword là child đầu tiên) — emit marker tương ứng rồi walk phần còn lại.
    if k == "control_transfer_statement" {
        if let Some(kw) = node.child(0).and_then(|c| text(&c, ctx.src)) {
            match kw.as_str() {
                "return" => chain_push(ctx, MARKER_RETURN),
                "break" => chain_push(ctx, MARKER_BREAK),
                "continue" => chain_push(ctx, MARKER_CONTINUE),
                "throw" => chain_push(ctx, MARKER_THROW),
                _ => {}
            }
        }
        for ch in named_children(node) {
            walk_chain(ctx, &ch, depth + 1, in_loop, condition.clone());
        }
        return;
    }

    // 6. Break / continue / throw.
    if ctx.spec.break_kinds.contains(&k) {
        chain_push(ctx, MARKER_BREAK);
        return;
    }
    if ctx.spec.continue_kinds.contains(&k) {
        chain_push(ctx, MARKER_CONTINUE);
        return;
    }
    if ctx.spec.throw_kinds.contains(&k) {
        chain_push(ctx, MARKER_THROW);
        for ch in named_children(node) {
            walk_chain(ctx, &ch, depth + 1, in_loop, condition.clone());
        }
        return;
    }

    // 7. Try.
    if ctx.spec.try_kinds.contains(&k) {
        if let Some(body) = node.child_by_field_name(ctx.spec.body_field) {
            walk_chain(ctx, &body, depth + 1, in_loop, condition.clone());
        }
        for ch in named_children(node) {
            if ctx.spec.except_kinds.contains(&ch.kind()) {
                chain_push(ctx, MARKER_IF_TRUE);
                walk_clause(ctx, &ch, depth + 1, in_loop, condition.clone());
                chain_push(ctx, MARKER_BRANCH_END);
            }
        }
        for ch in named_children(node) {
            if ctx.spec.try_else_kinds.contains(&ch.kind()) {
                walk_clause(ctx, &ch, depth + 1, in_loop, condition.clone());
            }
        }
        for ch in named_children(node) {
            if ctx.spec.finally_kinds.contains(&ch.kind()) {
                walk_clause(ctx, &ch, depth + 1, in_loop, condition.clone());
            }
        }
        return;
    }

    // 8. Default: recurse.
    for ch in named_children(node) {
        walk_chain(ctx, &ch, depth + 1, in_loop, condition.clone());
    }
}

/// Walk nhánh else/elif. elif có marker riêng (IF_TRUE + body + BRANCH_END).
fn walk_alternative(
    ctx: &mut ChainCtx,
    alt: &Node,
    depth: u32,
    in_loop: u32,
    condition: Option<String>,
) {
    if ctx.spec.elif_kinds.contains(&alt.kind()) {
        chain_push(ctx, MARKER_IF_TRUE);
        let cond_node = alt.child_by_field_name(ctx.spec.if_cond_field);
        let cond_text = cond_node
            .and_then(|c| text(&c, ctx.src))
            .unwrap_or_else(|| condition.clone().unwrap_or_default());
        let elif_cond = if cond_text.is_empty() {
            condition.clone()
        } else {
            Some(cond_text)
        };
        if let Some(cn) = cond_node {
            walk_chain(ctx, &cn, depth + 1, in_loop, elif_cond.clone());
        }
        if let Some(cons) = alt.child_by_field_name(ctx.spec.if_cons_field) {
            walk_chain(ctx, &cons, depth + 1, in_loop, elif_cond.clone());
        }
        if let Some(next) = alt.child_by_field_name(ctx.spec.if_alt_field) {
            walk_alternative(ctx, &next, depth, in_loop, negate_cond(elif_cond));
        }
        chain_push(ctx, MARKER_BRANCH_END);
    } else {
        walk_block(ctx, alt, depth, in_loop, condition);
    }
}

fn walk_block(
    ctx: &mut ChainCtx,
    node: &Node,
    depth: u32,
    in_loop: u32,
    condition: Option<String>,
) {
    for ch in named_children(node) {
        walk_chain(ctx, &ch, depth, in_loop, condition.clone());
    }
}

/// Walk một clause (except/else/finally) — body field nếu có, không thì toàn node.
fn walk_clause(
    ctx: &mut ChainCtx,
    node: &Node,
    depth: u32,
    in_loop: u32,
    condition: Option<String>,
) {
    if let Some(b) = node.child_by_field_name(ctx.spec.body_field) {
        walk_chain(ctx, &b, depth, in_loop, condition);
    } else {
        walk_block(ctx, node, depth, in_loop, condition);
    }
}

fn chain_push(ctx: &mut ChainCtx, marker: u64) {
    ctx.chain.push(marker);
}

fn negate_cond(cond: Option<String>) -> Option<String> {
    cond.map(|c| format!("!{c}"))
}

/// Tìm node condition của loop (while/for/do).
///
/// Đa số ngôn ngữ đặt `condition` field trực tiếp trên loop node (C/Java/JS
/// `while`/`for`). Go bọc init/cond/post trong `for_clause` — field `condition`
/// nằm trên child — và dạng `for cond {}` (while-equivalent) không có field,
/// expression trần là child. Bỏ qua child có field (`for x in xs` — Python/JS
/// `left`/`right` fielded, không phải condition).
fn loop_condition_node<'a>(node: &Node<'a>, spec: &'static LangSpec) -> Option<Node<'a>> {
    if let Some(cn) = node.child_by_field_name(spec.if_cond_field) {
        return Some(cn);
    }
    let body = node.child_by_field_name(spec.body_field);
    for i in 0..node.child_count() {
        let Some(ch) = node.child(i) else { continue };
        if !ch.is_named() || Some(ch) == body || node.field_name_for_child(i as u32).is_some() {
            continue;
        }
        // Go `for_clause` bọc init/cond/post — field nằm trên child.
        if let Some(cn) = ch.child_by_field_name(spec.if_cond_field) {
            return Some(cn);
        }
        // Go dạng `for cond {}`: child expression trần.
        if matches!(
            ch.kind(),
            "binary_expression"
                | "call_expression"
                | "parenthesized_expression"
                | "unary_expression"
        ) {
            return Some(ch);
        }
    }
    None
}

/// Block-like node đầu tiên trong children — consequence fallback khi thiếu
/// field (Swift `if` dùng node `statements` trần).
fn first_blockish<'a>(node: &Node<'a>, spec: &'static LangSpec) -> Option<Node<'a>> {
    named_children(node)
        .into_iter()
        .find(|c| spec.if_block_kinds.contains(&c.kind()))
}

/// Alternative fallback: blockish node đứng sau keyword `else` (Swift có node
/// `else` tên riêng; các ngôn ngữ khác dùng field alternative).
fn alternative_after_else<'a>(node: &Node<'a>, spec: &'static LangSpec) -> Option<Node<'a>> {
    if spec.if_block_kinds.is_empty() {
        return None;
    }
    let children = named_children(node);
    let else_pos = children.iter().position(|c| c.kind() == "else")?;
    children
        .iter()
        .skip(else_pos + 1)
        .find(|c| spec.if_block_kinds.contains(&c.kind()))
        .copied()
}

/// Gom case children của switch node (hỗ trợ wrapper như Java `switch_block`).
fn switch_cases<'a>(node: &Node<'a>, spec: &'static LangSpec) -> Vec<Node<'a>> {
    let is_case = |n: &Node| {
        spec.switch_case_kinds.contains(&n.kind()) || spec.switch_default_kinds.contains(&n.kind())
    };
    let mut out = Vec::new();
    for ch in named_children(node) {
        if is_case(&ch) {
            out.push(ch);
        } else if spec.switch_block_kinds.contains(&ch.kind()) {
            for cc in named_children(&ch) {
                if is_case(&cc) {
                    out.push(cc);
                }
            }
        }
    }
    out
}

/// Case label là string literal (`case 'optimize_text':`) — dispatch key theo
/// chuỗi, không phải call thật. Emit placeholder `0` + CallRecord với
/// `call_name = literal` (bỏ quote) để `search_by_call` index được. Không có
/// symbol tương ứng trong repo → không resolve được → giữ unresolved call.
fn emit_case_label_call(ctx: &mut ChainCtx, case: &Node, in_loop: u32, condition: Option<String>) {
    // Field `value` là expression của case (`case X:` → X). Fallback: named child
    // đầu tiên (một số grammar không đặt field).
    let value = case
        .child_by_field_name("value")
        .or_else(|| named_children(case).into_iter().next());
    let Some(value) = value else { return };
    if !is_string_literal_kind(value.kind()) {
        return;
    }
    let Some(lit) = text(&value, ctx.src) else {
        return;
    };
    let Some(name) = string_literal_value(&lit) else {
        return;
    };
    if name.is_empty() {
        return;
    }
    let position = ctx.chain.len();
    ctx.chain.push(0);
    let (effect, effect_desc) = classify_effect(&name);
    ctx.calls.push(CallRecord {
        caller_id: ctx.func_id,
        call_name: name,
        position,
        arg_exprs: Vec::new(),
        line: value.start_position().row as u32 + 1,
        condition,
        is_loop_body: in_loop > 0,
        effect,
        effect_desc: effect_desc.map(|s| s.to_string()),
        target_class: None,
        target_method: None,
    });
}

/// Node kind của một string literal — chấp nhận các tên theo từng grammar
/// (TS `string`, Java `string_literal`, Go `interpreted_string_literal`...).
fn is_string_literal_kind(kind: &str) -> bool {
    kind.contains("string")
        || matches!(
            kind,
            "template_string" | "template_literal" | "char_literal" | "quoted_string"
        )
}

/// Rút giá trị chuỗi từ source literal: `'opt'`/`"opt"`/`` `opt` `` → `opt`.
fn string_literal_value(lit: &str) -> Option<String> {
    let l = lit.trim();
    let b = l.as_bytes();
    if b.len() < 2 {
        return None;
    }
    let (open, close) = (b[0] as char, b[b.len() - 1] as char);
    let matched = matches!((open, close), ('\'', '\'') | ('"', '"') | ('`', '`'));
    matched.then(|| l[1..l.len() - 1].to_string())
}

/// Emit placeholder `0` + CallRecord cho một call site.
fn emit_call(
    ctx: &mut ChainCtx,
    node: &Node,
    rule: &'static CallRule,
    in_loop: u32,
    condition: Option<String>,
) {
    let callee = if rule.callee_field.is_empty() {
        named_children(node).into_iter().next()
    } else {
        node.child_by_field_name(rule.callee_field)
    };
    let Some(callee) = callee else { return };
    let name = if let Some(f) = rule.name_fn {
        f(node, ctx.src)
    } else {
        text(&callee, ctx.src)
    };
    let Some(name) = name else { return };
    if name.is_empty() {
        return;
    }
    let position = ctx.chain.len();
    ctx.chain.push(0);

    let mut arg_exprs = Vec::new();
    if let Some(args) = node.child_by_field_name(rule.arguments_field) {
        for ch in named_children(&args) {
            if let Some(t) = text(&ch, ctx.src) {
                arg_exprs.push(t);
            }
        }
    }
    let (effect, effect_desc) = classify_effect(&name);
    let (target_class, target_method) = rule
        .target_fn
        .map(|f| f(node, ctx.src))
        .unwrap_or((None, None));
    ctx.calls.push(CallRecord {
        caller_id: ctx.func_id,
        call_name: name,
        position,
        arg_exprs,
        line: node.start_position().row as u32 + 1,
        condition,
        is_loop_body: in_loop > 0,
        effect,
        effect_desc: effect_desc.map(|s| s.to_string()),
        target_class,
        target_method,
    });
}

// ==================== Helpers ====================

pub fn named_children<'a>(node: &Node<'a>) -> Vec<Node<'a>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

pub fn text<'a>(node: &Node<'a>, src: &'a [u8]) -> Option<String> {
    node.utf8_text(src).ok().map(|s| s.to_string())
}

/// Full text của callee expression — default cho mọi ngôn ngữ.
pub fn callee_full_text<'a>(node: &Node<'a>, src: &'a [u8]) -> Option<String> {
    text(node, src)
}

/// Tên dotted từ callee expression — gom identifier/field_identifier theo thứ tự.
pub fn dotted_call_name<'a>(node: &Node<'a>, src: &'a [u8]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut stack = vec![*node];
    while let Some(n) = stack.pop() {
        if matches!(
            n.kind(),
            "identifier" | "field_identifier" | "property_identifier" | "type_identifier"
        ) {
            if let Some(t) = text(&n, src) {
                parts.push(t);
            }
            continue;
        }
        let children = named_children(&n);
        for ch in children.into_iter().rev() {
            stack.push(ch);
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("."))
    }
}

fn name_from_declarator<'a>(n: &Node<'a>) -> Option<Node<'a>> {
    let declarator = n.child_by_field_name("declarator")?;
    declarator_name(&declarator)
}

/// C/C++ macro attribute trước qualified ctor → tree-sitter ERROR node; tên ctor
/// thật nằm trong function_declarator bên trong ERROR. DFS tìm declarator đó.
fn error_ctor_name<'a>(node: &Node<'a>) -> Option<Node<'a>> {
    let mut stack: Vec<Node<'a>> = Vec::new();
    for ch in named_children(node) {
        if ch.kind() == "ERROR" {
            stack.push(ch);
        }
    }
    while let Some(n) = stack.pop() {
        if n.kind() == "function_declarator" {
            return declarator_name(&n);
        }
        for c in named_children(&n) {
            stack.push(c);
        }
    }
    None
}

fn declarator_name<'a>(n: &Node<'a>) -> Option<Node<'a>> {
    match n.kind() {
        "identifier" | "field_identifier" | "destructor_name" | "operator_name" => Some(*n),
        "type_identifier" if is_conversion_declarator(n) => Some(*n),
        "init_declarator" | "declarator" | "variable_declarator" => n
            .child_by_field_name("name") // Java/JS/TS: variable_declarator có field `name`
            .or_else(|| n.child_by_field_name("declarator"))
            .and_then(|d| declarator_name(&d)),
        "function_declarator"
        | "pointer_declarator"
        | "reference_declarator"
        | "array_declarator"
        | "parenthesized_declarator"
        | "abstract_function_declarator"
        | "variadic_declarator" => declarator_child(n).and_then(|d| declarator_name(&d)),
        "qualified_identifier" => n.child_by_field_name("name"),
        _ => None,
    }
}

fn declarator_child<'a>(n: &Node<'a>) -> Option<Node<'a>> {
    if let Some(d) = n.child_by_field_name("declarator") {
        return Some(d);
    }
    named_children(n)
        .into_iter()
        .find(|c| is_declarator_kind(c.kind()))
}

fn is_declarator_kind(kind: &str) -> bool {
    matches!(
        kind,
        "function_declarator"
            | "pointer_declarator"
            | "reference_declarator"
            | "array_declarator"
            | "parenthesized_declarator"
            | "abstract_function_declarator"
            | "variadic_declarator"
            | "identifier"
            | "field_identifier"
            | "destructor_name"
            | "operator_name"
            | "qualified_identifier"
            | "operator_cast"
            | "init_declarator"
            | "declarator"
            | "variable_declarator"
    )
}

fn is_conversion_declarator(n: &Node) -> bool {
    n.parent()
        .map(|p| p.kind() == "operator_cast")
        .unwrap_or(false)
}

/// DFS tìm identifier đầu tiên trong subtree.
fn first_identifier<'a>(n: &Node<'a>) -> Option<Node<'a>> {
    let mut stack = vec![*n];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "identifier"
                | "type_identifier"
                | "field_identifier"
                | "property_identifier"
                | "simple_identifier"
                | "constant"
        ) {
            return Some(node);
        }
        let children = named_children(&node);
        for ch in children.into_iter().rev() {
            stack.push(ch);
        }
    }
    None
}

fn first_identifier_text(node: &Node, src: &[u8]) -> Option<String> {
    first_identifier(node).and_then(|n| text(&n, src))
}

fn find_function_declarator<'a>(n: &Node<'a>) -> Option<Node<'a>> {
    if n.kind() == "function_declarator" {
        return Some(*n);
    }
    for ch in named_children(n) {
        if let Some(found) = find_function_declarator(&ch) {
            return Some(found);
        }
    }
    None
}

/// Signature = text từ đầu declaration tới hết function declarator (hoặc đầu body).
fn extract_signature(node: &Node, src: &[u8], body_field: &str) -> Option<String> {
    let end = find_function_declarator(node)
        .map(|fd| fd.end_byte())
        .or_else(|| node.child_by_field_name(body_field).map(|b| b.start_byte()))
        .unwrap_or(node.end_byte());
    let start = node.start_byte();
    if end <= start {
        return None;
    }
    let text = std::str::from_utf8(&src[start..end]).ok()?;
    let sig = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if sig.is_empty() {
        None
    } else {
        Some(sig)
    }
}
