use crate::languages::common::{named_children, text, CallRule, LangSpec};
use codegraph_core::SymbolKind;
use tree_sitter::Node;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_java::LANGUAGE.into()
}

/// `obj.method` — object field nếu có (giống reference: `obj.Content + "." + name`).
fn method_invocation_name(node: &Node, src: &[u8]) -> Option<String> {
    let name = node
        .child_by_field_name("name")
        .and_then(|n| text(&n, src))?;
    if let Some(obj) = node.child_by_field_name("object") {
        if let Some(obj_text) = text(&obj, src) {
            if !obj_text.is_empty() {
                return Some(format!("{obj_text}.{name}"));
            }
        }
    }
    Some(name)
}

/// `new Foo(...)` → tên class (field `type`).
fn new_call_name(node: &Node, src: &[u8]) -> Option<String> {
    if let Some(t) = node.child_by_field_name("type") {
        if let Some(s) = text(&t, src) {
            return Some(s);
        }
    }
    named_children(node)
        .into_iter()
        .find(|c| matches!(c.kind(), "type_identifier" | "scoped_type_identifier"))
        .and_then(|c| text(&c, src))
}

/// Structural target: call trên class literal (`Foo.class.bar()` trực tiếp,
/// hoặc DI container `getBean(Foo.class).bar()`).
fn class_literal_target(node: &Node, src: &[u8]) -> (Option<String>, Option<String>) {
    let name = node.child_by_field_name("name").and_then(|n| text(&n, src));
    let obj = node.child_by_field_name("object");
    let (Some(name), Some(obj)) = (name, obj) else {
        return (None, None);
    };
    let class_lit = match obj.kind() {
        "class_literal" => Some(obj),
        "method_invocation" | "object_creation_expression" => obj
            .child_by_field_name("arguments")
            .map(|args| named_children(&args))
            .into_iter()
            .flatten()
            .find(|c| c.kind() == "class_literal"),
        _ => None,
    };
    let Some(class_lit) = class_lit else {
        return (None, None);
    };
    // "com.foo.PolicyUtils.class" → "PolicyUtils" (bỏ ".class" + package prefix).
    let class_name = text(&class_lit, src)
        .map(|c| c.strip_suffix(".class").unwrap_or(&c).to_string())
        .and_then(|c| c.rsplit('.').next().map(|s| s.to_string()));
    (class_name, Some(name))
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "java",
    extensions: &["java"],
    ts_language,
    decls: &[
        ("class_declaration", SymbolKind::Class),
        ("interface_declaration", SymbolKind::Interface),
        ("enum_declaration", SymbolKind::Enum),
        ("record_declaration", SymbolKind::Class),
        ("method_declaration", SymbolKind::Method),
        ("constructor_declaration", SymbolKind::Method),
        ("field_declaration", SymbolKind::Field),
        ("local_variable_declaration", SymbolKind::Variable),
        ("formal_parameter", SymbolKind::Parameter),
    ],
    func_kinds: &["method_declaration", "constructor_declaration"],
    class_kinds: &[
        "class_declaration",
        "interface_declaration",
        "enum_declaration",
        "record_declaration",
    ],
    param_kinds: &["formal_parameter"],
    annotation_kinds: &["annotation", "marker_annotation"],
    name_type_fallback: false,

    link_impl_methods: false,
    calls: &[
        CallRule {
            kind: "method_invocation",
            callee_field: "name",
            arguments_field: "arguments",
            name_fn: Some(method_invocation_name),
            target_fn: Some(class_literal_target),
        },
        CallRule {
            kind: "object_creation_expression",
            callee_field: "type",
            arguments_field: "arguments",
            name_fn: Some(new_call_name),
            target_fn: None,
        },
    ],
    class_type_name: None,
    if_kinds: &["if_statement"],
    elif_kinds: &[],
    if_block_kinds: &[],
    loop_kinds: &[
        "for_statement",
        "enhanced_for_statement",
        "while_statement",
        "do_statement",
    ],
    switch_kinds: &["switch_expression", "switch_statement"],
    switch_block_kinds: &["switch_block"],
    switch_case_kinds: &["switch_block_statement_group", "switch_rule"],
    switch_default_kinds: &[],
    return_kinds: &["return_statement"],
    break_kinds: &["break_statement"],
    continue_kinds: &["continue_statement"],
    throw_kinds: &["throw_statement"],
    try_kinds: &["try_statement", "try_with_resources_statement"],
    except_kinds: &["catch_clause"],
    try_else_kinds: &[],
    finally_kinds: &["finally_clause"],
    if_cond_field: "condition",
    if_cons_field: "consequence",
    if_alt_field: "alternative",
    body_field: "body",
};

crate::lang_parser!(JavaParser, SPEC);
