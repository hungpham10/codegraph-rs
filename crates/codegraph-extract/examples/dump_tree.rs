//! Debug tool: parse stdin with a language and print the tree-sitter s-expression
//! annotated with field names. Usage: `cargo run -p codegraph-extract --example dump_tree -- <lang>`

use codegraph_extract::registry;
use std::io::Read;

fn main() {
    let lang = std::env::args()
        .nth(1)
        .expect("usage: dump_tree <language>");
    let mut src = String::new();
    std::io::stdin().read_to_string(&mut src).unwrap();

    let parser = registry()
        .into_iter()
        .find(|p| p.name() == lang)
        .unwrap_or_else(|| panic!("no parser for {lang}"));

    let mut p = tree_sitter::Parser::new();
    p.set_language(&parser.ts_language()).unwrap();
    let tree = p.parse(&src, None).unwrap();
    print_sexp(&tree.root_node(), &src, 0);
}

fn print_sexp(node: &tree_sitter::Node, src: &str, depth: usize) {
    let indent = "  ".repeat(depth);
    let field = node
        .parent()
        .and_then(|par| {
            (0..par.child_count())
                .find(|&i| par.child(i).map(|c| c.id() == node.id()).unwrap_or(false))
                .and_then(|i| par.field_name_for_child(i as u32))
        })
        .map(|f| format!(" [{f}]"))
        .unwrap_or_default();
    let text = node
        .utf8_text(src.as_bytes())
        .ok()
        .map(|t| t.replace('\n', "\\n"))
        .map(|t| {
            if t.len() > 60 {
                format!("{}…", &t[..60])
            } else {
                t
            }
        });
    println!(
        "{indent}{}{}{}{}",
        node.kind(),
        field,
        if node.is_named() { "" } else { " !" },
        text.map(|t| format!("  \"{t}\"")).unwrap_or_default()
    );
    let mut cursor = node.walk();
    for ch in node.children(&mut cursor) {
        print_sexp(&ch, src, depth + 1);
    }
}
