use crate::languages::common::{text, CallRule, LangSpec};
use codegraph_core::SymbolKind;
use tree_sitter::Node;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_c_sharp::LANGUAGE.into()
}

/// `new List<int>(...)` — tên class gốc (strip generic args để resolve được).
fn new_call_name(node: &Node, src: &[u8]) -> Option<String> {
    let tn = node.child_by_field_name("type").and_then(|t| text(&t, src))?;
    let base = tn.split('<').next().unwrap_or(&tn);
    Some(base.trim().to_string())
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "csharp",
    extensions: &["cs"],
    ts_language,
    decls: &[
        ("class_declaration", SymbolKind::Class),
        ("struct_declaration", SymbolKind::Class),
        ("interface_declaration", SymbolKind::Interface),
        ("enum_declaration", SymbolKind::Enum),
        ("record_declaration", SymbolKind::Class),
        ("namespace_declaration", SymbolKind::Module),
        ("method_declaration", SymbolKind::Method),
        ("constructor_declaration", SymbolKind::Method),
        ("property_declaration", SymbolKind::Field),
        ("variable_declaration", SymbolKind::Variable),
        ("parameter", SymbolKind::Parameter),
    ],
    func_kinds: &["method_declaration", "constructor_declaration"],
    class_kinds: &[
        "class_declaration",
        "struct_declaration",
        "interface_declaration",
        "enum_declaration",
        "record_declaration",
    ],
    param_kinds: &["parameter"],
    annotation_kinds: &["attribute"],
    name_type_fallback: false,
    calls: &[
        CallRule {
            kind: "invocation_expression",
            callee_field: "function",
            arguments_field: "arguments",
            name_fn: None,
            target_fn: None,
        },
        CallRule {
            kind: "object_creation_expression",
            callee_field: "type",
            arguments_field: "arguments",
            name_fn: Some(new_call_name),
            target_fn: None,
        },
    ],
    class_type_name: None,
    if_kinds: &["if_statement"],
    elif_kinds: &[],
    if_block_kinds: &[],
    loop_kinds: &["for_statement", "foreach_statement", "while_statement", "do_statement"],
    switch_kinds: &["switch_statement", "switch_expression"],
    switch_block_kinds: &["switch_body"],
    switch_case_kinds: &["switch_section", "switch_expression_arm"],
    switch_default_kinds: &[],
    return_kinds: &["return_statement"],
    break_kinds: &["break_statement"],
    continue_kinds: &["continue_statement"],
    throw_kinds: &["throw_statement"],
    try_kinds: &["try_statement"],
    except_kinds: &["catch_clause", "catch_declaration"],
    try_else_kinds: &[],
    finally_kinds: &["finally_clause"],
    if_cond_field: "condition",
    if_cons_field: "consequence",
    if_alt_field: "alternative",
    body_field: "body",
};

crate::lang_parser!(CSharpParser, SPEC);
