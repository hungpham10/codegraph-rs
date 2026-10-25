use crate::languages::common::{text, CallRule, HookSpec, LangSpec, MarkerSpec};
use codegraph_core::SymbolKind;
use tree_sitter::Node;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_cpp::LANGUAGE.into()
}

/// Base class của class/struct có inheritance (`base_class_clause`:
/// `class D : public B1, private B2` → "B1,B2"). Base đóng vai interface cho
/// dynamic dispatch: method `virtual` của base + override ở derived.
fn cpp_base_classes(node: &Node, src: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "class_specifier" | "struct_specifier") {
        return None;
    }
    let base = node
        .children(&mut node.walk())
        .find(|c| c.kind() == "base_class_clause")?;
    let mut names: Vec<String> = Vec::new();
    let mut stack = vec![base];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "type_identifier" | "qualified_identifier" => {
                if let Some(t) = text(&n, src) {
                    // Template: `Base<T>` → "Base".
                    let name = t.split('<').next().unwrap_or("").trim().to_string();
                    if !name.is_empty()
                        && !matches!(
                            name.as_str(),
                            "virtual" | "public" | "private" | "protected"
                        )
                    {
                        names.push(name);
                    }
                }
            }
            _ => {
                let mut cursor = n.walk();
                for ch in n.children(&mut cursor) {
                    stack.push(ch);
                }
            }
        }
    }
    names.dedup();
    (!names.is_empty()).then(|| names.join(","))
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "cpp",
    extensions: &["cpp", "cc", "cxx", "hpp", "hh", "hxx"],
    ts_language,
    decls: &[
        ("function_definition", SymbolKind::Function),
        ("class_specifier", SymbolKind::Class),
        ("struct_specifier", SymbolKind::Class),
        ("union_specifier", SymbolKind::Class),
        ("enum_specifier", SymbolKind::Enum),
        ("namespace_definition", SymbolKind::Module),
        ("type_definition", SymbolKind::Constant),
        ("declaration", SymbolKind::Variable),
        ("field_declaration", SymbolKind::Field),
        ("parameter_declaration", SymbolKind::Parameter),
    ],
    func_kinds: &["function_definition"],
    class_kinds: &["class_specifier", "struct_specifier", "union_specifier"],
    param_kinds: &["parameter_declaration"],
    calls: &[CallRule {
        kind: "call_expression",
        callee_field: "function",
        arguments_field: "arguments",
        name_fn: None,
        target_fn: None,
    }],
    hooks: HookSpec {
        impl_trait_fn: Some(cpp_base_classes),
        fn_ptr_assign_fn: Some(crate::languages::c::c_fn_ptr_assign),
        ..HookSpec::none()
    },
    markers: MarkerSpec {
        if_kinds: &["if_statement"],
        loop_kinds: &[
            "for_statement",
            "for_range_loop",
            "while_statement",
            "do_statement",
        ],
        switch_kinds: &["switch_statement"],
        switch_case_kinds: &["case_statement"],
        switch_default_kinds: &["default_statement"],
        return_kinds: &["return_statement"],
        break_kinds: &["break_statement"],
        continue_kinds: &["continue_statement"],
        throw_kinds: &["throw_statement"],
        try_kinds: &["try_statement"],
        except_kinds: &["catch_clause"],
        ..MarkerSpec::dflt()
    },
    ..LangSpec::dflt()
};
crate::lang_parser!(CppParser, SPEC);
