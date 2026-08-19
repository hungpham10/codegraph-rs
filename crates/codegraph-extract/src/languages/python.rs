use crate::languages::common::{CallRule, LangSpec};
use codegraph_core::SymbolKind;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_python::LANGUAGE.into()
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "python",
    extensions: &["py", "pyi"],
    ts_language,
    decls: &[
        ("function_definition", SymbolKind::Function),
        ("class_definition", SymbolKind::Class),
    ],
    func_kinds: &["function_definition"],
    class_kinds: &["class_definition"],
    param_kinds: &[],
    annotation_kinds: &[],
    name_type_fallback: false,

    link_impl_methods: false,
    calls: &[CallRule {
        kind: "call",
        callee_field: "function",
        arguments_field: "arguments",
        name_fn: None,
        target_fn: None,
    }],
    class_type_name: None,
    if_kinds: &["if_statement"],
    elif_kinds: &["elif_clause"],
    if_block_kinds: &[],
    loop_kinds: &["for_statement", "while_statement"],
    switch_kinds: &["match_statement"],
    // Match cases nằm trong `block [body]` của match_statement.
    switch_block_kinds: &["block"],
    switch_case_kinds: &["case_clause"],
    switch_default_kinds: &[],
    return_kinds: &["return_statement"],
    break_kinds: &["break_statement"],
    continue_kinds: &["continue_statement"],
    throw_kinds: &["raise_statement"],
    try_kinds: &["try_statement"],
    except_kinds: &["except_clause"],
    try_else_kinds: &["else_clause"],
    finally_kinds: &["finally_clause"],
    if_cond_field: "condition",
    if_cons_field: "consequence",
    if_alt_field: "alternative",
    body_field: "body",
};

crate::lang_parser!(PythonParser, SPEC);
