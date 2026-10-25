use crate::languages::common::{text, CallRule, HookSpec, LangSpec, MarkerSpec};
use codegraph_core::SymbolKind;
use codegraph_graph::{FileDeps, ImportAlias};
use tree_sitter::Node;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_php::LANGUAGE_PHP.into()
}

/// `Foo::bar()` / `self::run()` — scope + "." + method.
fn scoped_call_name(node: &Node, src: &[u8]) -> Option<String> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| text(&n, src))?;
    if let Some(scope) = node.child_by_field_name("scope") {
        if let Some(s) = text(&scope, src) {
            if !s.is_empty() {
                return Some(format!("{s}.{name}"));
            }
        }
    }
    Some(name)
}

/// `$obj->method()` — object + "." + method (bỏ `$` prefix của biến PHP).
fn member_call_name(node: &Node, src: &[u8]) -> Option<String> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| text(&n, src))?;
    if let Some(obj) = node.child_by_field_name("object") {
        if let Some(o) = text(&obj, src) {
            let o = o.trim_start_matches('$');
            if !o.is_empty() {
                return Some(format!("{o}.{name}"));
            }
        }
    }
    Some(name)
}

/// `$f = function() {}` / `$h = fn() => ...` — hàm anonymous mượn tên biến
/// (node `name` bên trong variable_name, bỏ `$` prefix).
fn anonymous_name_node<'a>(node: &Node<'a>) -> Option<Node<'a>> {
    let p = node.parent()?;
    if p.kind() != "assignment_expression" {
        return None;
    }
    let right = p.child_by_field_name("right")?;
    if right.id() != node.id() {
        return None;
    }
    let left = p.child_by_field_name("left")?;
    let mut cursor = left.walk();
    let name_node = left.children(&mut cursor).find(|c| c.kind() == "name");
    name_node.or(Some(left))
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "php",
    extensions: &["php"],
    ts_language,
    decls: &[
        ("function_definition", SymbolKind::Function),
        ("method_declaration", SymbolKind::Method),
        ("class_declaration", SymbolKind::Class),
        ("interface_declaration", SymbolKind::Interface),
        ("enum_declaration", SymbolKind::Enum),
        ("trait_declaration", SymbolKind::Class),
        ("namespace_definition", SymbolKind::Module),
        ("property_declaration", SymbolKind::Field),
        ("variable_declaration", SymbolKind::Variable),
        ("const_declaration", SymbolKind::Constant),
        ("simple_parameter", SymbolKind::Parameter),
        ("property_promotion_parameter", SymbolKind::Parameter),
        ("anonymous_function", SymbolKind::Function),
        ("arrow_function", SymbolKind::Function),
    ],
    func_kinds: &[
        "function_definition",
        "method_declaration",
        "anonymous_function",
        "arrow_function",
    ],
    class_kinds: &[
        "class_declaration",
        "interface_declaration",
        "enum_declaration",
        "trait_declaration",
    ],
    param_kinds: &["simple_parameter", "property_promotion_parameter"],
    annotation_kinds: &["attribute"],
    calls: &[
        CallRule {
            kind: "function_call_expression",
            callee_field: "function",
            arguments_field: "arguments",
            name_fn: None,
            target_fn: None,
        },
        CallRule {
            kind: "member_call_expression",
            callee_field: "name",
            arguments_field: "arguments",
            name_fn: Some(member_call_name),
            target_fn: None,
        },
        CallRule {
            kind: "nullsafe_member_call_expression",
            callee_field: "name",
            arguments_field: "arguments",
            name_fn: Some(member_call_name),
            target_fn: None,
        },
        CallRule {
            kind: "scoped_call_expression",
            callee_field: "name",
            arguments_field: "arguments",
            name_fn: Some(scoped_call_name),
            target_fn: None,
        },
        CallRule {
            kind: "object_creation_expression",
            callee_field: "name",
            arguments_field: "arguments",
            name_fn: None,
            target_fn: None,
        },
    ],
    hooks: HookSpec {
        anonymous_name_fn: Some(anonymous_name_node),
        value_func_kinds: &["anonymous_function", "arrow_function"],
        impl_trait_fn: Some(php_supers),
        sig_key_fn: Some(php_signature_key),
        file_deps_fn: Some(php_use_deps),
        ..HookSpec::none()
    },
    markers: MarkerSpec {
        if_kinds: &["if_statement"],
        loop_kinds: &[
            "for_statement",
            "foreach_statement",
            "while_statement",
            "do_statement",
        ],
        switch_kinds: &["switch_statement"],
        switch_block_kinds: &["switch_block"],
        switch_case_kinds: &["case_statement"],
        switch_default_kinds: &["default_statement"],
        return_kinds: &["return_statement"],
        break_kinds: &["break_statement"],
        continue_kinds: &["continue_statement"],
        throw_kinds: &["throw_statement"],
        try_kinds: &["try_statement"],
        except_kinds: &["catch_clause"],
        finally_kinds: &["finally_clause"],
        ..MarkerSpec::dflt()
    },
    ..LangSpec::dflt()
};
crate::lang_parser!(PhpParser, SPEC);

