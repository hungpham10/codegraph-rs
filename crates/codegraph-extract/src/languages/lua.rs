use crate::languages::common::{named_children, CallRule, LangSpec};
use codegraph_core::SymbolKind;
use tree_sitter::Node;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_lua::LANGUAGE.into()
}

/// `f = function() end` / `local f = function() end` — function_definition
/// anonymous mượn tên từ variable_list của assignment_statement.
fn anonymous_name_node<'a>(node: &Node<'a>) -> Option<Node<'a>> {
    let el = node.parent()?;
    if el.kind() != "expression_list" {
        return None;
    }
    let stmt = el.parent()?;
    if stmt.kind() != "assignment_statement" {
        return None;
    }
    named_children(&stmt)
        .into_iter()
        .find(|c| c.kind() == "variable_list")
        .and_then(|vl| vl.child_by_field_name("name"))
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "lua",
    extensions: &["lua"],
    ts_language,
    decls: &[
        ("function_declaration", SymbolKind::Function),
        ("function_definition", SymbolKind::Function),
        ("local_function", SymbolKind::Function),
        ("variable_declaration", SymbolKind::Variable),
        ("local_variable_declaration", SymbolKind::Variable),
    ],
    func_kinds: &[
        "function_declaration",
        "function_definition",
        "local_function",
    ],
    class_kinds: &[],
    param_kinds: &[],
    annotation_kinds: &[],
    name_type_fallback: false,

    link_impl_methods: false,
    anonymous_name_fn: Some(anonymous_name_node),
    value_func_kinds: &["function_definition"],
    calls: &[CallRule {
        kind: "function_call",
        callee_field: "name",
        arguments_field: "arguments",
        name_fn: None,
        target_fn: None,
    }],
    class_type_name: None,
    if_kinds: &["if_statement"],
    elif_kinds: &[],
    if_block_kinds: &[],
    loop_kinds: &["for_statement", "while_statement", "repeat_statement"],
    switch_kinds: &[],
    switch_block_kinds: &[],
    switch_case_kinds: &[],
    switch_default_kinds: &[],
    return_kinds: &["return_statement"],
    break_kinds: &["break_statement"],
    continue_kinds: &[],
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

crate::lang_parser!(LuaParser, SPEC);
