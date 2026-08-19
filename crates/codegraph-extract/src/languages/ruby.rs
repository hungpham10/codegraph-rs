use crate::languages::common::{text, CallRule, LangSpec};
use codegraph_core::SymbolKind;
use tree_sitter::Node;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_ruby::LANGUAGE.into()
}

/// `receiver.method` nếu có receiver, không thì `method`.
fn call_name(node: &Node, src: &[u8]) -> Option<String> {
    let method = node
        .child_by_field_name("method")
        .and_then(|m| text(&m, src))?;
    if let Some(receiver) = node.child_by_field_name("receiver") {
        if let Some(r) = text(&receiver, src) {
            if !r.is_empty() {
                return Some(format!("{r}.{method}"));
            }
        }
    }
    Some(method)
}

/// Class Ruby `class Foo < Bar` — superclass làm type_name.
fn class_type_name(node: &Node, src: &[u8]) -> Option<String> {
    node.child_by_field_name("superclass")
        .and_then(|s| text(&s, src))
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "ruby",
    extensions: &["rb"],
    ts_language,
    decls: &[
        ("method", SymbolKind::Method),
        ("singleton_method", SymbolKind::Method),
        ("class", SymbolKind::Class),
        ("module", SymbolKind::Module),
    ],
    func_kinds: &["method", "singleton_method"],
    class_kinds: &["class", "module"],
    param_kinds: &[],
    annotation_kinds: &[],
    name_type_fallback: false,

    link_impl_methods: false,
    calls: &[
        CallRule {
            kind: "call",
            callee_field: "method",
            arguments_field: "arguments",
            name_fn: Some(call_name),
            target_fn: None,
        },
        CallRule {
            kind: "command_call",
            callee_field: "method",
            arguments_field: "arguments",
            name_fn: Some(call_name),
            target_fn: None,
        },
    ],
    class_type_name: Some(class_type_name),
    if_kinds: &["if"],
    elif_kinds: &["elsif"],
    if_block_kinds: &[],
    loop_kinds: &["while", "until", "for"],
    switch_kinds: &["case"],
    switch_block_kinds: &[],
    switch_case_kinds: &["when"],
    switch_default_kinds: &["else"],
    return_kinds: &["return"],
    break_kinds: &["break"],
    continue_kinds: &["next"],
    throw_kinds: &[],
    try_kinds: &[],
    except_kinds: &[],
    try_else_kinds: &[],
    finally_kinds: &[],
    if_cond_field: "condition",
    if_cons_field: "consequence",
    if_alt_field: "alternative",
    body_field: "body",
};

crate::lang_parser!(RubyParser, SPEC);