/// Supers của class/trait/interface: `class C extends B implements I, J` —
/// base_clause text chứa cả names (extends + implements gộp). Không có
/// base_clause → None.
fn php_supers(node: &Node, src: &[u8]) -> Option<String> {
    if !matches!(
        node.kind(),
        "class_declaration" | "interface_declaration" | "enum_declaration"
    ) {
        return None;
    }
    let mut names: Vec<String> = Vec::new();
    // extends: field base_clause — text "extends B" (kw là unnamed).
    let clause = node
        .children(&mut node.walk())
        .find(|c| c.kind() == "base_clause");
    if let Some(clause) = clause {
        if let Some(raw) = text(&clause, src) {
            let items: Vec<String> = raw
                .replacen("extends", "", 1)
                .split(',')
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty() && n != "implements")
                .collect();
            names.extend(items);
        }
    }
    // implements: field class_interface_clause — text "implements I, J".
    let iface_clause = node
        .children(&mut node.walk())
        .find(|c| c.kind() == "class_interface_clause");
    if let Some(clause) = iface_clause {
        if let Some(raw) = text(&clause, src) {
            let items: Vec<String> = raw
                .replacen("implements", "", 1)
                .split(',')
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty() && n != "implements")
                .collect();
            names.extend(items);
        }
    }
    names.dedup();
    (!names.is_empty()).then(|| names.join(","))
}

/// Signature key `(params)(return)` — PHP có type hints đủ dùng.
fn php_signature_key(node: &Node, src: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "method_declaration" | "function_definition") {
        return None;
    }
    let mut pt: Vec<String> = Vec::new();
    if let Some(params) = node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for ch in params.children(&mut cursor) {
            if ch.kind() == "simple_parameter" {
                if let Some(t) = ch.child_by_field_name("type") {
                    if let Some(txt) = text(&t, src) {
                        pt.push(txt.replace(' ', ""));
                    }
                }
            }
        }
    }
    let rt = node
        .child_by_field_name("return_type")
        .and_then(|t| text(&t, src))
        .map(|t| t.replace(' ', ""))
        .unwrap_or_else(|| "void".to_string());
    Some(format!("({})({})", pt.join(","), rt))
}

/// `use` statements của file → FileDeps:
/// - `use App\Models\User;` → alias "User", path "App\Models\User"
/// - `use App\Models\User as U;` → alias "U"
/// - `use function x;` / `use const X;` → bỏ (function/const import hiếm).
fn php_use_deps(root: &Node, src: &[u8]) -> Option<FileDeps> {
    let mut imports: Vec<ImportAlias> = Vec::new();
    let mut stack = vec![*root];
    while let Some(n) = stack.pop() {
        if n.kind() == "namespace_use_declaration" {
            let raw = text(&n, src).unwrap_or_default();
            let stmt = raw.trim().trim_end_matches(';').trim();
            let stmt = stmt.strip_prefix("use ").unwrap_or(stmt).trim();
            if stmt.starts_with("function ") || stmt.starts_with("const ") {
                continue;
            }
            let (path, alias) = match stmt.split_once(" as ") {
                Some((p, a)) => (p.trim().to_string(), a.trim().to_string()),
                None => {
                    let seg = stmt.rsplit('\\').next().unwrap_or("").to_string();
                    (stmt.to_string(), seg)
                }
            };
            if !alias.is_empty() && !path.is_empty() {
                imports.push(ImportAlias { alias, path });
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
