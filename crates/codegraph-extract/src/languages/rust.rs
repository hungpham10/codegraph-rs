use crate::languages::common::{named_children, text, CallRule, HookSpec, LangSpec, MarkerSpec};
use codegraph_core::SymbolKind;
use codegraph_graph::{FileDeps, ImportAlias};
use tree_sitter::Node;

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
        // Semantic enrichment: field/param/local — type_name từ field `type`
        // giúp tie-break resolve `db.query()` theo type của `db`.
        ("field_declaration", SymbolKind::Field),
        ("parameter", SymbolKind::Parameter),
        ("let_declaration", SymbolKind::Variable),
        // Method KHAI BÁO trong trait (như Go method_elem) — scope vào trait
        // Interface symbol → trigger fan-out thật (không có symbol này,
        // dynamic dispatch Rust chỉ match impl theo tên, chọn 1 best guess).
        ("function_signature_item", SymbolKind::Method),
    ],
    func_kinds: &["function_item"],
    class_kinds: &[
        "struct_item",
        "enum_item",
        "trait_item",
        "impl_item",
        "mod_item",
    ],
    annotation_kinds: &["attribute"],
    calls: &[
        CallRule {
            kind: "call_expression",
            callee_field: "function",
            arguments_field: "arguments",
            name_fn: None,
            target_fn: None,
        },
        // `println!(...)`, `vec![...]` — macro invocation là call thật.
        CallRule {
            kind: "macro_invocation",
            callee_field: "macro",
            arguments_field: "tokens",
            name_fn: Some(rust_macro_call_name),
            target_fn: None,
        },
    ],
    name_type_fallback: true,
    link_impl_methods: true,
    hooks: HookSpec {
        class_type_name: Some(rust_class_type_name),
        impl_trait_fn: Some(rust_impl_trait_name),
        sig_key_fn: Some(rust_signature_key),
        file_deps_fn: Some(rust_file_deps),
        ..HookSpec::none()
    },
    markers: MarkerSpec {
        if_kinds: &["if_expression", "if_let_expression"],
        loop_kinds: &[
            "loop_expression",
            "while_expression",
            "while_let_expression",
            "for_expression",
        ],
        switch_kinds: &["match_expression"],
        switch_block_kinds: &["match_block"],
        switch_case_kinds: &["match_arm"],
        return_kinds: &["return_expression"],
        break_kinds: &["break_expression"],
        continue_kinds: &["continue_expression"],
        ..MarkerSpec::dflt()
    },
    ..LangSpec::dflt()
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

/// Tên macro call: `println!` — macro field text + `!`.
fn rust_macro_call_name(node: &Node, src: &[u8]) -> Option<String> {
    let mac = node.child_by_field_name("macro")?;
    let raw = text(&mac, src)?;
    Some(format!("{raw}!"))
}

/// `use` declarations của file — nguyên liệu resolve qualified call
/// (`Database::new` với `use crate::storage::Database;`).
/// - `use foo::bar as baz;` → alias "baz", path "foo::bar"
/// - `use crate::storage::Database;` → alias "Database" (segment cuối)
fn rust_file_deps(root: &Node, src: &[u8]) -> Option<FileDeps> {
    let mut imports: Vec<ImportAlias> = Vec::new();
    let mut stack = vec![*root];
    while let Some(n) = stack.pop() {
        if n.kind() == "use_declaration" {
            if let Some(arg) = n.child_by_field_name("argument") {
                let raw = text(&arg, src).unwrap_or_default();
                let (path, alias) = match raw.split_once(" as ") {
                    Some((p, a)) => (p.trim().to_string(), a.trim().to_string()),
                    None => {
                        let seg = raw.rsplit("::").next().unwrap_or("").trim_end_matches('*');
                        (raw.clone(), seg.to_string())
                    }
                };
                if !alias.is_empty() && !path.is_empty() {
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

/// Signature key `(params)(return)` cho method/function — trait matching.
/// `&self`/`&mut self` → "self"; type text giữ nguyên (`&mut [u8]`, `usize`).
fn rust_signature_key(node: &Node, src: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "function_item" | "function_signature_item") {
        return None;
    }
    let mut pt: Vec<String> = Vec::new();
    if let Some(params) = node.child_by_field_name("parameters") {
        for ch in named_children(&params) {
            // self param không có field type — chuẩn hóa "self".
            let txt = match ch.child_by_field_name("type") {
                Some(t) => text(&t, src).map(|s| s.replace(' ', "")),
                None => {
                    if text(&ch, src).map(|s| s.contains("self")).unwrap_or(false) {
                        Some("self".to_string())
                    } else {
                        None
                    }
                }
            };
            if let Some(t) = txt {
                pt.push(t);
            }
        }
    }
    let mut rt: Vec<String> = Vec::new();
    if let Some(ret) = node.child_by_field_name("return_type") {
        if let Some(t) = text(&ret, src) {
            rt.push(t.replace(' ', ""));
        }
    }
    Some(format!("({})({})", pt.join(","), rt.join(",")))
}

/// Trait name của `impl Trait for Type` — field `trait` của impl_item.
/// Normalize generic: `Iterator<Item = u8>` → `"Iterator"` (fan-out index key
/// phải khớp tên trait thuần).
fn rust_impl_trait_name(node: &tree_sitter::Node, src: &[u8]) -> Option<String> {
    if node.kind() != "impl_item" {
        return None;
    }
    let raw = node
        .child_by_field_name("trait")
        .and_then(|t| text(&t, src))?;
    let name = raw.split('<').next()?.trim().to_string();
    (!name.is_empty()).then_some(name)
}

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

    #[test]
    fn rust_impl_trait_name_extracted_and_normalized() {
        let src = r#"
pub trait Handler { fn handle(&self); }
pub struct A;
pub struct B;
impl Handler for A { fn handle(&self) {} }
impl std::fmt::Display for B { fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { Ok(()) } }
impl A { fn inherent(&self) {} }
"#;
        let syms = parse(src);

        let trait_impl = syms
            .iter()
            .find(|s| s.impl_trait.as_deref() == Some("Handler"))
            .expect("impl Handler symbol not found");
        assert_eq!(trait_impl.name, "A");

        // Qualified trait `std::fmt::Display` — normalize giữ phần sau chấm cuối?
        // Không: fan-out key khớp tên trait thuần, giữ nguyên text đầy đủ.
        let display_impl = syms
            .iter()
            .find(|s| s.impl_trait.is_some() && s.name == "B")
            .expect("impl Display symbol not found");
        assert!(display_impl
            .impl_trait
            .as_deref()
            .unwrap()
            .contains("Display"));

        // Inherent impl không có trait.
        let inherent = syms
            .iter()
            .find(|s| s.name == "A" && s.kind == SymbolKind::Class)
            .expect("inherent impl not found");
        assert_eq!(inherent.impl_trait, None);
    }

    #[test]
    fn rust_method_call_records_receiver_text() {
        let src = r#"
struct S;
impl S {
    fn go(&self, h: &dyn Handler) {
        h.handle();
        plain();
    }
}
"#;
        let parsed = super::RustParser::new().parse_file("test.rs", src).unwrap();
        let method_call = parsed
            .calls
            .iter()
            .find(|c| c.call_name.ends_with("handle"))
            .expect("method call not found");
        assert_eq!(method_call.receiver_text.as_deref(), Some("h"));

        let plain = parsed
            .calls
            .iter()
            .find(|c| c.call_name == "plain")
            .expect("plain call not found");
        assert_eq!(plain.receiver_text, None);
    }
}
