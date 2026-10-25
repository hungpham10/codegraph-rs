//! Extraction phục vụ dynamic-dispatch fan-out: `Symbol.impl_trait`
//! (Java implements / C++ base class) và `ParseResult.fn_ptr_assigns` (C/C++
//! function pointer assign-site).

use codegraph_core::{SymbolKind, MARKER_DEFER, MARKER_GO, MARKER_LOOP, MARKER_SWITCH_START};
use codegraph_extract::languages::c::CParser;
use codegraph_extract::languages::cpp::CppParser;
use codegraph_extract::languages::go::GoParser;
use codegraph_extract::languages::java::JavaParser;
use codegraph_extract::languages::lua::LuaParser;
use codegraph_extract::languages::php::PhpParser;
use codegraph_extract::languages::python::PythonParser;
use codegraph_extract::languages::ruby::RubyParser;
use codegraph_extract::languages::scala::ScalaParser;
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

#[test]
fn rust_review_realistic_program() {
    // Fixture "thật" — dùng để review cấu hình Rust: trait method khai báo,
    // macro call, closure, async fn, associated const/type.
    let src = r#"
pub trait Repo {
    type Item;
    const MAX: usize;
    fn find(&self, id: u32) -> Option<Self::Item>;
    fn ping(&self);
}

pub struct Pg;

impl Repo for Pg {
    type Item = u32;
    const MAX: usize = 10;
    fn find(&self, id: u32) -> Option<u32> { Some(id) }
    fn ping(&self) { println!("ping"); }
}

pub struct Srv { repo: Pg }

impl Srv {
    pub async fn handle(&self, id: u32) -> Option<u32> {
        let cb = || self.repo.ping();
        cb();
        vec![1, 2].len();
        match id {
            0 => None,
            _ => { self.repo.find(id) }
        }
    }
}
"#;
    let parsed = codegraph_extract::languages::rust::RustParser
        .parse_file("srv.rs", src)
        .unwrap();
    for s in &parsed.symbols {
        eprintln!(
            "SYM {:?} {:?} scope={} type={:?} line={}",
            s.name, s.kind, s.scope_id, s.type_name, s.line
        );
    }
    for c in &parsed.calls {
        eprintln!(
            "CALL {:?} recv={:?} line={}",
            c.call_name, c.receiver_text, c.line
        );
    }
    // trait method khai báo (function_signature_item) có symbol?
    assert!(
        parsed.symbols.iter().any(|s| s.name == "find"
            && s.type_name.is_none()
            && parsed
                .symbols
                .iter()
                .find(|t| t.id == s.scope_id)
                .map(|p| p.kind)
                == Some(SymbolKind::Interface)),
        "trait-declared method find (Interface scope) missing"
    );
    // macro call được bắt?
    assert!(
        parsed.calls.iter().any(|c| c.call_name.contains("println")),
        "macro invocation println! missing from calls"
    );
}

#[tokio::test]
async fn rust_trait_fanout_end_to_end_real_extraction() {
    use codegraph_graph::{GraphIndex, ParseResult};

    let src = r#"
pub trait Repo {
    fn ping(&self);
}
pub struct Pg;
pub struct My;

impl Repo for Pg {
    fn ping(&self) { println!("pg"); }
}
impl Repo for My {
    fn ping(&self) { println!("my"); }
}

pub fn sink(r: &dyn Repo) {
    r.ping();
}
"#;
    let parsed: ParseResult = codegraph_extract::languages::rust::RustParser
        .parse_file("r.rs", src)
        .unwrap();
    let mut idx = GraphIndex::in_memory();
    idx.ingest(&[parsed], None).await.unwrap();

    // sink's call `r.ping()` phải fan-out cả Pg::ping lẫn My::ping.
    let sink_id = idx
        .resolve_by_name_or_id("sink", 0)
        .unwrap()
        .symbol
        .unwrap()
        .id;
    let flow = idx.flow(sink_id).await.unwrap();
    let ping = flow
        .calls
        .iter()
        .find(|c| c.to_name.ends_with("ping"))
        .expect("ping call missing");
    assert_eq!(
        ping.resolved_via,
        codegraph_core::ResolutionKind::HierarchyFanOut,
        "fan-out không fire trên extraction thật"
    );
    let mut names = ping.candidate_names.clone();
    names.sort();
    assert_eq!(names, vec!["ping", "ping"]);
}

