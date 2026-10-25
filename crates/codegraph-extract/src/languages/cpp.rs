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
        // Macro `#define CALL(x) x()` / `#define MAX 10` — Constant symbol,
        // signature = text định nghĩa (xem c_macro_sig). Invocation KHÔNG
        // resolve thành edge (macro không phải function) — usage tra qua
        // call-name index (references); hành vi semantic (exception-like…)
        // khai báo qua config `[[effect_rules]]`.
        ("preproc_function_def", SymbolKind::Constant),
        ("preproc_def", SymbolKind::Constant),
        ("declaration", SymbolKind::Variable),
        ("field_declaration", SymbolKind::Field),
        ("parameter_declaration", SymbolKind::Parameter),
        // Lambda `[](x){...}` — Function symbol, mượn tên biến qua
        // anonymous_name_fn (`auto f = [](){}` → f); chain riêng cho body.
        ("lambda_expression", SymbolKind::Function),
    ],
    func_kinds: &["function_definition", "lambda_expression"],
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
        sig_key_fn: Some(cpp_signature_key),
        file_deps_fn: Some(crate::languages::c::c_include_deps),
        typedef_sig_fn: Some(crate::languages::c::c_typedef_sig),
        anonymous_name_fn: Some(cpp_lambda_name),
        value_func_kinds: &["lambda_expression"],
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

/// `auto f = [](int x){ work(); };` — lambda mượn tên biến (pattern JS/PHP):
/// declaration → init_declarator có value là lambda_expression → field `name`.
/// Lambda truyền thẳng (`register([](){})`) không tên.
fn cpp_lambda_name<'a>(node: &Node<'a>) -> Option<Node<'a>> {
    let p = node.parent()?;
    if p.kind() != "init_declarator" {
        return None;
    }
    let value = p.child_by_field_name("value")?;
    if value.id() != node.id() {
        return None;
    }
    // C++ init_declarator: tên ở field `declarator` (JS variable_declarator
    // dùng field `name` — grammar khác nhau).
    p.child_by_field_name("declarator")
}

/// Signature key `(params)(return)` cho C++ — params từ function_declarator
/// trong declarator field; return = field `type`. Method inline trong class
/// (field_declaration với function_declarator) cũng tính.
fn cpp_signature_key(node: &Node, src: &[u8]) -> Option<String> {
    let is_fn = node.kind() == "function_definition";
    // field_declaration (method inline trong class): declarator field TRỰC
    // TIẾP là function_declarator (1 tầng, không 2).
    let is_method_decl = node.kind() == "field_declaration"
        && node
            .child_by_field_name("declarator")
            .is_some_and(|d| d.kind() == "function_declarator");
    if !is_fn && !is_method_decl {
        return None;
    }
    let declarator = node.child_by_field_name("declarator")?;
    let mut pt: Vec<String> = Vec::new();
    if let Some(params) = declarator.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for ch in params.children(&mut cursor) {
            if ch.kind() == "parameter" {
                if let Some(t) = ch.child_by_field_name("type") {
                    if let Some(txt) = text(&t, src) {
                        pt.push(txt.replace(' ', ""));
                    }
                }
            }
        }
    }
    let rt = node
        .child_by_field_name("type")
        .and_then(|t| text(&t, src))
        .map(|t| t.replace(' ', ""))
        .unwrap_or_else(|| "void".to_string());
    Some(format!("({})({})", pt.join(","), rt))
}
