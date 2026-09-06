use crate::languages::common::{first_identifier, CallRule, LangSpec};
use codegraph_core::SymbolKind;
use tree_sitter::Node;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_go::LANGUAGE.into()
}

/// `f := func(){}` / `var h = func(){}` — func_literal mượn tên biến.
/// func_literal truyền thẳng (`go func(){ }()`) không được đặt tên.
fn anonymous_name_node<'a>(node: &Node<'a>) -> Option<Node<'a>> {
    let el = node.parent()?;
    if el.kind() != "expression_list" {
        return None;
    }
    let p = el.parent()?;
    match p.kind() {
        // `var h = func(){ }` — tên ở field `name` của var_spec.
        "var_spec" => p.child_by_field_name("name"),
        // `f := func(){ }` — tên là identifier đầu của expression_list left.
        "short_var_declaration" => {
            let left = p.child_by_field_name("left")?;
            first_identifier(&left)
        }
        _ => None,
    }
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "go",
    extensions: &["go"],
    ts_language,
    decls: &[
        ("function_declaration", SymbolKind::Function),
        ("method_declaration", SymbolKind::Method),
        ("type_spec", SymbolKind::Class),
        ("var_spec", SymbolKind::Variable),
        ("const_spec", SymbolKind::Constant),
        ("parameter_declaration", SymbolKind::Parameter),
        ("func_literal", SymbolKind::Function),
    ],
    func_kinds: &["function_declaration", "method_declaration", "func_literal"],
    class_kinds: &[],
    param_kinds: &["parameter_declaration"],
    annotation_kinds: &[],
    name_type_fallback: false,

    link_impl_methods: false,
    anonymous_name_fn: Some(anonymous_name_node),
    value_func_kinds: &["func_literal"],
    calls: &[CallRule {
        kind: "call_expression",
        callee_field: "function",
        arguments_field: "arguments",
        name_fn: None,
        target_fn: None,
    }],
    class_type_name: None,
    if_kinds: &["if_statement"],
    elif_kinds: &[],
    if_block_kinds: &[],
    loop_kinds: &["for_statement"],
    switch_kinds: &["expression_switch_statement", "type_switch_statement"],
    switch_block_kinds: &[],
    switch_case_kinds: &["expression_case", "type_case"],
    switch_default_kinds: &["default_case"],
    return_kinds: &["return_statement"],
    break_kinds: &["break_statement"],
    continue_kinds: &["continue_statement"],
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

crate::lang_parser!(GoParser, SPEC);
