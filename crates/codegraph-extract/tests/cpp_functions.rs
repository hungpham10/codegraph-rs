//! Regression tests for C++ free function / out-of-class ctor extraction.

use codegraph_core::SymbolKind;
use codegraph_extract::registry;

fn parse_cpp(source: &str) -> codegraph_graph::ParseResult {
    let parser = registry()
        .into_iter()
        .find(|p| p.name() == "cpp")
        .expect("cpp parser");
    parser.parse_file("test.cpp", source).expect("parse")
}

fn functions(source: &str) -> Vec<(String, String)> {
    parse_cpp(source)
        .symbols
        .into_iter()
        .filter(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
        .map(|s| (s.name, s.signature.unwrap_or_default()))
        .collect()
}

#[test]
fn cpp_out_of_class_ctor_with_specifiers_issue_9() {
    let source = include_str!("fixtures/issue9_attr_specifiers.h");
    let result = parse_cpp(source);

    // 12 out-of-class definitions (3 ctor + 1 dtor, mỗi class) — function_definition
    // với qualified_identifier declarator.
    let out_of_class: Vec<_> = result
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Function)
        .map(|s| (s.name.clone(), s.signature.clone().unwrap_or_default()))
        .collect();
    assert_eq!(
        out_of_class.len(),
        12,
        "expected 12 out-of-class definitions, got {out_of_class:?}"
    );

    // Mỗi class: 3 ctor + 1 dtor, cùng tên với class.
    for (class, attr) in [
        ("ConstexprWidget", "constexpr"),
        ("NodiscardWidget", "[[nodiscard]]"),
        ("CustomWidget", "_CUSTOM_ATTRIBUTE"),
    ] {
        let ctor = out_of_class
            .iter()
            .filter(|(n, _)| n == class)
            .count();
        assert_eq!(ctor, 3, "{class} phải có 3 ctor, got {out_of_class:?}");
        let dtor = out_of_class
            .iter()
            .filter(|(n, _)| n == &format!("~{class}"))
            .count();
        assert_eq!(dtor, 1, "{class} phải có 1 dtor, got {out_of_class:?}");

        for (n, sig) in out_of_class.iter().filter(|(n, _)| n == class) {
            assert!(
                sig.contains(class) && sig.contains("::") && sig.contains(attr),
                "signature {sig:?} của {n} phải chứa {attr:?} và qualified name {class:?}"
            );
        }
    }

    // In-class declarations (`Foo();`) phải là Method, không phải Variable.
    let decls: Vec<_> = result
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Method)
        .collect();
    assert!(
        !decls.is_empty(),
        "expected in-class ctor declarations as Method"
    );
}

#[test]
fn cpp_free_functions_use_function_name_not_return_type() {
    let source = r#"
namespace repro_ns {

void alpha_void_plain() {}

void bravo_void_params(int x, double y) {}

int charlie_int_plain(int x) { return x; }

std::pair<int, int> delta_pair_plain(int a, int b) { return {a, b}; }

int *echo_pointer_plain(int *p) { return p; }

const int &foxtrot_const_ref_plain(const int &x) { return x; }

auto golf_auto_plain(int x) { return x; }

auto hotel_auto_trailing_plain(int x) -> int { return x; }

template <typename T> T india_template_T_return(T x) { return x; }

template <typename T> void juliet_template_void_return(T x) {}

template <typename T> int kilo_template_int_return(T x) { return 42; }

template <typename T> auto lima_template_auto_return(T x) { return x; }

template <typename T> auto mike_template_auto_trailing(T x) -> int {
  return 42;
}

template <typename T> std::pair<T, T> november_template_pair_return(T a, T b) {
  return {a, b};
}

template <typename T, typename = std::enable_if_t<std::is_integral_v<T>>>
T oscar_sfinae_return(T x) {
  return x;
}

template <typename T> T papa_noexcept_return(T x) noexcept { return x; }

constexpr int quebec_constexpr_plain(int x) { return x * 2; }

[[nodiscard]] int romeo_nodiscard_plain(int x) { return x; }

inline int sierra_inline_plain(int x) { return x; }

static int tango_static_plain(int x) { return x; }

} // namespace repro_ns
"#;

    let fns = functions(source);
    let names: Vec<_> = fns.iter().map(|(n, _)| n.clone()).collect();
    let expected = [
        "alpha_void_plain",
        "bravo_void_params",
        "charlie_int_plain",
        "delta_pair_plain",
        "echo_pointer_plain",
        "foxtrot_const_ref_plain",
        "golf_auto_plain",
        "hotel_auto_trailing_plain",
        "india_template_T_return",
        "juliet_template_void_return",
        "kilo_template_int_return",
        "lima_template_auto_return",
        "mike_template_auto_trailing",
        "november_template_pair_return",
        "oscar_sfinae_return",
        "papa_noexcept_return",
        "quebec_constexpr_plain",
        "romeo_nodiscard_plain",
        "sierra_inline_plain",
        "tango_static_plain",
    ];

    for name in expected {
        assert!(
            names.iter().any(|n| n == name),
            "missing function {name:?}, got {names:?}"
        );
    }
    assert!(
        !names.iter().any(|n| n == "T"),
        "return type must not be used as name, got {names:?}"
    );
}
