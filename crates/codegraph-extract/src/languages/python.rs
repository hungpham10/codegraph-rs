use crate::languages::common::{text, CallRule, HookSpec, LangSpec, MarkerSpec};
use codegraph_core::SymbolKind;
use codegraph_graph::{FileDeps, ImportAlias};
use tree_sitter::Node;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_python::LANGUAGE.into()
}

/// `f = lambda: ...` — lambda mượn tên biến ở vế trái assignment.
/// Lambda truyền thẳng (vd `map(lambda: 1, ...)`) không được đặt tên.
fn anonymous_name_node<'a>(node: &Node<'a>) -> Option<Node<'a>> {
    let p = node.parent()?;
    if p.kind() != "assignment" {
        return None;
    }
    let right = p.child_by_field_name("right")?;
    if right.id() != node.id() {
        return None;
    }
    p.child_by_field_name("left")
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "python",
    extensions: &["py", "pyi"],
    ts_language,
    decls: &[
        ("function_definition", SymbolKind::Function),
        ("class_definition", SymbolKind::Class),
        ("lambda", SymbolKind::Function),
    ],
    func_kinds: &["function_definition", "lambda"],
    class_kinds: &["class_definition"],
    param_container_kinds: &["parameters"],
    field_from_self_attr: true,
    calls: &[CallRule {
        kind: "call",
        callee_field: "function",
        arguments_field: "arguments",
        name_fn: None,
        target_fn: None,
    }],
    hooks: HookSpec {
        anonymous_name_fn: Some(anonymous_name_node),
        impl_trait_fn: Some(py_superclasses),
        file_deps_fn: Some(py_file_deps),
        ..HookSpec::none()
    },
    markers: MarkerSpec {
        if_kinds: &["if_statement"],
        elif_kinds: &["elif_clause"],
        loop_kinds: &["for_statement", "while_statement"],
        switch_kinds: &["match_statement"],
        switch_block_kinds: &["block"],
        switch_case_kinds: &["case_clause"],
        return_kinds: &["return_statement"],
        break_kinds: &["break_statement"],
        continue_kinds: &["continue_statement"],
        throw_kinds: &["raise_statement"],
        try_kinds: &["try_statement"],
        except_kinds: &["except_clause"],
        try_else_kinds: &["else_clause"],
        finally_kinds: &["finally_clause"],
        ..MarkerSpec::dflt()
    },
    ..LangSpec::dflt()
};
crate::lang_parser!(PythonParser, SPEC);

/// Base classes của class (`class File(Writer, close)` → "Writer,close") —
/// đóng vai impl_trait: fan-out dynamic dispatch qua machinery impl_index
/// (giống C++ base class: method của base + override ở subclass).
fn py_superclasses(node: &Node, src: &[u8]) -> Option<String> {
    if node.kind() != "class_definition" {
        return None;
    }
    let supers = node.child_by_field_name("superclasses")?;
    let mut names: Vec<String> = Vec::new();
    let mut stack = vec![supers];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "identifier" => {
                if let Some(t) = text(&n, src) {
                    names.push(t);
                }
            }
            "attribute" => {
                if let Some(t) = text(&n, src) {
                    names.push(t); // module.Class — giữ nguyên, split ',' sau
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

/// Imports của file → FileDeps:
/// - `import numpy as np` → alias "np", path "numpy"
/// - `import os.path` → alias "path", path "os.path"
/// - `from orders.models import Order, get_order as fetch`
///   → alias "Order"/"fetch", path "orders.models.Order"/"orders.models.get_order"
fn py_file_deps(root: &Node, src: &[u8]) -> Option<FileDeps> {
    let mut imports: Vec<ImportAlias> = Vec::new();
    let mut stack = vec![*root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "import_statement" => {
                let mut cursor = n.walk();
                for ch in n.children(&mut cursor) {
                    match ch.kind() {
                        "dotted_name" => {
                            let path = text(&ch, src).unwrap_or_default();
                            let alias = path.rsplit('.').next().unwrap_or("").to_string();
                            if !alias.is_empty() {
                                imports.push(ImportAlias { alias, path });
                            }
                        }
                        "aliased_import" => {
                            let raw = text(&ch, src).unwrap_or_default();
                            if let Some((p, a)) = raw.split_once(" as ") {
                                imports.push(ImportAlias {
                                    alias: a.trim().to_string(),
                                    path: p.trim().to_string(),
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
            "import_from_statement" => {
                let module_node = n.child_by_field_name("module_name");
                let module = module_node
                    .as_ref()
                    .and_then(|m| text(m, src))
                    .unwrap_or_default();
                let mut cursor = n.walk();
                for ch in n.children(&mut cursor) {
                    // Bỏ node module_name — chỉ xử lý các name được import.
                    if module_node.is_some_and(|m| m.id() == ch.id()) {
                        continue;
                    }
                    match ch.kind() {
                        "dotted_name" => {
                            let name = text(&ch, src).unwrap_or_default();
                            imports.push(ImportAlias {
                                alias: name.clone(),
                                path: format!("{module}.{name}"),
                            });
                        }
                        "aliased_import" => {
                            let raw = text(&ch, src).unwrap_or_default();
                            if let Some((p, a)) = raw.split_once(" as ") {
                                imports.push(ImportAlias {
                                    alias: a.trim().to_string(),
                                    path: format!("{}.{}", module, p.trim()),
                                });
                            }
                        }
                        _ => {}
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
    if imports.is_empty() {
        None
    } else {
        Some(FileDeps {
            package: None,
            imports,
        })
    }
}
