use crate::languages::common::{named_children, text, CallRule, LangSpec};
use codegraph_core::SymbolKind;
use tree_sitter::Node;

fn typescript_ts_language() -> tree_sitter::Language {
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
}

fn tsx_ts_language() -> tree_sitter::Language {
    tree_sitter_typescript::LANGUAGE_TSX.into()
}

/// Heritage: `class Foo extends Bar` / `interface Foo extends Bar, Baz` /
/// `implements A, B` — type đầu tiên làm type_name.
pub fn class_type_name(node: &Node, src: &[u8]) -> Option<String> {
    for ch in named_children(node) {
        match ch.kind() {
            "class_heritage" => {
                for cc in named_children(&ch) {
                    if cc.kind() == "extends_clause" {
                        return cc.child_by_field_name("name").and_then(|n| text(&n, src));
                    }
                }
            }
            "extends_clause" | "implements_clause" => {
                if let Some(name) = ch.child_by_field_name("name") {
                    return text(&name, src);
                }
            }
            _ => {}
        }
    }
    None
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "typescript",
    extensions: &["ts", "mts", "cts"],
    ts_language: typescript_ts_language,
    decls: &[
        ("function_declaration", SymbolKind::Function),
        ("generator_function_declaration", SymbolKind::Function),
        ("function_expression", SymbolKind::Function),
        ("arrow_function", SymbolKind::Function),
        ("method_definition", SymbolKind::Method),
        ("method_signature", SymbolKind::Method),
        ("class_declaration", SymbolKind::Class),
        ("class", SymbolKind::Class),
        ("interface_declaration", SymbolKind::Interface),
        ("enum_declaration", SymbolKind::Enum),
        ("type_alias_declaration", SymbolKind::Class),
        ("internal_module", SymbolKind::Module),
        ("variable_declarator", SymbolKind::Variable),
    ],
    func_kinds: &[
        "function_declaration",
        "generator_function_declaration",
        "function_expression",
        "arrow_function",
        "method_definition",
        "method_signature",
    ],
    class_kinds: &[
        "class_declaration",
        "class",
        "interface_declaration",
        "enum_declaration",
        "internal_module",
    ],
    param_kinds: &[],
    annotation_kinds: &[],
    name_type_fallback: false,
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

crate::lang_parser!(TypeScriptParser, SPEC);
crate::lang_parser!(TsxParser, SPEC, "tsx", &["tsx"], tsx_ts_language);