#[test]
fn java_supers_sig_and_file_deps() {
    let src = r#"
package orders;

import com.foo.Payment;
import static com.foo.Util.go;

public class CardPayment extends BasePayment implements Payment, Serializable {
    public void pay(int amount, String currency) {}
    public CardPayment(int x) {}
}
"#;
    let parsed = JavaParser.parse_file("C.java", src).unwrap();

    // J2: extends + implements gộp vào impl_trait (superclass trước).
    let cls = parsed
        .symbols
        .iter()
        .find(|s| s.name == "CardPayment")
        .unwrap();
    assert_eq!(
        cls.impl_trait.as_deref(),
        Some("BasePayment,Payment,Serializable")
    );

    // J3: sig_key — tên param bỏ.
    let pay = parsed.symbols.iter().find(|s| s.name == "pay").unwrap();
    assert_eq!(pay.sig_key.as_deref(), Some("(int,String)(void)"));
    let ctor = parsed
        .symbols
        .iter()
        .find(|s| s.name == "CardPayment" && s.kind == SymbolKind::Method)
        .unwrap();
    assert_eq!(ctor.sig_key.as_deref(), Some("(int)(void)"));

    // J1: imports — static bỏ qua, wildcard bỏ qua.
    let deps = parsed.file_deps.expect("file deps missing");
    let mut pairs: Vec<(&str, &str)> = deps
        .imports
        .iter()
        .map(|i| (i.alias.as_str(), i.path.as_str()))
        .collect();
    pairs.sort();
    assert_eq!(pairs, vec![("Payment", "com.foo.Payment")]);
}

#[test]
fn js_params_imports_extends_fn_value() {
    let src = r#"
import D, { helper as h } from './util.js';
import * as ns from 'lib';

class Base {
    render() {}
}
class Page extends Base {
    render() { return 1; }
}

const handler = process;

function setup(db, ...rest) {
    handler(db);
    const { a, b } = obj;
}
"#;
    let parsed = codegraph_extract::languages::javascript::JavaScriptParser
        .parse_file("p.js", src)
        .unwrap();

    // JS1: params — db + ...rest.
    assert!(parsed
        .symbols
        .iter()
        .any(|s| s.kind == SymbolKind::Parameter && s.name == "db"));
    assert!(parsed
        .symbols
        .iter()
        .any(|s| s.kind == SymbolKind::Parameter && s.name == "rest"));

    // JS2: imports.
    let deps = parsed.file_deps.expect("file deps missing");
    let mut pairs: Vec<(&str, &str)> = deps
        .imports
        .iter()
        .map(|i| (i.alias.as_str(), i.path.as_str()))
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![("D", "./util.js"), ("h", "./util.js"), ("ns", "lib")]
    );

    // JS3: extends → impl_trait.
    let page = parsed.symbols.iter().find(|s| s.name == "Page").unwrap();
    assert_eq!(page.impl_trait.as_deref(), Some("Base"));

    // JS4: fn-value assignment.
    assert!(parsed
        .fn_ptr_assigns
        .iter()
        .any(|a| a.var == "handler" && a.target == "process"));
    // destructuring KHÔNG thành fn-value.
    assert!(!parsed.fn_ptr_assigns.iter().any(|a| a.var == "a"));
}

