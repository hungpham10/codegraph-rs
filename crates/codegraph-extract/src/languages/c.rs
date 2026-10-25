use crate::languages::common::{text, CallRule, HookSpec, LangSpec, MarkerSpec};
use codegraph_core::SymbolKind;
use tree_sitter::Node;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_c::LANGUAGE.into()
}

/// Gán function pointer cho indirect-call resolution:
/// - `zend_compile_file = compile_file;` (assignment_expression)
/// - `zend_op_array *(*f)(...) = compile_file;` (init_declarator)
///
/// Chỉ nhận LHS/var là identifier thuần và RHS/value là identifier thuần
/// (địa chỉ hàm; `&f` cũng qua identifier con).
pub(crate) fn c_fn_ptr_assign(node: &Node, src: &[u8]) -> Option<(String, String)> {
    match node.kind() {
        "assignment_expression" => {
            let left = node.child_by_field_name("left")?;
            let right = node.child_by_field_name("right")?;
            if left.kind() != "identifier" {
                return None;
            }
            let var = text(&left, src)?;
            let target = identifier_text(&right, src)?;
            Some((var, target))
        }
        "init_declarator" => {
            let declarator = node.child_by_field_name("declarator")?;
            let value = node.child_by_field_name("value")?;
            // `(*f)(...)` → pointer_declarator → function_declarator →
            // pointer_declarator → identifier. Lấy identifier *cuối* của
            // declarator (tên biến, không phải kiểu).
            let var = last_identifier(&declarator, src)?;
            let target = identifier_text(&value, src)?;
            Some((var, target))
        }
        _ => None,
    }
}

/// Text identifier khi node là identifier thuần. `&f` (address-of) cố tình bỏ —
/// không có type info để phân biệt fn-ptr var với biến thường (tránh false positive).
fn identifier_text(node: &Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => text(node, src),
        _ => None,
    }
}

/// Identifier cuối cùng trong declarator (tên biến — kiểu nằm trước).
fn last_identifier(node: &Node, src: &[u8]) -> Option<String> {
    if node.kind() == "identifier" {
        return text(node, src);
    }
    let mut cursor = node.walk();
    let mut found = None;
    for ch in node.children(&mut cursor) {
        if let Some(t) = last_identifier(&ch, src) {
            found = Some(t); // giữ con CUỐI cùng có identifier
        }
    }
    found
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
    calls: &[CallRule {
        kind: "call_expression",
        callee_field: "function",
        arguments_field: "arguments",
        name_fn: None,
        target_fn: None,
    }],
    hooks: HookSpec {
        fn_ptr_assign_fn: Some(c_fn_ptr_assign),
        ..HookSpec::none()
    },
    markers: MarkerSpec {
        if_kinds: &["if_statement"],
        loop_kinds: &["for_statement", "while_statement", "do_statement"],
        switch_kinds: &["switch_statement"],
        switch_case_kinds: &["case_statement"],
        switch_default_kinds: &["default_statement"],
        return_kinds: &["return_statement"],
        break_kinds: &["break_statement"],
        continue_kinds: &["continue_statement"],
        ..MarkerSpec::dflt()
    },
    ..LangSpec::dflt()
};
crate::lang_parser!(CParser, SPEC);
