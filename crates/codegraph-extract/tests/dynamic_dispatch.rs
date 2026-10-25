//! Extraction phục vụ dynamic-dispatch fan-out: `Symbol.impl_trait`
//! (Java implements / C++ base class) và `ParseResult.fn_ptr_assigns` (C/C++
//! function pointer assign-site).

use codegraph_core::{SymbolKind, MARKER_DEFER, MARKER_GO, MARKER_LOOP, MARKER_SWITCH_START};
use codegraph_extract::languages::c::CParser;
use codegraph_extract::languages::cpp::CppParser;
use codegraph_extract::languages::go::GoParser;
use codegraph_extract::languages::java::JavaParser;
use codegraph_extract::languages::python::PythonParser;
use codegraph_extract::LangParser;

#[test]
fn java_interfaces_extracted() {
    let src = r#"
interface Payment { void pay(int amount); }
interface Serializable {}
class CardPayment implements Payment {
    public void pay(int amount) {}
}
class Multi implements Payment, Serializable {
    public void pay(int amount) {}
}
class Plain {}
"#;
    let parsed = JavaParser.parse_file("T.java", src).unwrap();

    let card = parsed
        .symbols
        .iter()
        .find(|s| s.name == "CardPayment")
        .unwrap();
    assert_eq!(card.impl_trait.as_deref(), Some("Payment"));

    let multi = parsed.symbols.iter().find(|s| s.name == "Multi").unwrap();
    assert_eq!(multi.impl_trait.as_deref(), Some("Payment,Serializable"));

    let plain = parsed.symbols.iter().find(|s| s.name == "Plain").unwrap();
    assert_eq!(plain.impl_trait, None);
}

#[test]
fn go_interface_type_refined_to_interface_kind() {
    let src = r#"package main

type Writer interface {
	Write(p []byte) (int, error)
}

type Buf struct{ data []byte }

func (b *Buf) Write(p []byte) (int, error) { return len(p), nil }

func sink(w Writer) {
	w.Write(nil)
}
"#;
    let parsed = GoParser.parse_file("w.go", src).unwrap();

    let writer = parsed.symbols.iter().find(|s| s.name == "Writer").unwrap();
    assert_eq!(writer.kind, SymbolKind::Interface);

    let buf = parsed.symbols.iter().find(|s| s.name == "Buf").unwrap();
    assert_eq!(buf.kind, SymbolKind::Class);
}

#[test]
fn cpp_base_classes_extracted() {
    let src = r#"
class Base {
public:
    virtual void run() {}
};
class Derived : public Base {
public:
    void run() override {}
};
"#;
    let parsed = CppParser.parse_file("T.cpp", src).unwrap();

    let derived = parsed.symbols.iter().find(|s| s.name == "Derived").unwrap();
    assert_eq!(derived.impl_trait.as_deref(), Some("Base"));

    let base = parsed.symbols.iter().find(|s| s.name == "Base").unwrap();
    assert_eq!(base.impl_trait, None);
}

#[test]
fn c_fn_ptr_assigns_extracted() {
    let src = r#"
typedef int (*handler_t)(int);

int compile_file(int x) { return x; }
int dtrace_compile_file(int x) { return x; }

int (*zend_compile_file)(int) = compile_file;

void setup(void) {
    zend_compile_file = dtrace_compile_file;
    void (*local)(int) = compile_file;
}
"#;
    let parsed = CParser.parse_file("z.c", src).unwrap();

    let mut assigns: Vec<(&str, &str)> = parsed
        .fn_ptr_assigns
        .iter()
        .map(|a| (a.var.as_str(), a.target.as_str()))
        .collect();
    assigns.sort();
    assert_eq!(
        assigns,
        vec![
            ("local", "compile_file"),
            ("zend_compile_file", "compile_file"),
            ("zend_compile_file", "dtrace_compile_file"),
        ]
    );
}

#[test]
fn go_receiver_type_and_interface_method_scope() {
    // Chốt mục 4: interface method được extract với scope = interface symbol;
    // method thường có receiver base type trong type_name (method-set matching).
    let src = r#"package main

type Writer interface {
	Write(p []byte) (int, error)
	Flush() error
}

type Buf struct{}

func (b *Buf) Write(p []byte) (int, error) { return len(p), nil }
func (b *Buf) Flush() error { return nil }
func (f *File) Write(p []byte) (int, error) { return len(p), nil }
"#;
    let parsed = GoParser.parse_file("w.go", src).unwrap();

    let writer_id = parsed
        .symbols
        .iter()
        .find(|s| s.name == "Writer")
        .unwrap()
        .id;
    // Interface methods: scope vào interface symbol, không có receiver type.
    for s in &parsed.symbols {
        eprintln!(
            "DBG {} {:?} scope={} type={:?} line={}",
            s.name, s.kind, s.scope_id, s.type_name, s.line
        );
    }
    let iface_write = parsed
        .symbols
        .iter()
        .find(|s| s.name == "Write" && s.scope_id == writer_id)
        .expect("interface method Write not scoped to Writer");
    assert_eq!(iface_write.kind, SymbolKind::Method);
    assert_eq!(iface_write.type_name, None);

    // Method thường: receiver base type.
    let buf_write = parsed
        .symbols
        .iter()
        .find(|s| s.name == "Write" && s.type_name.as_deref() == Some("Buf"))
        .expect("Buf.Write receiver type missing");
    assert_eq!(buf_write.kind, SymbolKind::Method);
    let _file_write = parsed
        .symbols
        .iter()
        .find(|s| s.name == "Write" && s.type_name.as_deref() == Some("File"))
        .expect("File.Write receiver type missing");
    assert!(_file_write.type_name.is_some());
}

