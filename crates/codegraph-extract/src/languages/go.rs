use crate::languages::common::{
    first_identifier, named_children, text, CallRule, HookSpec, LangSpec, MarkerSpec,
};
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
        ("method_elem", SymbolKind::Method),
        ("type_spec", SymbolKind::Class),
        ("var_spec", SymbolKind::Variable),
        ("const_spec", SymbolKind::Constant),
        ("parameter_declaration", SymbolKind::Parameter),
        ("func_literal", SymbolKind::Function),
    ],
    func_kinds: &["function_declaration", "method_declaration", "func_literal"],
    class_kinds: &["type_spec"],
    param_kinds: &["parameter_declaration"],
    calls: &[CallRule {
        kind: "call_expression",
        callee_field: "function",
        arguments_field: "arguments",
        name_fn: None,
        target_fn: None,
    }],
    hooks: HookSpec {
        sig_key_fn: Some(go_signature_key),
        refine_kind_fn: Some(go_refine_kind),
        fn_ptr_assign_fn: Some(go_fn_ptr_assign),
        receiver_fn: Some(go_receiver_type),
        file_deps_fn: Some(go_file_deps),
        anonymous_name_fn: Some(anonymous_name_node),
        value_func_kinds: &["func_literal"],
        ..HookSpec::none()
    },
    markers: MarkerSpec {
        go_stmt_kinds: &["go_statement"],
        defer_kinds: &["defer_statement"],
        if_kinds: &["if_statement"],
        loop_kinds: &["for_statement"],
        switch_kinds: &[
            "expression_switch_statement",
            "type_switch_statement",
            "select_statement",
        ],
        switch_case_kinds: &["expression_case", "type_case", "communication_case"],
        switch_default_kinds: &["default_case"],
        return_kinds: &["return_statement"],
        break_kinds: &["break_statement"],
        continue_kinds: &["continue_statement"],
        ..MarkerSpec::dflt()
    },
    ..LangSpec::dflt()
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

/// Signature key chuan hoa `(param_types)(result_types)` — bo ten param, giu
/// nguyen type text (`[]byte`, `map[string]int`, `*T`, `time.Time`, `...int`).
/// method_declaration va method_elem (interface) dung cung ham nen key so
/// sanh truc tiep. `Reader.Read` -> `([]byte)(int,error)`.
fn go_signature_key(node: &Node, src: &[u8]) -> Option<String> {
    if !matches!(
        node.kind(),
        "method_declaration" | "method_elem" | "function_declaration"
    ) {
        return None;
    }
    let params = node.child_by_field_name("parameters")?;
    let mut pt: Vec<String> = Vec::new();
    for ch in named_children(&params) {
        if let Some(t) = ch.child_by_field_name("type") {
            if let Some(txt) = text(&t, src) {
                pt.push(compact(&txt));
            }
        }
    }
    let mut rt: Vec<String> = Vec::new();
    if let Some(res) = node.child_by_field_name("result") {
        if res.kind() == "parameter_list" {
            for ch in named_children(&res) {
                if let Some(t) = ch.child_by_field_name("type") {
                    if let Some(txt) = text(&t, src) {
                        rt.push(compact(&txt));
                    }
                }
            }
        } else if let Some(txt) = text(&res, src) {
            rt.push(compact(&txt));
        }
    }
    Some(format!("({})({})", pt.join(","), rt.join(",")))
}

/// Go bo whitespace trong type text: `map[string] int` -> `map[string]int`.
fn compact(t: &str) -> String {
    t.chars().filter(|c| !c.is_whitespace()).collect()
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
