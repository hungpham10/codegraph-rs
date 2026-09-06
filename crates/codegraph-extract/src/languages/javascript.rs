use crate::languages::common::{named_children, text, CallRule, LangSpec};
use codegraph_core::SymbolKind;
use tree_sitter::Node;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_javascript::LANGUAGE.into()
}

/// `class Foo extends Bar` — heritage làm type_name.
pub fn class_type_name(node: &Node, src: &[u8]) -> Option<String> {
    for ch in named_children(node) {
        if ch.kind() == "class_heritage" {
            for cc in named_children(&ch) {
                if cc.kind() == "extends_clause" {
                    return cc.child_by_field_name("name").and_then(|n| text(&n, src));
                }
            }
        }
    }
    None
}

/// Hàm anonymous gán qua ngữ cảnh — mượn tên từ nơi gán:
/// `var a = function(){}` / `const f = () => {}` (declarator name),
/// `obj.foo = function(){}` (left), `{ foo: function(){} }` (pair key).
pub fn anonymous_name_node<'a>(node: &Node<'a>) -> Option<Node<'a>> {
    let p = node.parent()?;
    let (value_field, name_field) = match p.kind() {
        "variable_declarator" => ("value", "name"),
        "assignment_expression" => ("right", "left"),
        "pair" | "property" => ("value", "key"),
        _ => return None,
    };
    let value = p.child_by_field_name(value_field)?;
    if value.id() != node.id() {
        return None;
    }
    p.child_by_field_name(name_field)
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "javascript",
    extensions: &["js", "jsx", "mjs", "cjs"],
    ts_language,
    decls: &[
        ("function_declaration", SymbolKind::Function),
        ("generator_function_declaration", SymbolKind::Function),
        ("function_expression", SymbolKind::Function),
        ("arrow_function", SymbolKind::Function),
        ("method_definition", SymbolKind::Method),
        ("class_declaration", SymbolKind::Class),
        ("class", SymbolKind::Class),
        ("variable_declarator", SymbolKind::Variable),
    ],
    func_kinds: &[
        "function_declaration",
        "generator_function_declaration",
        "function_expression",
        "arrow_function",
        "method_definition",
    ],
    class_kinds: &["class_declaration", "class"],
    param_kinds: &[],
    annotation_kinds: &[],
    name_type_fallback: false,

    link_impl_methods: false,
    anonymous_name_fn: Some(anonymous_name_node),
    value_func_kinds: &["function_expression", "arrow_function"],
    calls: &[
        CallRule {
            kind: "call_expression",
            callee_field: "function",
            arguments_field: "arguments",
            name_fn: None,
            target_fn: None,
        },
        CallRule {
            kind: "new_expression",
            callee_field: "constructor",
            arguments_field: "arguments",
            name_fn: None,
            target_fn: None,
        },
    ],
    class_type_name: Some(class_type_name),
    if_kinds: &["if_statement"],
    elif_kinds: &[],
    if_block_kinds: &[],
    loop_kinds: &[
        "for_statement",
        "for_in_statement",
        "for_of_statement",
        "while_statement",
        "do_statement",
    ],
    switch_kinds: &["switch_statement"],
    switch_block_kinds: &["switch_body"],
    switch_case_kinds: &["switch_case"],
    switch_default_kinds: &["switch_default"],
    return_kinds: &["return_statement"],
    break_kinds: &["break_statement"],
    continue_kinds: &["continue_statement"],
    throw_kinds: &["throw_statement"],
    try_kinds: &["try_statement"],
    except_kinds: &["catch_clause"],
    try_else_kinds: &[],
    finally_kinds: &["finally_clause"],
    if_cond_field: "condition",
    if_cons_field: "consequence",
    if_alt_field: "alternative",
    body_field: "body",
};

crate::lang_parser!(JavaScriptParser, SPEC);
