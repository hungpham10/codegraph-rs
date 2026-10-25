use crate::languages::common::{CallRule, HookSpec, LangSpec, MarkerSpec};
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
    calls: &[CallRule {
        kind: "call_expression",
        callee_field: "",
        arguments_field: "arguments",
        name_fn: None,
        target_fn: None,
    }],
    hooks: HookSpec::none(),
    markers: MarkerSpec {
        if_kinds: &["if_statement"],
        if_block_kinds: &["statements"],
        loop_kinds: &["for_statement", "while_statement", "repeat_while_statement"],
        switch_kinds: &["switch_statement"],
        switch_case_kinds: &["switch_entry"],
        return_kinds: &["return_statement"],
        break_kinds: &["break_statement"],
        continue_kinds: &["continue_statement"],
        throw_kinds: &["throw_statement"],
        ..MarkerSpec::dflt()
    },
    ..LangSpec::dflt()
};
crate::lang_parser!(SwiftParser, SPEC);
