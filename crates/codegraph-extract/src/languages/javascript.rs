use crate::languages::common::{named_children, text, CallRule, HookSpec, LangSpec, MarkerSpec};
use codegraph_core::SymbolKind;
use codegraph_graph::{FileDeps, ImportAlias};
use tree_sitter::Node;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_javascript::LANGUAGE.into()
}

/// `class Foo extends Bar` — heritage làm type_name.
pub fn class_type_name(node: &Node, src: &[u8]) -> Option<String> {
    for ch in named_children(node) {
        if ch.kind() == "class_heritage" {
            for cc in named_children(&ch) {
                if cc.kind() == "extends_clause" {
                    return cc.child_by_field_name("name").and_then(|n| text(&n, src));
                }
            }
        }
    }
    None
}

/// Hàm anonymous gán qua ngữ cảnh — mượn tên từ nơi gán:
/// `var a = function(){}` / `const f = () => {}` (declarator name),
/// `obj.foo = function(){}` (left), `{ foo: function(){} }` (pair key).
pub fn anonymous_name_node<'a>(node: &Node<'a>) -> Option<Node<'a>> {
    let p = node.parent()?;
    let (value_field, name_field) = match p.kind() {
        "variable_declarator" => ("value", "name"),
        "assignment_expression" => ("right", "left"),
        "pair" | "property" => ("value", "key"),
        _ => return None,
    };
    let value = p.child_by_field_name(value_field)?;
    if value.id() != node.id() {
        return None;
    }
    p.child_by_field_name(name_field)
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "javascript",
    extensions: &["js", "jsx", "mjs", "cjs"],
    ts_language,
    decls: &[
        ("function_declaration", SymbolKind::Function),
        ("generator_function_declaration", SymbolKind::Function),
        ("function_expression", SymbolKind::Function),
        ("arrow_function", SymbolKind::Function),
        ("method_definition", SymbolKind::Method),
        ("class_declaration", SymbolKind::Class),
        ("class", SymbolKind::Class),
        ("variable_declarator", SymbolKind::Variable),
    ],
    func_kinds: &[
        "function_declaration",
        "generator_function_declaration",
        "function_expression",
        "arrow_function",
        "method_definition",
    ],
    class_kinds: &["class_declaration", "class"],
    param_container_kinds: &["formal_parameters"],
    field_from_self_attr: true,
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
    hooks: HookSpec {
        class_type_name: Some(class_type_name),
        anonymous_name_fn: Some(anonymous_name_node),
        value_func_kinds: &["function_expression", "arrow_function"],
        impl_trait_fn: Some(js_extends),
        file_deps_fn: Some(js_file_deps),
        fn_ptr_assign_fn: Some(js_fn_value_assign),
        ..HookSpec::none()
    },
    markers: MarkerSpec {
        if_kinds: &["if_statement"],
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
        finally_kinds: &["finally_clause"],
        await_kinds: &["await_expression"],
        ..MarkerSpec::dflt()
    },
    ..LangSpec::dflt()
};
crate::lang_parser!(JavaScriptParser, SPEC);

fn first_identifier_text(node: &Node, src: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    let mut found = None;
    for ch in node.children(&mut cursor) {
        if ch.kind() == "identifier" {
            found = Some(ch);
            break;
        }
    }
    found.and_then(|n| text(&n, src))
}

/// Base class của `class D extends B` → "B" — inheritance fan-out qua
/// machinery C++-base (impl_index key = base name).
///
/// JS thuần: class_heritage chứa identifier/member_expression TRỰC TIẾP
/// (không có node extends_clause như TS — nghĩa là class_type_name cũ cũng
/// không bao giờ match JS!). TS dùng class_type_name qua extends_clause.
pub fn js_extends(node: &Node, src: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "class_declaration" | "class") {
        return None;
    }
    let heritage = node
        .children(&mut node.walk())
        .find(|c| c.kind() == "class_heritage")?;
    let named = named_children(&heritage);
    // TS: heritage chứa extends_clause/implements_clause — dùng field name.
    if let Some(first) = named.first() {
        if matches!(first.kind(), "extends_clause" | "implements_clause") {
            return first
                .child_by_field_name("name")
                .and_then(|n| text(&n, src));
        }
    }
    // JS: extends token là unnamed → named đầu tiên là base.
    named.first().and_then(|n| text(n, src))
}

/// Imports của file → FileDeps (ESM):
/// - `import D from 'mod'` → alias "D", path "mod"
/// - `import { a, b as c } from 'mod'` → alias "a"/"c", path "mod.a"/"mod.b"
/// - `import * as ns from 'mod'` → alias "ns"
/// - `import 'mod'` / `export ... from` → bỏ.
pub fn js_file_deps(root: &Node, src: &[u8]) -> Option<FileDeps> {
    let mut imports: Vec<ImportAlias> = Vec::new();
    let mut stack = vec![*root];
    while let Some(n) = stack.pop() {
        if n.kind() == "import_statement" {
            let Some(source) = n.child_by_field_name("source") else {
                continue;
            };
            let raw = text(&source, src).unwrap_or_default();
            let path = raw.trim_matches(|c| c == '\'' || c == '"').to_string();
            if path.is_empty() {
                continue;
            }
            let mut push = |alias: String| {
                if !alias.is_empty() {
                    imports.push(ImportAlias {
                        alias,
                        path: path.clone(),
                    });
                }
            };
            for ch in named_children(&n) {
                match ch.kind() {
                    // `import D from 'mod'` — default trong import_clause.
                    "identifier" => {
                        if let Some(t) = text(&ch, src) {
                            push(t);
                        }
                    }
                    // `import * as ns from 'mod'`.
                    "namespace_import" => {
                        let t = first_identifier_text(&ch, src);
                        push(t.unwrap_or_default());
                    }
                    "import_clause" => {
                        let mut cc = ch.walk();
                        for g in ch.children(&mut cc) {
                            match g.kind() {
                                "identifier" => {
                                    if let Some(t) = text(&g, src) {
                                        push(t);
                                    }
                                }
                                "namespace_import" => {
                                    if let Some(t) = first_identifier_text(&g, src) {
                                        push(t);
                                    }
                                }
                                "named_imports" => {
                                    let mut gc = g.walk();
                                    for spec in g.children(&mut gc) {
                                        if spec.kind() == "import_specifier" {
                                            let raw = text(&spec, src).unwrap_or_default();
                                            let alias = match raw.split_once(" as ") {
                                                Some((_, a)) => a.trim().to_string(),
                                                None => raw.trim().to_string(),
                                            };
                                            push(alias);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
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

pub fn js_fn_value_assign(node: &Node, src: &[u8]) -> Option<(String, String)> {
    match node.kind() {
        "assignment_expression" => {
            let left = node.child_by_field_name("left")?;
            let right = node.child_by_field_name("right")?;
            if right.kind() != "identifier" {
                return None;
            }
            match left.kind() {
                // `handler = process`.
                "identifier" => Some((text(&left, src)?, text(&right, src)?)),
                // `obj.handler = fn` / `Foo.prototype.run = fn` — key dotted
                // (call `obj.handler()` khớp qua guard fn_ptr_index mở).
                "member_expression" => Some((text(&left, src)?, text(&right, src)?)),
                _ => None,
            }
        }
        "variable_declarator" => {
            let name = node.child_by_field_name("name")?;
            let value = node.child_by_field_name("value")?;
            if name.kind() != "identifier" || value.kind() != "identifier" {
                return None;
            }
            Some((text(&name, src)?, text(&value, src)?))
        }
        _ => None,
    }
}
