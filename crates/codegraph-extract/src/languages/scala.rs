use crate::languages::common::{CallRule, HookSpec, LangSpec, MarkerSpec};
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
    calls: &[CallRule {
        kind: "call_expression",
        callee_field: "function",
        arguments_field: "arguments",
        name_fn: None,
        target_fn: None,
    }],
    hooks: HookSpec::none(),
    markers: MarkerSpec {
        if_kinds: &["if_expression"],
        loop_kinds: &["for_expression", "while_expression", "do_while_expression"],
        switch_kinds: &["match_expression"],
        switch_block_kinds: &["case_block"],
        switch_case_kinds: &["case_clause"],
        return_kinds: &["return_expression"],
        throw_kinds: &["throw_expression"],
        try_kinds: &["try_expression"],
        except_kinds: &["catch_clause"],
        finally_kinds: &["finally_clause"],
        ..MarkerSpec::dflt()
    },
    ..LangSpec::dflt()
};
crate::lang_parser!(ScalaParser, SPEC);
