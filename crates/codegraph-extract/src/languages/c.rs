use crate::languages::common::{CallRule, LangSpec};
use codegraph_core::SymbolKind;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_c::LANGUAGE.into()
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "c",
    extensions: &["c"],
    ts_language,
    decls: &[
        ("function_definition", SymbolKind::Function),
        ("struct_specifier", SymbolKind::Class),
        ("union_specifier", SymbolKind::Class),
        ("enum_specifier", SymbolKind::Enum),
        ("type_definition", SymbolKind::Constant),
        ("declaration", SymbolKind::Variable),
        ("field_declaration", SymbolKind::Field),
        ("parameter_declaration", SymbolKind::Parameter),
    ],
    func_kinds: &["function_definition"],
    class_kinds: &["struct_specifier", "union_specifier"],
    param_kinds: &["parameter_declaration"],
    annotation_kinds: &[],
    name_type_fallback: false,
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
    loop_kinds: &["for_statement", "while_statement", "do_statement"],
    switch_kinds: &["switch_statement"],
    switch_block_kinds: &[],
    switch_case_kinds: &["case_statement"],
    switch_default_kinds: &["default_statement"],
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

crate::lang_parser!(CParser, SPEC);
