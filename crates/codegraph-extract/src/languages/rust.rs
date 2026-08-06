use crate::languages::common::{CallRule, LangSpec};
use codegraph_core::SymbolKind;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_rust::LANGUAGE.into()
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "rust",
    extensions: &["rs"],
    ts_language,
    decls: &[
        ("function_item", SymbolKind::Function),
        ("struct_item", SymbolKind::Class),
        ("enum_item", SymbolKind::Enum),
        ("trait_item", SymbolKind::Interface),
        ("impl_item", SymbolKind::Class),
        ("mod_item", SymbolKind::Module),
        ("const_item", SymbolKind::Constant),
        ("static_item", SymbolKind::Variable),
        ("type_item", SymbolKind::Class),
    ],
    func_kinds: &["function_item"],
    class_kinds: &[
        "struct_item",
        "enum_item",
        "trait_item",
        "impl_item",
        "mod_item",
    ],
    param_kinds: &[],
    annotation_kinds: &[],
    // `impl Foo` không có name field — tên nằm ở field `type`.
    name_type_fallback: true,
    calls: &[CallRule {
        kind: "call_expression",
        callee_field: "function",
        arguments_field: "arguments",
        name_fn: None,
        target_fn: None,
    }],
    class_type_name: None,
    if_kinds: &["if_expression", "if_let_expression"],
    elif_kinds: &[],
    if_block_kinds: &[],
    loop_kinds: &[
        "loop_expression",
        "while_expression",
        "while_let_expression",
        "for_expression",
    ],
    switch_kinds: &["match_expression"],
    switch_block_kinds: &["match_block"],
    switch_case_kinds: &["match_arm"],
    switch_default_kinds: &[],
    return_kinds: &["return_expression"],
    break_kinds: &["break_expression"],
    continue_kinds: &["continue_expression"],
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

crate::lang_parser!(RustParser, SPEC);