#[test]
fn ts_typed_params_extracted() {
    let src = r#"
interface Handler {
    handle(req: Request): Promise<void>;
}

function run(db: Database, limit?: number): void {
    db.query();
}
"#;
    let parsed = codegraph_extract::languages::typescript::TypeScriptParser
        .parse_file("t.ts", src)
        .unwrap();
    let db = parsed
        .symbols
        .iter()
        .find(|s| s.name == "db" && s.kind == SymbolKind::Parameter)
        .expect("ts param db missing");
    assert_eq!(db.type_name.as_deref(), Some("Database"));
    let limit = parsed
        .symbols
        .iter()
        .find(|s| s.name == "limit" && s.kind == SymbolKind::Parameter)
        .expect("ts optional param missing");
    assert_eq!(limit.type_name.as_deref(), Some("number"));
    // interface method (method_signature) scope vào interface.
    let iface = parsed.symbols.iter().find(|s| s.name == "Handler").unwrap();
    assert!(parsed
        .symbols
        .iter()
        .any(|s| s.name == "handle" && s.kind == SymbolKind::Method && s.scope_id == iface.id));
}

#[test]
fn php_supers_sig_use_deps() {
    let src = r#"
<?php
namespace App;

use App\Models\User;
use App\Models\Order as O;

class CardPayment extends BasePayment implements Payment, JsonSerializable {
    public function pay(int $amount, string $currency): void {}
}
"#;
    let parsed = PhpParser.parse_file("P.php", src).unwrap();
    for s in &parsed.symbols {
        eprintln!("PHPDBG {} {:?} line={}", s.name, s.kind, s.line);
    }

    let cls = parsed
        .symbols
        .iter()
        .find(|s| s.name == "CardPayment")
        .unwrap();
    let supers = cls.impl_trait.as_deref().unwrap();
    assert!(supers.starts_with("BasePayment"), "supers: {supers}");
    assert!(supers.contains("Payment"), "supers: {supers}");

    let _pay = parsed
        .symbols
        .iter()
        .find(|s| s.name == "pay")
        .expect("pay missing");

    let deps = parsed.file_deps.expect("use deps missing");
    let mut pairs: Vec<(&str, &str)> = deps
        .imports
        .iter()
        .map(|i| (i.alias.as_str(), i.path.as_str()))
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![("O", "App\\Models\\Order"), ("User", "App\\Models\\User")]
    );
}

#[test]
fn ruby_params_superclass_require() {
    let src = r#"
require 'net/http'
require_relative 'orders/helper'

class FileRepo < BaseRepo
  def save(record, force: false)
    persist(record)
  end

  def fetch(*ids)
  end
end
"#;
    let parsed = RubyParser.parse_file("r.rb", src).unwrap();

    // Params: save(record, force: false) + fetch(*ids).
    for name in ["record", "force", "ids"] {
        assert!(
            parsed
                .symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Parameter && s.name == name),
            "param {name} missing"
        );
    }

    // Superclass → impl_trait (fan-out).
    let cls = parsed
        .symbols
        .iter()
        .find(|s| s.name == "FileRepo")
        .unwrap();
    assert_eq!(cls.impl_trait.as_deref(), Some("BaseRepo"));

    // require deps.
    let deps = parsed.file_deps.expect("require deps missing");
    let mut pairs: Vec<(&str, &str)> = deps
        .imports
        .iter()
        .map(|i| (i.alias.as_str(), i.path.as_str()))
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![("helper", "orders/helper"), ("http", "net/http")]
    );
}

#[test]
fn lua_params_require_fn_value() {
    let src = r#"
local json = require('dkjson')
local utils = require('utils')

local M = {}

function M.run(db, opts)
    handler = process
    handler(db)
    return db
end

return M
"#;
    let parsed = LuaParser.parse_file("m.lua", src).unwrap();

    // Params.
    for name in ["db", "opts"] {
        assert!(
            parsed
                .symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Parameter && s.name == name),
            "param {name} missing"
        );
    }

    // require deps.
    let deps = parsed.file_deps.expect("require deps missing");
    let mut pairs: Vec<(&str, &str)> = deps
        .imports
        .iter()
        .map(|i| (i.alias.as_str(), i.path.as_str()))
        .collect();
    pairs.sort();
    assert_eq!(pairs, vec![("json", "dkjson"), ("utils", "utils")]);

    // Fn-value.
    assert!(parsed
        .fn_ptr_assigns
        .iter()
        .any(|a| a.var == "handler" && a.target == "process"));
}

