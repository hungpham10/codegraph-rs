use crate::languages::common::{CallRule, LangSpec};
use codegraph_core::SymbolKind;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_lua::LANGUAGE.into()
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
    func_kinds: &["function_declaration", "function_definition", "local_function"],
    class_kinds: &[],
    param_kinds: &[],
    annotation_kinds: &[],
    name_type_fallback: false,
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
