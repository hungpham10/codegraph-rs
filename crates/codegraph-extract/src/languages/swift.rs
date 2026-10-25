use crate::languages::common::{text, CallRule, HookSpec, LangSpec, MarkerSpec};
use codegraph_core::SymbolKind;
use codegraph_graph::{FileDeps, ImportAlias};
use tree_sitter::Node;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_swift::LANGUAGE.into()
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "swift",
    extensions: &["swift"],
    ts_language,
    decls: &[
        ("function_declaration", SymbolKind::Function),
        ("init_declaration", SymbolKind::Method),
        ("deinit_declaration", SymbolKind::Method),
        ("class_declaration", SymbolKind::Class),
        ("struct_declaration", SymbolKind::Class),
        ("enum_declaration", SymbolKind::Enum),
        ("protocol_declaration", SymbolKind::Interface),
        ("property_declaration", SymbolKind::Field),
        ("variable_declaration", SymbolKind::Variable),
        ("parameter", SymbolKind::Parameter),
        // Method KHAI BÁO trong protocol (như Rust function_signature_item) —
        // scope vào protocol Interface → trigger fan-out.
        ("protocol_function_declaration", SymbolKind::Method),
    ],
    func_kinds: &[
        "function_declaration",
        "init_declaration",
        "deinit_declaration",
    ],
    class_kinds: &[
        "class_declaration",
        "struct_declaration",
        "enum_declaration",
        "protocol_declaration",
    ],
    param_kinds: &["parameter"],
    annotation_kinds: &["attribute"],
    calls: &[CallRule {
        kind: "call_expression",
        callee_field: "",
        arguments_field: "arguments",
        name_fn: None,
        target_fn: None,
    }],
    hooks: HookSpec {
        impl_trait_fn: Some(swift_supers),
        sig_key_fn: Some(swift_signature_key),
        file_deps_fn: Some(swift_import_deps),
        ..HookSpec::none()
    },
    markers: MarkerSpec {
        if_kinds: &["if_statement"],
        if_block_kinds: &["statements"],
        loop_kinds: &["for_statement", "while_statement", "repeat_while_statement"],
        switch_kinds: &["switch_statement"],
        switch_case_kinds: &["switch_entry"],
        return_kinds: &["return_statement"],
        break_kinds: &["break_statement"],
        continue_kinds: &["continue_statement"],
        throw_kinds: &["throw_statement"],
        await_kinds: &["await_expression"],
        ..MarkerSpec::dflt()
    },
    ..LangSpec::dflt()
};
crate::lang_parser!(SwiftParser, SPEC);

/// Supers: `class FileRepo: BaseRepo, Repo` — inheritance_specifier con trực
/// tiếp (field inherits_from → user_type). Protocol conformance đi cùng cơ
/// chế → fan-out cho protocol dispatch.
fn swift_supers(node: &Node, src: &[u8]) -> Option<String> {
    if !matches!(
        node.kind(),
        "class_declaration" | "struct_declaration" | "enum_declaration"
    ) {
        return None;
    }
    let mut names: Vec<String> = Vec::new();
    let mut cursor = node.walk();
    for ch in node.children(&mut cursor) {
        if ch.kind() != "inheritance_specifier" {
            continue;
        }
        // inherits_from → user_type → type_identifier đầu.
        if let Some(t) = text(&ch, src) {
            let base = t.split('<').next().unwrap_or("").trim().to_string();
            if !base.is_empty() {
                names.push(base);
            }
        }
    }
    names.dedup();
    (!names.is_empty()).then(|| names.join(","))
}

/// Signature key `(params)(return)`.
///
/// Swift grammar quái: parameter có HAI field cùng tên "name" (thứ nhất =
/// tên param simple_identifier, thứ hai = type user_type); return type cũng
/// là field "name" sau token `->`. Lấy field "name" CUỐI của từng node.
fn swift_signature_key(node: &Node, src: &[u8]) -> Option<String> {
    if !matches!(
        node.kind(),
        "function_declaration" | "protocol_function_declaration"
    ) {
        return None;
    }
    let mut pt: Vec<String> = Vec::new();
    let mut ret: Option<String> = None;
    let mut cursor = node.walk();
    let mut after_arrow = false;
    for ch in node.children(&mut cursor) {
        if ch.kind() == "->" {
            after_arrow = true;
            continue;
        }
        if ch.kind() == "parameter" {
            if let Some(t) = swift_last_name_field(&ch, src) {
                pt.push(t.replace(' ', ""));
            }
        } else if after_arrow && ch.kind() == "user_type" {
            ret = Some(text(&ch, src)?.replace(' ', ""));
        }
    }
    Some(format!(
        "({})({})",
        pt.join(","),
        ret.unwrap_or_else(|| "void".to_string())
    ))
}

/// Field "name" CUỐI của node (Swift dùng trùng tên field cho tên param + type).
fn swift_last_name_field<'a>(node: &Node<'a>, src: &'a [u8]) -> Option<String> {
    let mut cursor = node.walk();
    let mut found: Option<String> = None;
    for (i, ch) in node.children(&mut cursor).enumerate() {
        if node.field_name_for_child(i as u32) == Some("name") {
            if let Some(t) = text(&ch, src) {
                found = Some(t);
            }
        }
    }
    found
}

/// `import Foundation` → FileDeps (alias = segment cuối; Swift single-module
/// nên import chủ yếu external — stdlib ngoài index → giữ raw).
fn swift_import_deps(root: &Node, src: &[u8]) -> Option<FileDeps> {
    let mut imports: Vec<ImportAlias> = Vec::new();
    let mut stack = vec![*root];
    while let Some(n) = stack.pop() {
        if n.kind() == "import_declaration" {
            // identifier cuối của path.
            let mut cursor = n.walk();
            let mut last: Option<String> = None;
            for ch in n.children(&mut cursor) {
                if ch.kind() == "identifier" {
                    if let Some(t) = text(&ch, src) {
                        last = Some(t);
                    }
                }
            }
            if let Some(path) = last {
                let alias = path.rsplit('.').next().unwrap_or("").to_string();
                if !alias.is_empty() {
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
