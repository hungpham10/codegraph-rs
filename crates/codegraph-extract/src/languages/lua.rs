use crate::languages::common::{named_children, text, CallRule, HookSpec, LangSpec, MarkerSpec};
use codegraph_core::SymbolKind;
use codegraph_graph::{FileDeps, ImportAlias};
use tree_sitter::Node;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_lua::LANGUAGE.into()
}

/// `f = function() end` / `local f = function() end` — function_definition
/// anonymous mượn tên từ variable_list của assignment_statement.
fn anonymous_name_node<'a>(node: &Node<'a>) -> Option<Node<'a>> {
    let el = node.parent()?;
    if el.kind() != "expression_list" {
        return None;
    }
    let stmt = el.parent()?;
    if stmt.kind() != "assignment_statement" {
        return None;
    }
    named_children(&stmt)
        .into_iter()
        .find(|c| c.kind() == "variable_list")
        .and_then(|vl| vl.child_by_field_name("name"))
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
    func_kinds: &[
        "function_declaration",
        "function_definition",
        "local_function",
    ],
    class_kinds: &[],
    param_container_kinds: &["parameters"],
    calls: &[CallRule {
        kind: "function_call",
        callee_field: "name",
        arguments_field: "arguments",
        name_fn: None,
        target_fn: None,
    }],
    hooks: HookSpec {
        anonymous_name_fn: Some(anonymous_name_node),
        value_func_kinds: &["function_definition"],
        file_deps_fn: Some(lua_require_deps),
        fn_ptr_assign_fn: Some(lua_fn_value_assign),
        ..HookSpec::none()
    },
    markers: MarkerSpec {
        if_kinds: &["if_statement"],
        loop_kinds: &["for_statement", "while_statement", "repeat_statement"],
        return_kinds: &["return_statement"],
        break_kinds: &["break_statement"],
        ..MarkerSpec::dflt()
    },
    ..LangSpec::dflt()
};
crate::lang_parser!(LuaParser, SPEC);

/// `local mod = require('mod')` / `mod = require('mod')` → FileDeps (alias =
/// biến gán, path = string literal) — pattern chuẩn của Lua module system.
fn lua_require_deps(root: &Node, src: &[u8]) -> Option<FileDeps> {
    let mut imports: Vec<ImportAlias> = Vec::new();
    let mut stack = vec![*root];
    while let Some(n) = stack.pop() {
        let is_assign = matches!(
            n.kind(),
            "assignment_statement" | "local_variable_declaration"
        );
        if is_assign {
            // variable_list (trái) + expression_list (phải, chứa require call).
            let mut left_names: Vec<Option<String>> = Vec::new();
            let mut right_calls: Vec<Option<String>> = Vec::new();
            let mut cursor = n.walk();
            for ch in n.children(&mut cursor) {
                match ch.kind() {
                    "variable_list" | "variable_declaration" => {
                        let mut c2 = ch.walk();
                        for g in ch.children(&mut c2) {
                            if matches!(g.kind(), "identifier" | "variable") {
                                left_names.push(first_name(&g).and_then(|nn| text(&nn, src)));
                            }
                        }
                    }
                    "expression_list" => {
                        let mut c2 = ch.walk();
                        for g in ch.children(&mut c2) {
                            if g.kind() == "function_call" {
                                let callee =
                                    g.child_by_field_name("name").and_then(|nn| text(&nn, src));
                                if callee.as_deref() == Some("require") {
                                    right_calls.push(lua_require_path(&g, src));
                                } else {
                                    right_calls.push(None);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            // Ghép theo vị trí: local mod = require('mod').
            for (i, path) in right_calls.into_iter().enumerate() {
                if let (Some(path), Some(alias)) = (path, left_names.get(i).cloned().flatten()) {
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

/// Tên (identifier/variable/dot) đầu tiên của node.
fn first_name<'a>(node: &Node<'a>) -> Option<Node<'a>> {
    if matches!(node.kind(), "identifier" | "variable" | "name") {
        return Some(*node);
    }
    let mut cursor = node.walk();
    let mut found = None;
    for ch in node.children(&mut cursor) {
        if let Some(f) = first_name(&ch) {
            found = Some(f);
            break;
        }
    }
    found
}

/// String literal đầu trong arguments của `require('...')`.
fn lua_require_path<'a>(call: &Node<'a>, src: &'a [u8]) -> Option<String> {
    let args = call.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let mut found = None;
    for ch in args.children(&mut cursor) {
        if ch.kind() == "string" {
            if let Some(t) = text(&ch, src) {
                found = Some(t.trim_matches(|c| c == '\'' || c == '"').to_string());
            }
            break;
        }
    }
    found.filter(|p| !p.is_empty())
}

/// Function-value: `handler = funcName` (identifier = identifier).
fn lua_fn_value_assign(node: &Node, src: &[u8]) -> Option<(String, String)> {
    if node.kind() != "assignment_statement" {
        return None;
    }
    let mut names: Vec<String> = Vec::new();
    let mut values: Vec<String> = Vec::new();
    let mut cursor = node.walk();
    for ch in node.children(&mut cursor) {
        match ch.kind() {
            "variable_list" => {
                let mut c2 = ch.walk();
                for g in ch.children(&mut c2) {
                    if let Some(nn) = first_name(&g) {
                        if let Some(t) = text(&nn, src) {
                            names.push(t);
                        }
                    }
                }
            }
            "expression_list" => {
                let mut c2 = ch.walk();
                for g in ch.children(&mut c2) {
                    if g.kind() == "identifier" {
                        if let Some(t) = text(&g, src) {
                            values.push(t);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if names.len() == 1 && values.len() == 1 {
        Some((names.remove(0), values.remove(0)))
    } else {
        None
    }
}
