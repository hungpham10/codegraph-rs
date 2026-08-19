use crate::languages::common::{CallRule, LangSpec};
use codegraph_core::SymbolKind;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_scala::LANGUAGE.into()
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "scala",
    extensions: &["scala", "sc"],
    ts_language,
    decls: &[
        ("function_definition", SymbolKind::Function),
        ("function_declaration", SymbolKind::Function),
        ("class_definition", SymbolKind::Class),
        ("object_definition", SymbolKind::Module),
        ("trait_definition", SymbolKind::Class),
        ("enum_definition", SymbolKind::Enum),
        ("val_definition", SymbolKind::Constant),
        ("var_definition", SymbolKind::Variable),
        ("parameter", SymbolKind::Parameter),
    ],
    func_kinds: &["function_definition", "function_declaration"],
    class_kinds: &[
        "class_definition",
        "trait_definition",
        "object_definition",
        "enum_definition",
    ],
    param_kinds: &["parameter"],
    annotation_kinds: &[],
    name_type_fallback: false,

    link_impl_methods: false,
    calls: &[CallRule {
        kind: "call_expression",
        callee_field: "function",
        arguments_field: "arguments",
        name_fn: None,
        target_fn: None,
    }],
    class_type_name: None,
    if_kinds: &["if_expression"],
    elif_kinds: &[],
    if_block_kinds: &[],
    loop_kinds: &["for_expression", "while_expression", "do_while_expression"],
    switch_kinds: &["match_expression"],
    switch_block_kinds: &["case_block"],
    switch_case_kinds: &["case_clause"],
    switch_default_kinds: &[],
    return_kinds: &["return_expression"],
    break_kinds: &[],
    continue_kinds: &[],
    throw_kinds: &["throw_expression"],
    try_kinds: &["try_expression"],
    except_kinds: &["catch_clause"],
    try_else_kinds: &[],
    finally_kinds: &["finally_clause"],
    if_cond_field: "condition",
    if_cons_field: "consequence",
    if_alt_field: "alternative",
    body_field: "body",
};

crate::lang_parser!(ScalaParser, SPEC);
