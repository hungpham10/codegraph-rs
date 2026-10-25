use crate::languages::common::{named_children, text, CallRule, HookSpec, LangSpec, MarkerSpec};
use codegraph_core::SymbolKind;
use codegraph_graph::{FileDeps, ImportAlias};
use tree_sitter::Node;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_scala::LANGUAGE.into()
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "scala",
    extensions: &["scala", "sc"],
    ts_language,
    decls: &[
        ("function_definition", SymbolKind::Function),
        ("function_declaration", SymbolKind::Function),
        ("class_definition", SymbolKind::Class),
        ("object_definition", SymbolKind::Module),
        ("trait_definition", SymbolKind::Class),
        ("enum_definition", SymbolKind::Enum),
        ("val_definition", SymbolKind::Constant),
        ("var_definition", SymbolKind::Variable),
        ("parameter", SymbolKind::Parameter),
    ],
    func_kinds: &["function_definition", "function_declaration"],
    class_kinds: &[
        "class_definition",
        "trait_definition",
        "object_definition",
        "enum_definition",
    ],
    param_kinds: &["parameter"],
    calls: &[CallRule {
        kind: "call_expression",
        callee_field: "function",
        arguments_field: "arguments",
        name_fn: None,
        target_fn: None,
    }],
    hooks: HookSpec {
        impl_trait_fn: Some(scala_supers),
        sig_key_fn: Some(scala_signature_key),
        file_deps_fn: Some(scala_file_deps),
        ..HookSpec::none()
    },
    markers: MarkerSpec {
        if_kinds: &["if_expression"],
        loop_kinds: &["for_expression", "while_expression", "do_while_expression"],
        switch_kinds: &["match_expression"],
        switch_block_kinds: &["case_block"],
        switch_case_kinds: &["case_clause"],
        return_kinds: &["return_expression"],
        throw_kinds: &["throw_expression"],
        try_kinds: &["try_expression"],
        except_kinds: &["catch_clause"],
        finally_kinds: &["finally_clause"],
        ..MarkerSpec::dflt()
    },
    ..LangSpec::dflt()
};
crate::lang_parser!(ScalaParser, SPEC);

/// Supers: `class C extends B with T1 with T2` → "B,T1,T2" (extends_clause
/// chứa type list). `extends App` object cũng ăn.
fn scala_supers(node: &Node, src: &[u8]) -> Option<String> {
    if !matches!(
        node.kind(),
        "class_definition" | "trait_definition" | "object_definition" | "enum_definition"
    ) {
        return None;
    }
    let clause = node.child_by_field_name("extend")?;
    let mut names: Vec<String> = Vec::new();
    let mut stack = vec![clause];
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "type_identifier" | "generic_type" | "call_site") {
            // call_site: `extends App(...)` — tên ở con đầu.
            let t = text(&n, src).map(|s| s.split('(').next().unwrap_or("").trim().to_string());
            if let Some(t) = t {
                if !t.is_empty() {
                    names.push(t);
                }
            }
            if n.kind() != "generic_type" {
                continue; // generic_type cần đi tiếp con để lấy base
            }
        }
        let mut cursor = n.walk();
        for ch in n.children(&mut cursor) {
            stack.push(ch);
        }
    }
    names.dedup();
    (!names.is_empty()).then(|| names.join(","))
}

/// Signature key `(params)(return)` — def có type hints.
fn scala_signature_key(node: &Node, src: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "function_definition" | "function_declaration") {
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
        .child_by_field_name("return_type")
        .or_else(|| node.child_by_field_name("type"))
        .and_then(|t| text(&t, src))
        .map(|t| t.replace(' ', ""))
        .unwrap_or_default();
    Some(format!("({})({})", pt.join(","), rt))
}

/// `import a.b.c` / `import a.{x, y}` → FileDeps:
/// - importer đơn → alias "c", path "a.b.c"
/// - `import a.{x as z, y}` → alias "z"/"y", path "a.x"/"a.y"
fn scala_file_deps(root: &Node, src: &[u8]) -> Option<FileDeps> {
    let mut imports: Vec<ImportAlias> = Vec::new();
    let mut stack = vec![*root];
    while let Some(n) = stack.pop() {
        if n.kind() == "import_declaration" && n.child_count() > 1 {
            // selectors. Lấy toàn bộ path text trước, xử lý selector `{...}`.
            let raw = text(&n, src).unwrap_or_default();
            let stmt = raw.trim().trim_end_matches(';').trim();
            let stmt = stmt.strip_prefix("import ").unwrap_or(stmt).trim();
            if let Some((base, sel)) = stmt.split_once(".{") {
                let base = base.trim().to_string();
                let sel = sel.trim_end_matches('}').trim();
                for item in sel.split(',') {
                    let item = item.trim();
                    if item.is_empty() {
                        continue;
                    }
                    let (name, alias) = match item.split_once("=>") {
                        Some((n2, a)) => (n2.trim().to_string(), a.trim().to_string()),
                        None => (
                            item.to_string(),
                            item.rsplit('.').next().unwrap_or(item).to_string(),
                        ),
                    };
                    if alias != "_" && !name.is_empty() {
                        imports.push(ImportAlias {
                            alias,
                            path: format!("{base}.{name}"),
                        });
                    }
                }
            } else {
                let seg = stmt.rsplit('.').next().unwrap_or("").to_string();
                if !seg.is_empty() && seg != "_" {
                    imports.push(ImportAlias {
                        alias: seg,
                        path: stmt.to_string(),
                    });
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
