use crate::languages::common::{first_identifier, named_children, text, CallRule, LangSpec};
use codegraph_core::SymbolKind;
use codegraph_graph::{FileDeps, ImportAlias};
use tree_sitter::Node;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_go::LANGUAGE.into()
}

/// `f := func(){}` / `var h = func(){}` — func_literal mượn tên biến.
/// func_literal truyền thẳng (`go func(){ }()`) không được đặt tên.
fn anonymous_name_node<'a>(node: &Node<'a>) -> Option<Node<'a>> {
    let el = node.parent()?;
    if el.kind() != "expression_list" {
        return None;
    }
    let p = el.parent()?;
    match p.kind() {
        // `var h = func(){ }` — tên ở field `name` của var_spec.
        "var_spec" => p.child_by_field_name("name"),
        // `f := func(){ }` — tên là identifier đầu của expression_list left.
        "short_var_declaration" => {
            let left = p.child_by_field_name("left")?;
            first_identifier(&left)
        }
        _ => None,
    }
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "go",
    extensions: &["go"],
    ts_language,
    decls: &[
        ("function_declaration", SymbolKind::Function),
        ("method_declaration", SymbolKind::Method),
        // Method KHAI BÁO trong interface body — grammar node riêng (mục 4).
        ("method_elem", SymbolKind::Method),
        ("type_spec", SymbolKind::Class),
        ("var_spec", SymbolKind::Variable),
        ("const_spec", SymbolKind::Constant),
        ("parameter_declaration", SymbolKind::Parameter),
        ("func_literal", SymbolKind::Function),
    ],
    func_kinds: &["function_declaration", "method_declaration", "func_literal"],
    // `type_spec` là class-like scope: fields của struct và method của
    // interface scope vào symbol của type (interface method cần scope đúng để
    // dynamic-dispatch fan-out nhận ra).
    class_kinds: &["type_spec"],
    param_kinds: &["parameter_declaration"],
    annotation_kinds: &[],
    name_type_fallback: false,

    link_impl_methods: false,
    anonymous_name_fn: Some(anonymous_name_node),
    value_func_kinds: &["func_literal"],
    calls: &[CallRule {
        kind: "call_expression",
        callee_field: "function",
        arguments_field: "arguments",
        name_fn: None,
        target_fn: None,
    }],
    class_type_name: None,
    impl_trait_fn: None,
    refine_kind_fn: Some(go_refine_kind),
    fn_ptr_assign_fn: Some(go_fn_ptr_assign),
    receiver_fn: Some(go_receiver_type),
    file_deps_fn: Some(go_file_deps),
    go_stmt_kinds: &["go_statement"],
    defer_kinds: &["defer_statement"],
    if_kinds: &["if_statement"],
    elif_kinds: &[],
    if_block_kinds: &[],
    loop_kinds: &["for_statement"],
    switch_kinds: &[
        "expression_switch_statement",
        "type_switch_statement",
        "select_statement",
    ],
    switch_block_kinds: &[],
    switch_case_kinds: &["expression_case", "type_case", "communication_case"],
    switch_default_kinds: &["default_case"],
    return_kinds: &["return_statement"],
    break_kinds: &["break_statement"],
    continue_kinds: &["continue_statement"],
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

crate::lang_parser!(GoParser, SPEC);

/// Go interface là ngầm định: `type_spec` có body `interface_type` → Interface
/// (mặc định type_spec là Class). Struct giữ nguyên Class.
fn go_refine_kind(node: &Node) -> Option<SymbolKind> {
    if node.kind() != "type_spec" {
        return None;
    }
    let body = node.child_by_field_name("type")?;
    (body.kind() == "interface_type").then_some(SymbolKind::Interface)
}

/// Receiver base type cua method: `func (f *File) Write` -> "File" (pointer
/// va value nhu nhau - method set matching theo ten type). Generic `List[T]`
/// -> "List".
fn go_receiver_type(node: &Node, src: &[u8]) -> Option<String> {
    if node.kind() != "method_declaration" {
        return None;
    }
    let receiver = node.child_by_field_name("receiver")?;
    // receiver là parameter_list → parameter_declaration đầu tiên → type.
    let param = receiver
        .children(&mut receiver.walk())
        .find(|c| c.kind() == "parameter_declaration")?;
    let ty = param.child_by_field_name("type")?;
    let mut stack = vec![ty];
    while let Some(n) = stack.pop() {
        if n.kind() == "type_identifier" {
            let raw = text(&n, src)?;
            let name = raw.split('[').next()?.trim().to_string();
            return (!name.is_empty()).then_some(name);
        }
        let mut cursor = n.walk();
        for ch in n.children(&mut cursor) {
            stack.push(ch);
        }
    }
    None
}

/// Function-value assignment: `handler := process` / `handler = process` -
/// ca hai phia la identifier thuan. Go khong co fn pointer kieu C nhung
/// function value dispatch giong het -> dung chung machinery PointerAssign.
fn go_fn_ptr_assign(node: &Node, src: &[u8]) -> Option<(String, String)> {
    match node.kind() {
        "short_var_declaration" | "assignment_statement" => {
            let left = node.child_by_field_name("left")?;
            let right = node.child_by_field_name("right")?;
            single_identifier(&left, src).zip(single_identifier(&right, src))
        }
        _ => None,
    }
}

/// Package + imports cua file: `package orders` + khoi import (alias -> path).
/// Import khong alias: alias = segment cuoi cua path (`net/http` -> `http`).
fn go_file_deps(root: &Node, src: &[u8]) -> Option<FileDeps> {
    let mut deps = FileDeps::default();
    let mut stack = vec![*root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "package_clause" => {
                // package_identifier là unnamed child (không có field "name").
                let mut cursor = n.walk();
                for ch in n.children(&mut cursor) {
                    if ch.kind() == "package_identifier" {
                        if let Some(t) = text(&ch, src) {
                            deps.package = Some(t);
                        }
                    }
                }
            }
            "import_spec" => {
                let path_node = n.child_by_field_name("path")?;
                let raw = text(&path_node, src)?;
                let path = raw.trim_matches('"').to_string();
                let alias = if let Some(name) = n.child_by_field_name("name") {
                    text(&name, src)?
                } else {
                    path.rsplit('/').next().unwrap_or("").to_string()
                };
                if !alias.is_empty() {
                    deps.imports.push(ImportAlias { alias, path });
                }
            }
            _ => {}
        }
        let mut cursor = n.walk();
        for ch in n.children(&mut cursor) {
            stack.push(ch);
        }
    }
    (deps.package.is_some() || !deps.imports.is_empty()).then_some(deps)
}

/// Identifier duy nhat trong node (expression_list 1 phan tu hoac identifier).
fn single_identifier(node: &Node, src: &[u8]) -> Option<String> {
    if node.kind() == "identifier" {
        return text(node, src);
    }
    let kids = named_children(node);
    if kids.len() != 1 && node.kind() == "expression_list" {
        return None; // `a, b := f, g` - khong track
    }
    for k in kids {
        if k.kind() == "identifier" {
            return text(&k, src);
        }
    }
    None
}
