use crate::languages::common::{text, CallRule, HookSpec, LangSpec, MarkerSpec};
use codegraph_core::SymbolKind;
use codegraph_graph::{FileDeps, ImportAlias};
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
/// `#include` graph: `"foo.h"` (quoted) / `<openssl/ssl.h>` (system) →
/// FileDeps — path nguyên văn, alias = tên file (không extension). Include
/// graph rebuild miễn phí qua machinery file_imports.
pub(crate) fn c_include_deps(root: &Node, src: &[u8]) -> Option<FileDeps> {
    let mut imports: Vec<ImportAlias> = Vec::new();
    let mut stack = vec![*root];
    while let Some(n) = stack.pop() {
        if n.kind() == "preproc_include" {
            if let Some(path_node) = n.child_by_field_name("path") {
                let raw = text(&path_node, src).unwrap_or_default();
                let path = raw
                    .trim_matches(|c| c == '"' || c == '<' || c == '>')
                    .to_string();
                let alias = path
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .split('.')
                    .next()
                    .unwrap_or("")
                    .to_string();
                if !path.is_empty() && !alias.is_empty() {
                    imports.push(ImportAlias { alias, path });
                }
            }
        }
        let mut cursor = n.walk();
        for ch in n.children(&mut cursor) {
            stack.push(ch);
        }
    }
    if imports.is_empty() {
        None
    } else {
        Some(FileDeps {
            package: None,
            imports,
        })
    }
}

pub(crate) fn c_fn_ptr_assign(node: &Node, src: &[u8]) -> Option<(String, String)> {
    match node.kind() {
        "assignment_expression" => {
            let left = node.child_by_field_name("left")?;
            let right = node.child_by_field_name("right")?;
            // LHS: identifier thuần, field (`obj.handler` — dotted key), deref
            // (`*handler` — strip *). Bracket `handlers[i]` skip.
            let var = match left.kind() {
                "identifier" => text(&left, src)?,
                "field_expression" => text(&left, src)?,
                "pointer_expression" => {
                    let inner = left.child_by_field_name("argument")?;
                    if inner.kind() != "identifier" {
                        return None;
                    }
                    text(&inner, src)?
                }
                _ => return None,
            };
            // RHS: identifier thuần, `&f` (address-of), `&ns::cb` (qualified).
            let target = match right.kind() {
                "identifier" => text(&right, src)?,
                "qualified_identifier" => text(&right, src)?,
                "pointer_expression" => identifier_text(&right, src)?,
                _ => return None,
            };
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

/// Text identifier khi node là identifier thuần hoặc `&f` (address-of —
/// RHS của fn-ptr assignment; LHS vẫn yêu cầu identifier thuần nên không
/// có false positive biến thường).
fn identifier_text(node: &Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => text(node, src),
        "pointer_expression" => {
            let inner = node.child_by_field_name("argument")?;
            (inner.kind() == "identifier").then(|| text(&inner, src))?
        }
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
        file_deps_fn: Some(c_include_deps),
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
