use crate::languages::common::{text, CallRule, HookSpec, LangSpec, MarkerSpec};
use codegraph_core::SymbolKind;
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
crate::lang_parser!(RubyParser, SPEC);