#[test]
fn scala_supers_sig_imports() {
    let src = r#"
package orders

import scala.collection.mutable
import orders.repo.{BaseRepo, UserRepo => UR}

class CardRepo extends BaseRepo with Logging {
  def pay(amount: Int, currency: String): Unit = {}
}
"#;
    let parsed = ScalaParser.parse_file("S.scala", src).unwrap();

    // Supers: extends B with T.
    let cls = parsed
        .symbols
        .iter()
        .find(|s| s.name == "CardRepo")
        .unwrap();
    let supers = cls.impl_trait.as_deref().expect("supers missing");
    eprintln!("SCALADBG supers={supers}");
    assert!(supers.contains("BaseRepo"), "supers: {supers}");
    assert!(supers.contains("Logging"), "supers: {supers}");

    // Sig key.
    let pay = parsed.symbols.iter().find(|s| s.name == "pay").unwrap();
    assert_eq!(pay.sig_key.as_deref(), Some("(Int,String)(Unit)"));

    // Imports: đơn + selector + alias.
    let deps = parsed.file_deps.expect("imports missing");
    let mut pairs: Vec<(&str, &str)> = deps
        .imports
        .iter()
        .map(|i| (i.alias.as_str(), i.path.as_str()))
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("BaseRepo", "orders.repo.BaseRepo"),
            ("UR", "orders.repo.UserRepo"),
            ("mutable", "scala.collection.mutable"),
        ]
    );
}

#[test]
fn fn_value_and_member_assignment_across_languages() {
    // ── Python: identifier + self.attr member ──
    let py = PythonParser
        .parse_file("p.py", "def process(x): pass\n\ndef setup():\n    handler = process\n    self.handler = process\n    other.x = process\n")
        .unwrap();
    let py_vars: Vec<(&str, &str)> = py
        .fn_ptr_assigns
        .iter()
        .map(|a| (a.var.as_str(), a.target.as_str()))
        .collect();
    let mut py_sorted = py_vars.clone();
    py_sorted.sort();
    assert_eq!(
        py_sorted,
        vec![("handler", "process"), ("self.handler", "process")]
    );

    // ── Ruby: method(:sym) — plain `x = process` là lời gọi, không track ──
    let rb = RubyParser
        .parse_file(
            "r.rb",
            "def setup\n  handler = method(:process)\n  other = process\nend\n",
        )
        .unwrap();
    let rb_vars: Vec<(&str, &str)> = rb
        .fn_ptr_assigns
        .iter()
        .map(|a| (a.var.as_str(), a.target.as_str()))
        .collect();
    assert_eq!(rb_vars, vec![("handler", "process")]);

    // ── Java: method reference ──
    let jv = codegraph_extract::languages::java::JavaParser
        .parse_file("J.java", "class A {\n  void setup() {\n    Runnable r = this::run;\n    Supplier<String> s = Foo::bar;\n  }\n}\n")
        .unwrap();
    let mut jv_vars: Vec<(&str, &str)> = jv
        .fn_ptr_assigns
        .iter()
        .map(|a| (a.var.as_str(), a.target.as_str()))
        .collect();
    jv_vars.sort();
    assert_eq!(jv_vars, vec![("r", "run"), ("s", "bar")]);

    // ── C#: delegate identifier ──
    let cs = codegraph_extract::languages::csharp::CSharpParser
        .parse_file(
            "C.cs",
            "class A {\n  void Setup() {\n    Action handler = Process;\n  }\n}\n",
        )
        .unwrap();
    let cs_vars: Vec<(&str, &str)> = cs
        .fn_ptr_assigns
        .iter()
        .map(|a| (a.var.as_str(), a.target.as_str()))
        .collect();
    assert_eq!(cs_vars, vec![("handler", "Process")]);

    // ── JS: member assignment (kể cả prototype) ──
    let js = codegraph_extract::languages::javascript::JavaScriptParser
        .parse_file(
            "j.js",
            "obj.handler = process;\nFoo.prototype.run = runImpl;\nplain = other;\n",
        )
        .unwrap();
    let mut js_vars: Vec<(&str, &str)> = js
        .fn_ptr_assigns
        .iter()
        .map(|a| (a.var.as_str(), a.target.as_str()))
        .collect();
    js_vars.sort();
    assert_eq!(
        js_vars,
        vec![
            ("Foo.prototype.run", "runImpl"),
            ("obj.handler", "process"),
            ("plain", "other"),
        ]
    );

    // ── Lua: field_expression ──
    let lua = LuaParser
        .parse_file("l.lua", "obj.handler = process\nlocal x = 1\n")
        .unwrap();
    let lua_vars: Vec<(&str, &str)> = lua
        .fn_ptr_assigns
        .iter()
        .map(|a| (a.var.as_str(), a.target.as_str()))
        .collect();
    assert_eq!(lua_vars, vec![("obj.handler", "process")]);
}