#[test]
fn go_fn_value_assign_extracted() {
    let src = r#"package main

func process(x int) int { return x }

func run() {
	handler := process
	handler(1)
	handler = process
	handler(2)
	a, b := 1, 2
}
"#;
    let parsed = GoParser.parse_file("h.go", src).unwrap();
    let assigns: Vec<(&str, &str)> = parsed
        .fn_ptr_assigns
        .iter()
        .map(|a| (a.var.as_str(), a.target.as_str()))
        .collect();
    assert_eq!(
        assigns,
        vec![("handler", "process"), ("handler", "process")]
    );
}

#[test]
fn go_file_deps_extracted() {
    let src = r#"package orders

import (
	"context"
	httpx "net/http"
	"corp/api/internal/billing"
)
"#;
    let parsed = GoParser.parse_file("o.go", src).unwrap();
    let deps = parsed.file_deps.expect("file deps missing");
    assert_eq!(deps.package.as_deref(), Some("orders"));
    let mut pairs: Vec<(&str, &str)> = deps
        .imports
        .iter()
        .map(|i| (i.alias.as_str(), i.path.as_str()))
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("billing", "corp/api/internal/billing"),
            ("context", "context"),
            ("httpx", "net/http"),
        ]
    );
}

#[test]
fn go_go_defer_select_markers_in_chain() {
    let src = r#"package main

func worker() {}
func cleanup() {}
func recv(ch chan int) {}

func run(ch chan int) {
	for i := 0; i < 3; i++ {
		go worker()
		defer cleanup()
	}
	select {
	case v := <-ch:
		recv(ch)
	default:
	}
}
"#;
    let parsed = GoParser.parse_file("m.go", src).unwrap();
    let run_chain = parsed
        .chains
        .values()
        .find(|c| c.contains(&MARKER_GO) && c.contains(&MARKER_DEFER) && c.contains(&MARKER_LOOP))
        .expect("go/defer markers missing from chain");
    assert!(run_chain.contains(&MARKER_GO));
    assert!(run_chain.contains(&MARKER_DEFER));
    // select → switch structure (SWITCH markers tồn tại trong chain nào đó).
    let has_switch = parsed
        .chains
        .values()
        .any(|c| c.contains(&MARKER_SWITCH_START));
    assert!(has_switch, "select_statement should emit switch markers");
}

#[test]
fn go_ast_dump() {
    use tree_sitter::Parser;
    let src = "package main\n\ntype Writer interface {\n\tWrite(p []byte) (int, error)\n}\n\nfunc (f *File) Write(b []byte) error { return nil }\n";
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();
    fn dump(n: tree_sitter::Node, src: &str, d: usize) {
        let mut fields = Vec::new();
        for i in 0..n.child_count() {
            let fname = n.field_name_for_child(i as u32).unwrap_or("_");
            fields.push(format!("{}:{}", fname, n.child(i).unwrap().kind()));
        }
        let txt: String = src[n.byte_range()].chars().take(40).collect();
        eprintln!(
            "{}{} [{}] {}",
            "  ".repeat(d),
            n.kind(),
            fields.join(","),
            txt
        );
        for i in 0..n.child_count() {
            dump(n.child(i).unwrap(), src, d + 1);
        }
    }
    dump(tree.root_node(), src, 0);
}

#[test]
fn go_signature_key_normalized_and_param_name_insensitive() {
    let src = r#"package main

type Reader interface {
	Read(p []byte) (int, error)
}

type Good struct{}
type Wrong struct{}

func (g Good) Read(b []byte) (int, error) { return 0, nil }
func (w Wrong) Read(s string) string { return "" }
"#;
    let parsed = GoParser.parse_file("r.go", src).unwrap();
    let key = |type_name: &str| {
        parsed
            .symbols
            .iter()
            .find(|s| s.name == "Read" && s.type_name.as_deref() == Some(type_name))
            .unwrap()
            .sig_key
            .clone()
            .expect("sig_key missing")
    };
    // Interface method (method_elem) và impl (method_declaration) cùng hàm.
    let iface_key = parsed
        .symbols
        .iter()
        .find(|s| s.name == "Read" && s.type_name.is_none())
        .unwrap()
        .sig_key
        .clone()
        .expect("iface sig_key missing");
    assert_eq!(iface_key, "([]byte)(int,error)");
    assert_eq!(key("Good"), "([]byte)(int,error)"); // khác tên param, cùng sig
    assert_eq!(key("Wrong"), "(string)(string)"); // sai signature
}

