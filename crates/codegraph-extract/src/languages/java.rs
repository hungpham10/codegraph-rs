use crate::languages::common::{named_children, text, CallRule, HookSpec, LangSpec, MarkerSpec};
use codegraph_core::SymbolKind;
use codegraph_graph::{FileDeps, ImportAlias};
use tree_sitter::Node as TsNode;
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
    hooks: HookSpec {
        impl_trait_fn: Some(java_supers),
        sig_key_fn: Some(java_signature_key),
        file_deps_fn: Some(java_file_deps),
        fn_ptr_assign_fn: Some(java_fn_value_assign),
        ..HookSpec::none()
    },
    markers: MarkerSpec {
        if_kinds: &["if_statement"],
        loop_kinds: &[
            "for_statement",
            "enhanced_for_statement",
            "while_statement",
            "do_statement",
        ],
        switch_kinds: &["switch_expression", "switch_statement"],
        switch_block_kinds: &["switch_block"],
        switch_case_kinds: &["switch_block_statement_group", "switch_rule"],
        return_kinds: &["return_statement"],
        break_kinds: &["break_statement"],
        continue_kinds: &["continue_statement"],
        throw_kinds: &["throw_statement"],
        try_kinds: &["try_statement", "try_with_resources_statement"],
        except_kinds: &["catch_clause"],
        finally_kinds: &["finally_clause"],
        ..MarkerSpec::dflt()
    },
    ..LangSpec::dflt()
};
crate::lang_parser!(JavaParser, SPEC);

/// Supers của class: superclass (extends) + interfaces (implements) — cả hai
/// vào `impl_trait` (split ',' ở `build_impl_index`). Superclass đứng trước →
/// inheritance fan-out qua machinery C++-base (method của base + override).
fn java_supers(node: &Node, src: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "class_declaration" | "record_declaration") {
        return None;
    }
    let mut names: Vec<String> = Vec::new();
    // extends: field `superclass` (type_identifier | scoped type_identifier).
    if let Some(sc) = node.child_by_field_name("superclass") {
        // Field text mang cả keyword: "extends BasePayment".
        if let Some(t) = text(&sc, src) {
            names.push(t.replacen("extends", "", 1).trim().to_string());
        }
    }
    // implements: field `interfaces` — text mang cả keyword "implements A, B".
    if let Some(tl) = node.child_by_field_name("interfaces") {
        if let Some(raw) = text(&tl, src) {
            for n in raw.replacen("implements", "", 1).split(',') {
                let n = n.trim();
                if !n.is_empty() {
                    names.push(n.to_string());
                }
            }
        }
    }
    names.dedup();
    (!names.is_empty()).then(|| names.join(","))
}

/// Signature key `(params)(return)` cho method/constructor — Go-style.
/// `pay(int amount) void` → "(int)(void)"; tên param bỏ.
fn java_signature_key(node: &TsNode, src: &[u8]) -> Option<String> {
    if !matches!(
        node.kind(),
        "method_declaration" | "constructor_declaration"
    ) {
        return None;
    }
    let mut pt: Vec<String> = Vec::new();
    if let Some(params) = node.child_by_field_name("parameters") {
        for ch in named_children(&params) {
            if let Some(t) = ch.child_by_field_name("type") {
                if let Some(txt) = text(&t, src) {
                    pt.push(txt.replace(' ', ""));
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

/// Imports của file → FileDeps:
/// - `import com.foo.Bar;` → alias "Bar", path "com.foo.Bar"
/// - `import com.foo.Bar as B;` (hiếm, preview) → alias "B"
/// - `import com.foo.*;` / `import static ...;` wildcard → bỏ qua.
///
/// Function-value Java: `Runnable r = this::run;` — variable_declarator với
/// value là method_reference (`this::run` / `Foo::bar`). Lambda body — skip.
fn java_fn_value_assign(node: &TsNode, src: &[u8]) -> Option<(String, String)> {
    if node.kind() != "variable_declarator" {
        return None;
    }
    let name = node.child_by_field_name("name")?;
    let value = node.child_by_field_name("value")?;
    if value.kind() != "method_reference" {
        return None;
    }
    // method_reference: `this::run` / `Foo::bar` — identifier CUỐI là tên hàm
    // (không phụ thuộc field name của grammar).
    let mut target = None;
    let mut c = value.walk();
    for ch in value.children(&mut c) {
        if ch.kind() == "identifier" {
            target = Some(ch);
        }
    }
    let target = text(&target?, src)?;
    let var = text(&name, src)?;
    Some((var, target))
}

fn java_file_deps(root: &TsNode, src: &[u8]) -> Option<FileDeps> {
    let mut imports: Vec<ImportAlias> = Vec::new();
    let mut stack = vec![*root];
    while let Some(n) = stack.pop() {
        if n.kind() == "import_declaration" {
            let raw = text(&n, src).unwrap_or_default();
            let stmt = raw.trim().trim_end_matches(';').trim();
            let stmt = stmt.strip_prefix("import ").unwrap_or(stmt).trim();
            if stmt.starts_with("static ") || stmt.ends_with(".*") {
                continue; // static import / wildcard — không có alias đơn
            }
            let (path, alias) = match stmt.split_once(" as ") {
                Some((p, a)) => (p.trim().to_string(), a.trim().to_string()),
                None => {
                    let seg = stmt.rsplit('.').next().unwrap_or("").to_string();
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
