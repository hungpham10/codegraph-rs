use crate::languages::common::{CallRule, LangSpec};
use codegraph_core::SymbolKind;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_swift::LANGUAGE.into()
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "swift",
    extensions: &["swift"],
    ts_language,
    decls: &[
        ("function_declaration", SymbolKind::Function),
        ("init_declaration", SymbolKind::Method),
        ("deinit_declaration", SymbolKind::Method),
        ("class_declaration", SymbolKind::Class),
        ("struct_declaration", SymbolKind::Class),
        ("enum_declaration", SymbolKind::Enum),
        ("protocol_declaration", SymbolKind::Interface),
        ("property_declaration", SymbolKind::Field),
        ("variable_declaration", SymbolKind::Variable),
        ("parameter", SymbolKind::Parameter),
    ],
    func_kinds: &[
        "function_declaration",
        "init_declaration",
        "deinit_declaration",
    ],
    class_kinds: &[
        "class_declaration",
        "struct_declaration",
        "enum_declaration",
        "protocol_declaration",
    ],
    param_kinds: &["parameter"],
    annotation_kinds: &["attribute"],
    name_type_fallback: false,

    link_impl_methods: false,
    calls: &[CallRule {
        // Swift call_expression không có callee field — dùng named child đầu tiên
        // làm callee (verify bằng dump_tree).
        kind: "call_expression",
        callee_field: "",
        arguments_field: "arguments",
        name_fn: None,
        target_fn: None,
    }],
    class_type_name: None,
    if_kinds: &["if_statement"],
    elif_kinds: &[],
    // Swift if/for không có consequence/body field — body là node `statements` trần.
    if_block_kinds: &["statements"],
    loop_kinds: &["for_statement", "while_statement", "repeat_while_statement"],
    switch_kinds: &["switch_statement"],
    switch_block_kinds: &[],
    switch_case_kinds: &["switch_entry"],
    switch_default_kinds: &[],
    return_kinds: &["return_statement"],
    break_kinds: &["break_statement"],
    continue_kinds: &["continue_statement"],
    throw_kinds: &["throw_statement"],
    try_kinds: &[],
    except_kinds: &[],
    try_else_kinds: &[],
    finally_kinds: &[],
    if_cond_field: "condition",
    if_cons_field: "consequence",
    if_alt_field: "alternative",
    body_field: "body",
};

crate::lang_parser!(SwiftParser, SPEC);
