use crate::languages::common::{text, CallRule, HookSpec, LangSpec, MarkerSpec};
use codegraph_core::SymbolKind;
use codegraph_graph::{FileDeps, ImportAlias};
use tree_sitter::Node;

fn ts_language() -> tree_sitter::Language {
    tree_sitter_ruby::LANGUAGE.into()
}

/// `receiver.method` nếu có receiver, không thì `method`.
fn call_name(node: &Node, src: &[u8]) -> Option<String> {
    let method = node
        .child_by_field_name("method")
        .and_then(|m| text(&m, src))?;
    if let Some(receiver) = node.child_by_field_name("receiver") {
        if let Some(r) = text(&receiver, src) {
            if !r.is_empty() {
                return Some(format!("{r}.{method}"));
            }
        }
    }
    Some(method)
}

/// Class Ruby `class Foo < Bar` — superclass làm type_name.
fn class_type_name(node: &Node, src: &[u8]) -> Option<String> {
    node.child_by_field_name("superclass")
        .and_then(|s| text(&s, src))
}

pub static SPEC: LangSpec = LangSpec {
    language_name: "ruby",
    extensions: &["rb"],
    ts_language,
    decls: &[
        ("method", SymbolKind::Method),
        ("singleton_method", SymbolKind::Method),
        ("class", SymbolKind::Class),
        ("module", SymbolKind::Module),
    ],
    func_kinds: &["method", "singleton_method"],
    class_kinds: &["class", "module"],
    param_container_kinds: &["method_parameters"],
    calls: &[
        CallRule {
            kind: "call",
            callee_field: "method",
            arguments_field: "arguments",
            name_fn: Some(call_name),
            target_fn: None,
        },
        CallRule {
            kind: "command_call",
            callee_field: "method",
            arguments_field: "arguments",
            name_fn: Some(call_name),
            target_fn: None,
        },
    ],
    hooks: HookSpec {
        class_type_name: Some(class_type_name),
        impl_trait_fn: Some(ruby_superclass),
        file_deps_fn: Some(ruby_require_deps),
        fn_ptr_assign_fn: Some(ruby_fn_value_assign),
        ..HookSpec::none()
    },
    markers: MarkerSpec {
        if_kinds: &["if"],
        elif_kinds: &["elsif"],
        loop_kinds: &["while", "until", "for"],
        switch_kinds: &["case"],
        switch_case_kinds: &["when"],
        switch_default_kinds: &["else"],
        return_kinds: &["return"],
        break_kinds: &["break"],
        continue_kinds: &["next"],
        ..MarkerSpec::dflt()
    },
    ..LangSpec::dflt()
};
/// String literal đầu tiên trong argument list của `require` — helper tách
/// riêng để tránh borrow scope của cursor.
/// Function-value assignment Ruby: `handler = method(:process)` — RHS là
/// call `method(...)` với symbol literal. `handler = process` là LỜI GỌI
/// process() trong Ruby — KHÔNG track (khác Go/JS).
fn ruby_fn_value_assign(node: &Node, src: &[u8]) -> Option<(String, String)> {
    if node.kind() != "assignment" && node.kind() != "single_assignment" {
        return None;
    }
    let left = node.child_by_field_name("left")?;
    if left.kind() != "identifier" {
        return None;
    }
    let right = node.child_by_field_name("right")?;
    if right.kind() != "call" {
        return None;
    }
    let m = right.child_by_field_name("method")?;
    if text(&m, src).as_deref() != Some("method") {
        return None;
    }
    // Argument: symbol literal `:process` — bỏ ':'.
    let args = right.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let target = args
        .children(&mut cursor)
        .find(|c| c.kind() == "simple_symbol")
        .and_then(|s| text(&s, src))
        .map(|t| t.trim_start_matches(':').to_string())?;
    let var = text(&left, src)?;
    Some((var, target))
}

fn ruby_require_path<'a>(args: &Node<'a>, src: &'a [u8]) -> Option<String> {
    let mut cursor = args.walk();
    let mut found = None;
    for ch in args.children(&mut cursor) {
        if ch.kind() == "string" {
            if let Some(t) = text(&ch, src) {
                found = Some(
                    t.trim_matches(|c| c == '\'' || c == '"')
                        .trim_start_matches("./")
                        .to_string(),
                );
            }
            break;
        }
    }
    found
}

crate::lang_parser!(RubyParser, SPEC);

/// Superclass `class Foo < Bar` → "Bar" — inheritance fan-out (giống type_name
/// nhưng đi vào impl_index; `Object`/`Struct::new`-style bỏ qua).
fn ruby_superclass(node: &Node, src: &[u8]) -> Option<String> {
    if node.kind() != "class" {
        return None;
    }
    let sc = node.child_by_field_name("superclass")?;
    let raw = text(&sc, src)?;
    let name = raw.trim_start_matches('<').trim();
    // Loại dynamic superclass (`< Struct.new(:x)` / `< Base.new`).
    if name.is_empty() || name.contains('(') || name.contains(".new") {
        return None;
    }
    Some(name.to_string())
}

/// `require 'x'` / `require_relative 'x'` → FileDeps (alias = segment cuối).
fn ruby_require_deps(root: &Node, src: &[u8]) -> Option<FileDeps> {
    let mut imports: Vec<ImportAlias> = Vec::new();
    let mut stack = vec![*root];
    while let Some(n) = stack.pop() {
        let is_require = n.kind() == "call"
            && n.child_by_field_name("method")
                .and_then(|m| text(&m, src))
                .is_some_and(|mtxt| mtxt == "require" || mtxt == "require_relative");
        if is_require {
            // argument đầu là string literal.
            if n.kind() == "call" {
                if let Some(args) = n.child_by_field_name("arguments") {
                    if let Some(path) = ruby_require_path(&args, src) {
                        if let Some(seg) = path.rsplit('/').next() {
                            if !seg.is_empty() {
                                imports.push(ImportAlias {
                                    alias: seg.to_string(),
                                    path,
                                });
                            }
                        }
                    }
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
