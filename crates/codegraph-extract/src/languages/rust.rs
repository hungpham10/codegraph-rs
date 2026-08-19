use crate::languages::common::{text, CallRule, LangSpec};
use codegraph_core::SymbolKind;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_rust::LANGUAGE.into()
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "rust",
    extensions: &["rs"],
    ts_language,
    decls: &[
        ("function_item", SymbolKind::Function),
        ("struct_item", SymbolKind::Class),
        ("enum_item", SymbolKind::Enum),
        ("trait_item", SymbolKind::Interface),
        ("impl_item", SymbolKind::Class),
        ("mod_item", SymbolKind::Module),
        ("const_item", SymbolKind::Constant),
        ("static_item", SymbolKind::Variable),
        ("type_item", SymbolKind::Class),
    ],
    func_kinds: &["function_item"],
    class_kinds: &[
        "struct_item",
        "enum_item",
        "trait_item",
        "impl_item",
        "mod_item",
    ],
    param_kinds: &[],
    annotation_kinds: &["attribute"],
    // `impl Foo` không có name field — tên nằm ở field `type`.
    name_type_fallback: true,
    // Rust: impl_item cũng là Class → re-parent methods về struct def cùng tên.
    link_impl_methods: true,
    calls: &[CallRule {
        kind: "call_expression",
        callee_field: "function",
        arguments_field: "arguments",
        name_fn: None,
        target_fn: None,
    }],
    class_type_name: Some(rust_class_type_name),
    if_kinds: &["if_expression", "if_let_expression"],
    elif_kinds: &[],
    if_block_kinds: &[],
    loop_kinds: &[
        "loop_expression",
        "while_expression",
        "while_let_expression",
        "for_expression",
    ],
    switch_kinds: &["match_expression"],
    switch_block_kinds: &["match_block"],
    switch_case_kinds: &["match_arm"],
    switch_default_kinds: &[],
    return_kinds: &["return_expression"],
    break_kinds: &["break_expression"],
    continue_kinds: &["continue_expression"],
    throw_kinds: &[],
    try_kinds: &[],
    except_kinds: &[],
    try_else_kinds: &[],
    finally_kinds: &[],
    if_cond_field: "condition",
    if_cons_field: "consequence",
    if_alt_field: "alternative",
    body_field: "body",
};

/// Chỉ `impl Foo` / `impl Trait for Foo` có `type` field (self type) — dùng làm
/// `type_name` để `link_impl_methods_to_def` nối impl → struct def cùng tên.
/// Các class-like khác (struct/enum/trait/mod) trả None → không bị coi là impl.
fn rust_class_type_name(node: &tree_sitter::Node, src: &[u8]) -> Option<String> {
    if node.kind() == "impl_item" {
        node.child_by_field_name("type").and_then(|t| text(&t, src))
    } else {
        None
    }
}

crate::lang_parser!(RustParser, SPEC);

#[cfg(test)]
mod tests {
    use crate::LangParser;
    use codegraph_core::{Symbol, SymbolKind};

    fn parse(src: &str) -> Vec<Symbol> {
        super::RustParser::new()
            .parse_file("test.rs", src)
            .unwrap()
            .symbols
    }

    fn ann_names(sym: &Symbol) -> Vec<String> {
        sym.annotations.iter().map(|a| a.name.clone()).collect()
    }

    #[test]
    fn rust_attributes_are_extracted() {
        let src = r#"
#[derive(Debug, Clone)]
pub struct Foo;

#[tokio::main]
async fn main() {}
"#;
        let syms = parse(src);

        let foo = syms
            .iter()
            .find(|s| s.name == "Foo")
            .expect("Foo not found")
            .clone();
        assert_eq!(foo.kind, SymbolKind::Class);
        let ann = ann_names(&foo);
        assert!(
            ann.contains(&"derive".to_string()),
            "Foo missing derive: {ann:?}"
        );

        let main = syms
            .iter()
            .find(|s| s.name == "main")
            .expect("main not found")
            .clone();
        assert_eq!(main.kind, SymbolKind::Function);
        let ann = ann_names(&main);
        // Rust attribute has no `name` field, so the first path identifier is used.
        assert!(
            ann.contains(&"tokio".to_string()),
            "main missing tokio attribute: {ann:?}"
        );
    }

    #[test]
    fn rust_impl_methods_attached_to_struct() {
        let src = r#"
pub struct Foo {
    x: i32,
}
impl Foo {
    pub fn new() -> Foo { Foo { x: 0 } }
    pub fn get(&self) -> i32 { self.x }
}
"#;
        let syms = parse(src);

        // Đúng 1 symbol Class "Foo" có type_ref == 0 (chính là struct def).
        let defs: Vec<&Symbol> = syms
            .iter()
            .filter(|s| s.kind == SymbolKind::Class && s.name == "Foo" && s.type_ref == 0)
            .collect();
        assert_eq!(defs.len(), 1, "expected exactly one struct definition Foo");
        let struct_id = defs[0].id;

        // Impl symbol (cũng Class "Foo") phải có type_ref trỏ về struct.
        let impls: Vec<&Symbol> = syms
            .iter()
            .filter(|s| s.kind == SymbolKind::Class && s.name == "Foo" && s.type_ref != 0)
            .collect();
        assert_eq!(impls.len(), 1, "expected one impl symbol linked to struct");
        assert_eq!(impls[0].type_ref, struct_id);

        // Method `new` phải được scoped vào struct, không phải impl.
        let new_method = syms
            .iter()
            .find(|s| s.kind == SymbolKind::Method && s.name == "new")
            .expect("new method not found");
        assert_eq!(
            new_method.scope_id, struct_id,
            "method should be scoped to the struct, not the impl"
        );
    }
}
