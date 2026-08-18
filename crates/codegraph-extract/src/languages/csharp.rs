use crate::languages::common::{text, CallRule, LangSpec};
use codegraph_core::SymbolKind;
use tree_sitter::Node;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_c_sharp::LANGUAGE.into()
}

/// `new List<int>(...)` — tên class gốc (strip generic args để resolve được).
fn new_call_name(node: &Node, src: &[u8]) -> Option<String> {
    let tn = node
        .child_by_field_name("type")
        .and_then(|t| text(&t, src))?;
    let base = tn.split('<').next().unwrap_or(&tn);
    Some(base.trim().to_string())
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "csharp",
    extensions: &["cs"],
    ts_language,
    decls: &[
        ("class_declaration", SymbolKind::Class),
        ("struct_declaration", SymbolKind::Class),
        ("interface_declaration", SymbolKind::Interface),
        ("enum_declaration", SymbolKind::Enum),
        ("record_declaration", SymbolKind::Class),
        ("namespace_declaration", SymbolKind::Module),
        ("method_declaration", SymbolKind::Method),
        ("constructor_declaration", SymbolKind::Method),
        ("property_declaration", SymbolKind::Field),
        ("variable_declaration", SymbolKind::Variable),
        ("parameter", SymbolKind::Parameter),
    ],
    func_kinds: &["method_declaration", "constructor_declaration"],
    class_kinds: &[
        "class_declaration",
        "struct_declaration",
        "interface_declaration",
        "enum_declaration",
        "record_declaration",
    ],
    param_kinds: &["parameter"],
    annotation_kinds: &["attribute"],
    name_type_fallback: false,
    calls: &[
        CallRule {
            kind: "invocation_expression",
            callee_field: "function",
            arguments_field: "arguments",
            name_fn: None,
            target_fn: None,
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
        "foreach_statement",
        "while_statement",
        "do_statement",
    ],
    switch_kinds: &["switch_statement", "switch_expression"],
    switch_block_kinds: &["switch_body"],
    switch_case_kinds: &["switch_section", "switch_expression_arm"],
    switch_default_kinds: &[],
    return_kinds: &["return_statement"],
    break_kinds: &["break_statement"],
    continue_kinds: &["continue_statement"],
    throw_kinds: &["throw_statement"],
    try_kinds: &["try_statement"],
    except_kinds: &["catch_clause", "catch_declaration"],
    try_else_kinds: &[],
    finally_kinds: &["finally_clause"],
    if_cond_field: "condition",
    if_cons_field: "consequence",
    if_alt_field: "alternative",
    body_field: "body",
};

crate::lang_parser!(CSharpParser, SPEC);

#[cfg(test)]
mod tests {
    use crate::LangParser;
    use codegraph_core::{Symbol, SymbolKind};

    fn parse(src: &str) -> Vec<Symbol> {
        super::CSharpParser::new()
            .parse_file("test.cs", src)
            .unwrap()
            .symbols
    }

    fn ann_names(sym: &Symbol) -> Vec<String> {
        sym.annotations.iter().map(|a| a.name.clone()).collect()
    }

    #[test]
    fn csharp_controller_annotations_are_extracted() {
        let src = r#"
using Microsoft.AspNetCore.Mvc;

namespace CodeGraphReproFixtures.Controllers;

[ApiController]
[Route("api/[controller]")]
public class ProductsController : ControllerBase
{
    [HttpGet]
    public IActionResult GetAll() => Ok();

    [HttpGet("{id}")]
    public IActionResult GetById(string id) => Ok();

    [HttpPost()]
    public IActionResult Create([FromBody] object body) => Ok();

    [HttpPost("custom")]
    public IActionResult CreateCustom([FromBody] object body) => Ok();

    [HttpPut("{id}")]
    public IActionResult Replace(string id, [FromBody] object body) => Ok();

    [HttpPatch("{id}")]
    public IActionResult PartialUpdate(string id, [FromBody] object body) => Ok();

    [HttpDelete("{id}")]
    public IActionResult Delete(string id) => Ok();
}
"#;
        let syms = parse(src);
        let by_name = |n: &str| {
            syms.iter()
                .find(|s| s.name == n)
                .unwrap_or_else(|| panic!("symbol `{n}` not found"))
                .clone()
        };

        let cls = by_name("ProductsController");
        assert_eq!(cls.kind, SymbolKind::Class);
        let cls_ann = ann_names(&cls);
        assert!(
            cls_ann.contains(&"ApiController".to_string()),
            "class missing ApiController: {cls_ann:?}"
        );
        assert!(
            cls_ann.contains(&"Route".to_string()),
            "class missing Route: {cls_ann:?}"
        );

        for (method, attr) in [
            ("GetAll", "HttpGet"),
            ("GetById", "HttpGet"),
            ("Create", "HttpPost"),
            ("CreateCustom", "HttpPost"),
            ("Replace", "HttpPut"),
            ("PartialUpdate", "HttpPatch"),
            ("Delete", "HttpDelete"),
        ] {
            let m = by_name(method);
            assert_eq!(m.kind, SymbolKind::Method, "kind of {method}");
            let ann = ann_names(&m);
            assert!(
                ann.contains(&attr.to_string()),
                "{method} missing {attr}: {ann:?}"
            );
        }

        // Route argument template should be captured positionally.
        let route = cls
            .annotations
            .iter()
            .find(|a| a.name == "Route")
            .expect("Route annotation");
        assert!(
            route.args.values().any(|v| v.contains("api/[controller]")),
            "route args: {:?}",
            route.args
        );
    }
}