#[test]
fn await_markers_in_chain() {
    // Python `await` + JS `await_expression` → MARKER_AWAIT trong chain.
    let py = PythonParser
        .parse_file("p.py", "async def f():\n    await g()\n")
        .unwrap();
    assert!(py
        .chains
        .values()
        .any(|c| c.contains(&codegraph_core::MARKER_AWAIT)));

    let js = codegraph_extract::languages::javascript::JavaScriptParser
        .parse_file("j.js", "async function f() { await g(); }\n")
        .unwrap();
    assert!(js
        .chains
        .values()
        .any(|c| c.contains(&codegraph_core::MARKER_AWAIT)));
}

#[test]
fn c_includes_and_extended_fn_ptr() {
    let src = r#"
#include "foo.h"
#include <openssl/ssl.h>

void cb(int x) {}

void setup(void) {
  obj.handler = cb;
  *handler = cb;
  handler = &ns_cb;
}
"#;
    let parsed = codegraph_extract::languages::c::CParser
        .parse_file("s.c", src)
        .unwrap();

    // C1: include graph.
    let deps = parsed.file_deps.expect("include deps missing");
    let mut pairs: Vec<(&str, &str)> = deps
        .imports
        .iter()
        .map(|i| (i.alias.as_str(), i.path.as_str()))
        .collect();
    pairs.sort();
    assert_eq!(pairs, vec![("foo", "foo.h"), ("ssl", "openssl/ssl.h")]);

    // C2: fn-ptr mở rộng — field / deref / address-of.
    let mut assigns: Vec<(&str, &str)> = parsed
        .fn_ptr_assigns
        .iter()
        .map(|a| (a.var.as_str(), a.target.as_str()))
        .collect();
    assigns.sort();
    assert_eq!(
        assigns,
        vec![
            ("handler", "cb"),
            ("handler", "ns_cb"),
            ("obj.handler", "cb")
        ]
    );
}

#[test]
fn cpp_sig_key_and_virtual_declaration() {
    let src = r#"
class Base {
public:
  virtual void run();
};
class D : public Base {
public:
  void run() override;
  void helper();
};
void D::run() {}
"#;
    let parsed = codegraph_extract::languages::cpp::CppParser
        .parse_file("s.cpp", src)
        .unwrap();

    // C3: sig_key — method inline (field_declaration) + out-of-line definition.
    let run_inline = parsed
        .symbols
        .iter()
        .find(|s| s.name == "run" && s.kind == SymbolKind::Method && s.line == 8)
        .expect("inline run missing");
    assert_eq!(run_inline.sig_key.as_deref(), Some("()(void)"));

    // C4 dữ liệu nền: signature chứa virtual/override (filter dùng ở graph).
    let base_run = parsed
        .symbols
        .iter()
        .find(|s| s.name == "run" && s.line == 4)
        .expect("base run decl missing");
    assert!(base_run.signature.as_deref().unwrap().contains("virtual"));
    let d_run = parsed
        .symbols
        .iter()
        .find(|s| s.name == "run" && s.line == 8)
        .unwrap();
    assert!(d_run.signature.as_deref().unwrap().contains("override"));
}
