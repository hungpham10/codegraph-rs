use crate::languages::common::{text, CallRule, LangSpec};
use codegraph_core::SymbolKind;
use tree_sitter::Node;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_php::LANGUAGE_PHP.into()
}

/// `Foo::bar()` / `self::run()` — scope + "." + method.
fn scoped_call_name(node: &Node, src: &[u8]) -> Option<String> {
    let name = node.child_by_field_name("name").and_then(|n| text(&n, src))?;
    if let Some(scope) = node.child_by_field_name("scope") {
        if let Some(s) = text(&scope, src) {
            if !s.is_empty() {
                return Some(format!("{s}.{name}"));
            }
        }
    }
    Some(name)
}

/// `$obj->method()` — object + "." + method (bỏ `$` prefix của biến PHP).
fn member_call_name(node: &Node, src: &[u8]) -> Option<String> {
    let name = node.child_by_field_name("name").and_then(|n| text(&n, src))?;
    if let Some(obj) = node.child_by_field_name("object") {
        if let Some(o) = text(&obj, src) {
            let o = o.trim_start_matches('$');
            if !o.is_empty() {
                return Some(format!("{o}.{name}"));
            }
        }
    }
    Some(name)
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "php",
    extensions: &["php"],
    ts_language,
    decls: &[
        ("function_definition", SymbolKind::Function),
        ("method_declaration", SymbolKind::Method),
        ("class_declaration", SymbolKind::Class),
        ("interface_declaration", SymbolKind::Interface),
        ("enum_declaration", SymbolKind::Enum),
        ("trait_declaration", SymbolKind::Class),
        ("namespace_definition", SymbolKind::Module),
        ("property_declaration", SymbolKind::Field),
        ("variable_declaration", SymbolKind::Variable),
        ("const_declaration", SymbolKind::Constant),
        ("simple_parameter", SymbolKind::Parameter),
        ("property_promotion_parameter", SymbolKind::Parameter),
    ],
    func_kinds: &["function_definition", "method_declaration"],
    class_kinds: &[
        "class_declaration",
        "interface_declaration",
        "enum_declaration",
        "trait_declaration",
    ],
    param_kinds: &["simple_parameter", "property_promotion_parameter"],
    annotation_kinds: &["attribute"],
    name_type_fallback: false,
    calls: &[
        CallRule {
            kind: "function_call_expression",
            callee_field: "function",
            arguments_field: "arguments",
            name_fn: None,
            target_fn: None,
        },
        CallRule {
            kind: "member_call_expression",
            callee_field: "name",
            arguments_field: "arguments",
            name_fn: Some(member_call_name),
            target_fn: None,
        },
        CallRule {
            kind: "nullsafe_member_call_expression",
            callee_field: "name",
            arguments_field: "arguments",
            name_fn: Some(member_call_name),
            target_fn: None,
        },
        CallRule {
            kind: "scoped_call_expression",
            callee_field: "name",
            arguments_field: "arguments",
            name_fn: Some(scoped_call_name),
            target_fn: None,
        },
        CallRule {
            kind: "object_creation_expression",
            callee_field: "name",
            arguments_field: "arguments",
            name_fn: None,
            target_fn: None,
        },
    ],
    class_type_name: None,
    if_kinds: &["if_statement"],
    elif_kinds: &[],
    if_block_kinds: &[],
    loop_kinds: &["for_statement", "foreach_statement", "while_statement", "do_statement"],
    switch_kinds: &["switch_statement"],
    switch_block_kinds: &["switch_block"],
    switch_case_kinds: &["case_statement"],
    switch_default_kinds: &["default_statement"],
    return_kinds: &["return_statement"],
    break_kinds: &["break_statement"],
    continue_kinds: &["continue_statement"],
    throw_kinds: &["throw_statement"],
    try_kinds: &["try_statement"],
    except_kinds: &["catch_clause"],
    try_else_kinds: &[],
    finally_kinds: &["finally_clause"],
    if_cond_field: "condition",
    // PHP if_statement đặt nhánh then trong field `body` (không phải `consequence`).
    if_cons_field: "body",
    if_alt_field: "alternative",
    body_field: "body",
};

crate::lang_parser!(PhpParser, SPEC);