#[test]
fn rust_field_param_symbols_extracted() {
    let src = r#"
pub struct Server {
    pub db: Database,
}

fn run(db: Database, config: Config) -> u32 {
    let cache = 1;
    db.query();
    cache
}
"#;
    let parsed = codegraph_extract::languages::rust::RustParser
        .parse_file("s.rs", src)
        .unwrap();
    for s in &parsed.symbols {
        eprintln!(
            "DBG {} {:?} scope={} type={:?}",
            s.name, s.kind, s.scope_id, s.type_name
        );
    }
    let field = parsed
        .symbols
        .iter()
        .find(|s| s.name == "db" && s.kind == SymbolKind::Field);
    assert!(field.is_some(), "field db missing");
    assert_eq!(field.unwrap().type_name.as_deref(), Some("Database"));
    let param = parsed
        .symbols
        .iter()
        .find(|s| s.name == "config" && s.kind == SymbolKind::Parameter);
    assert!(param.is_some(), "parameter config missing");
    assert_eq!(param.unwrap().type_name.as_deref(), Some("Config"));
    assert!(
        parsed
            .symbols
            .iter()
            .any(|s| s.name == "cache" && s.kind == SymbolKind::Variable),
        "let cache missing"
    );
    // Không sinh symbol giả mang tên kiểu
    assert!(!parsed
        .symbols
        .iter()
        .any(|s| s.name == "Database" && s.kind == SymbolKind::Parameter));
}

#[test]
fn rust_use_deps_and_sig_key_extracted() {
    let src = r#"
use crate::storage::Database;
use std::collections::HashMap as Map;

pub struct Database;

impl Database {
    pub fn new() -> Self { Database }
    pub fn query(&self, sql: &str) -> u32 { 0 }
}
"#;
    let parsed = codegraph_extract::languages::rust::RustParser
        .parse_file("main.rs", src)
        .unwrap();
    let deps = parsed.file_deps.expect("file deps missing");
    let mut pairs: Vec<(&str, &str)> = deps
        .imports
        .iter()
        .map(|i| (i.alias.as_str(), i.path.as_str()))
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("Database", "crate::storage::Database"),
            ("Map", "std::collections::HashMap")
        ]
    );

    // sig_key: query(&self, sql: &str) -> u32 → "(self,&str)(u32)";
    // tên param bỏ (`sql` không vào key).
    let query = parsed
        .symbols
        .iter()
        .find(|s| s.name == "query")
        .expect("query missing");
    assert_eq!(query.sig_key.as_deref(), Some("(self,&str)(u32)"));
}

#[test]
fn python_params_self_fields_imports_superclasses() {
    let src = r#"
import os
import numpy as np
from orders.models import Order, get_order as fetch

class Writer:
    def write(self, data): pass

class File(Writer):
    def __init__(self, path: str, mode="r"):
        self.path = path
        self.data = np.array()

    def write(self, data: str) -> int:
        return len(data)
"#;
    let parsed = PythonParser.parse_file("f.py", src).unwrap();

    // P1: params — typed + plain + default.
    for name in ["self", "path", "mode", "data"] {
        assert!(
            parsed
                .symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Parameter && s.name == name),
            "param {name} missing"
        );
    }
    let path_param = parsed
        .symbols
        .iter()
        .find(|s| s.name == "path" && s.kind == SymbolKind::Parameter)
        .unwrap();
    assert_eq!(path_param.type_name.as_deref(), Some("str"));

    // P1b: self.attr → Field, type từ RHS class call.
    let path_field = parsed
        .symbols
        .iter()
        .find(|s| s.name == "path" && s.kind == SymbolKind::Field)
        .expect("field self.path missing");
    assert_eq!(path_field.type_name, None); // RHS là param, không phải class call
    let data_field = parsed
        .symbols
        .iter()
        .find(|s| s.name == "data" && s.kind == SymbolKind::Field)
        .expect("field self.data missing");
    assert_eq!(data_field.type_name.as_deref(), Some("np.array"));
    // Field scope vào class File (không phải __init__).
    let file_id = parsed.symbols.iter().find(|s| s.name == "File").unwrap().id;
    assert_eq!(data_field.scope_id, file_id);

    // P2: imports.
    let deps = parsed.file_deps.expect("file deps missing");
    let mut pairs: Vec<(&str, &str)> = deps
        .imports
        .iter()
        .map(|i| (i.alias.as_str(), i.path.as_str()))
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("Order", "orders.models.Order"),
            ("fetch", "orders.models.get_order"),
            ("np", "numpy"),
            ("os", "os"),
        ]
    );

    // P5: inheritance → impl_trait "Writer" (fan-out machinery ăn trực tiếp).
    let file = parsed.symbols.iter().find(|s| s.name == "File").unwrap();
    assert_eq!(file.impl_trait.as_deref(), Some("Writer"));
}
