//! Golden tests: call-chain per-language (marker + callee name), kiểu semgraph.
//!
//! Chain của 1 hàm = `[owner_id, m1, callee, m2, ...]`; assertion dưới đây render
//! phần walk (bỏ owner) thành tên marker (`[LOOP]`, `[IF_TRUE]`, ...) và tên callee.

use codegraph_core::{marker_name, SymbolKind};
use codegraph_extract::registry;

fn walk(lang: &str, src: &str) -> Vec<String> {
    let parser = registry()
        .into_iter()
        .find(|p| p.name() == lang)
        .unwrap_or_else(|| panic!("no parser {lang}"));
    let res = parser.parse_file("golden.test", src).expect("parse");
    // Class-like symbol giờ cũng có chain tối thiểu `[owner]` — golden test này
    // chỉ xét chain của function/method nên lọc theo owner kind.
    let func_owner: std::collections::HashSet<u64> = res
        .symbols
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
        .map(|s| s.id)
        .collect();
    let func_chains: Vec<&Vec<u64>> = res
        .chains
        .iter()
        .filter(|(id, _)| func_owner.contains(id))
        .map(|(_, c)| c)
        .collect();
    assert_eq!(
        func_chains.len(),
        1,
        "{lang}: expected exactly 1 function chain, got {:?}",
        func_chains
    );
    let chain = func_chains[0];
    // Placeholder 0 chưa resolve — render qua CallRecord (position = index trong chain).
    let name_at = |i: usize, id: u64| -> String {
        if let Some(m) = marker_name(id) {
            return format!("[{m}]");
        }
        if id != 0 {
            if let Some(s) = res.symbols.iter().find(|s| s.id == id) {
                return s.name.clone();
            }
        }
        res.calls
            .iter()
            .find(|c| c.position == i)
            .map(|c| c.call_name.clone())
            .unwrap_or_else(|| format!("?{id}"))
    };
    chain
        .iter()
        .enumerate()
        .skip(1) // bỏ owner id ở đầu
        .map(|(i, id)| name_at(i, *id))
        .collect()
}

#[test]
fn python_loop_with_branch_and_return() {
    let c = walk(
        "python",
        r#"
def process(x):
    for i in items:
        if i > 0:
            save(i)
        else:
            skip(i)
    return x
"#,
    );
    assert_eq!(
        c,
        [
            "[LOOP]",
            "[IF_TRUE]",
            "save",
            "[IF_FALSE]",
            "skip",
            "[BRANCH_END]",
            "[LOOP_BACK]",
            "[RETURN]"
        ]
    );
}

#[test]
fn python_try_except_else_finally() {
    let c = walk(
        "python",
        r#"
def process(x):
    try:
        risky(x)
    except ValueError:
        handle(x)
    else:
        ok()
    finally:
        cleanup()
"#,
    );
    assert_eq!(
        c,
        [
            "risky",
            "[IF_TRUE]",
            "handle",
            "[BRANCH_END]",
            "ok",
            "cleanup",
        ]
    );
}

#[test]
fn python_match_cases() {
    let c = walk(
        "python",
        r#"
def process(x):
    match x:
        case 1:
            one()
        case _:
            other()
"#,
    );
    assert_eq!(
        c,
        [
            "[SWITCH_CASE]",
            "one",
            "[SWITCH_END]",
            "[SWITCH_CASE]",
            "other",
            "[SWITCH_END]"
        ]
    );
}

#[test]
fn java_method_call_with_if_else() {
    let c = walk(
        "java",
        r#"
class Foo {
    void M(int x) {
        obj.run(x);
        if (x > 0) {
            this.helper(x);
        } else {
            fallback(x);
        }
    }
}
"#,
    );
    assert_eq!(
        c,
        [
            "obj.run",
            "[IF_TRUE]",
            "this.helper",
            "[IF_FALSE]",
            "fallback",
            "[BRANCH_END]"
        ]
    );
}

#[test]
fn diag_go_terraform_root_main() {
    use codegraph_core::marker_name;
    let src = std::fs::read_to_string("/Users/lap02921/Desktop/Workspace/terraform/main.go")
        .expect("read terraform main.go");
    let parser = registry()
        .into_iter()
        .find(|p| p.name() == "go")
        .expect("go parser");
    let res = parser.parse_file("main.go", &src).expect("parse");
    for s in &res.symbols {
        let chain = res.chains.get(&s.id);
        println!(
            "id={} kind={:?} name={:?} line={} chain={:?}",
            s.id,
            s.kind,
            s.name,
            s.line,
            chain.map(|c| c
                .iter()
                .enumerate()
                .skip(1)
                .map(|(i, id)| if let Some(m) = marker_name(*id) {
                    format!("[{m}]")
                } else if *id == 0 {
                    res.calls
                        .iter()
                        .find(|c2| c2.position == i)
                        .map(|c2| c2.call_name.clone())
                        .unwrap_or_else(|| "?0".into())
                } else {
                    res.symbols
                        .iter()
                        .find(|s2| s2.id == *id)
                        .map(|s2| s2.name.clone())
                        .unwrap_or_else(|| format!("?{id}"))
                })
                .collect::<Vec<_>>())
        );
    }
    assert!(
        res.symbols.iter().any(|s| s.name == "main"),
        "no main symbol"
    );
}

#[test]
fn go_switch_and_loop() {
    let c = walk(
        "go",
        r#"
package main

func process(u *User) {
    switch u.Name {
    case "a":
        fmt.Println("a")
    default:
        fmt.Println("other")
    }
    for i := 0; i < 10; i++ {
        save(i)
    }
}
"#,
    );
    assert_eq!(
        c,
        [
            "[SWITCH_CASE]",
            "fmt.Println",
            "[SWITCH_END]",
            "[SWITCH_CASE]",
            "fmt.Println",
            "[SWITCH_END]",
            "[LOOP]",
            "save",
            "[LOOP_BACK]",
        ]
    );
}

#[test]
fn ruby_elsif_chain() {
    let c = walk(
        "ruby",
        r#"
def process(x)
  if x > 0
    validate(x)
  elsif x < 0
    warn(x)
  else
    fail(x)
  end
end
"#,
    );
    assert_eq!(
        c,
        [
            "[IF_TRUE]",
            "validate",
            "[IF_TRUE]",
            "warn",
            "fail",
            "[BRANCH_END]",
            "[BRANCH_END]"
        ]
    );
}

#[test]
fn cpp_loop_with_return() {
    let c = walk(
        "cpp",
        r#"
int add(int a, int b) {
    for (int i = 0; i < 10; i++) {
        if (i > a) {
            return compute(i);
        }
    }
    return b;
}
"#,
    );
    assert_eq!(
        c,
        [
            "[LOOP]",
            "[IF_TRUE]",
            "[RETURN]",
            "compute",
            "[BRANCH_END]",
            "[LOOP_BACK]",
            "[RETURN]",
        ]
    );
}

#[test]
fn swift_loop_switch_return() {
    let c = walk(
        "swift",
        r#"
func process(x: Int) -> Int {
    for i in 0..<10 {
        save(i)
    }
    switch x {
    case 1:
        run(1)
    default:
        stop()
    }
    return x
}
"#,
    );
    assert_eq!(
        c,
        [
            "[LOOP]",
            "save",
            "[LOOP_BACK]",
            "[SWITCH_CASE]",
            "run",
            "[SWITCH_END]",
            "[SWITCH_CASE]",
            "stop",
            "[SWITCH_END]",
            "[RETURN]",
        ]
    );
}

#[test]
fn js_loop_switch_with_break() {
    let c = walk(
        "javascript",
        r#"
function f(y) {
    for (const i of arr) {
        qux(i);
    }
    switch (y) {
        case 1: one(); break;
        default: two();
    }
    return y;
}
"#,
    );
    assert_eq!(
        c,
        [
            "[LOOP]",
            "qux",
            "[LOOP_BACK]",
            "[SWITCH_CASE]",
            "one",
            "[BREAK]",
            "[SWITCH_END]",
            "[SWITCH_CASE]",
            "two",
            "[SWITCH_END]",
            "[RETURN]",
        ]
    );
}

#[test]
fn ts_try_catch() {
    let c = walk(
        "typescript",
        r#"
class Service {
    async run(id: string): Promise<void> {
        try {
            await this.repo.find(id);
        } catch (e) {
            log("missing");
        }
    }
}
"#,
    );
    assert_eq!(c, ["this.repo.find", "[IF_TRUE]", "log", "[BRANCH_END]"]);
}

#[test]
fn rust_match_expression() {
    let c = walk(
        "rust",
        r#"
fn f(x: i32) -> i32 {
    match x {
        1 => one(),
        _ => other(),
    }
    return x;
}
"#,
    );
    assert_eq!(
        c,
        [
            "[SWITCH_CASE]",
            "one",
            "[SWITCH_END]",
            "[SWITCH_CASE]",
            "other",
            "[SWITCH_END]",
            "[RETURN]"
        ]
    );
}

#[test]
fn csharp_switch_sections() {
    let c = walk(
        "csharp",
        r#"
class Foo {
    void M() {
        switch (x) {
            case 1: a(); break;
            default: b();
        }
    }
}
"#,
    );
    assert_eq!(
        c,
        [
            "[SWITCH_CASE]",
            "a",
            "[BREAK]",
            "[SWITCH_END]",
            "[SWITCH_CASE]",
            "b",
            "[SWITCH_END]"
        ]
    );
}

#[test]
fn lua_if_for_return() {
    let c = walk(
        "lua",
        r#"
local function process(x)
  if x > 0 then
    validate(x)
  else
    fail()
  end
  for i = 1, 10 do
    save(i)
  end
  return nil
end
"#,
    );
    assert_eq!(
        c,
        [
            "[IF_TRUE]",
            "validate",
            "[IF_FALSE]",
            "fail",
            "[BRANCH_END]",
            "[LOOP]",
            "save",
            "[LOOP_BACK]",
            "[RETURN]"
        ]
    );
}

#[test]
fn php_foreach_member_call() {
    let c = walk(
        "php",
        r#"
<?php
function process($x) {
    foreach ($arr as $i) {
        save($i);
    }
    $obj->method(1);
    self::run(2);
    return $x;
}
"#,
    );
    assert_eq!(
        c,
        [
            "[LOOP]",
            "save",
            "[LOOP_BACK]",
            "obj.method",
            "self.run",
            "[RETURN]"
        ]
    );
}

#[test]
fn scala_match_expression() {
    let c = walk(
        "scala",
        r#"
def f(x: Int) = {
  x match {
    case 1 => one()
    case _ => other()
  }
  return x
}
"#,
    );
    assert_eq!(
        c,
        [
            "[SWITCH_CASE]",
            "one",
            "[SWITCH_END]",
            "[SWITCH_CASE]",
            "other",
            "[SWITCH_END]",
            "[RETURN]"
        ]
    );
}

#[test]
fn c_if_else_return() {
    let c = walk(
        "c",
        r#"
int add(int a, int b) {
    if (a > b) {
        return compute(a);
    } else {
        return b;
    }
}
"#,
    );
    assert_eq!(
        c,
        [
            "[IF_TRUE]",
            "[RETURN]",
            "compute",
            "[IF_FALSE]",
            "[RETURN]",
            "[BRANCH_END]"
        ]
    );
}

/// Calls TRONG condition giờ được emit vào chain: `if (a && b(c()))` → sau
/// `[IF_TRUE]` có `b` rồi `c` (call trong đối số của `b`), rồi mới tới body `d`.
/// `a` là identifier trần (không có parens) — đúng là không phải call. Loop
/// condition cũng được capture (`while` cùng cấu trúc).
#[test]
fn c_calls_in_conditions_captured_in_chain() {
    let c = walk(
        "c",
        r#"
int f(int x) {
    if (a && b(c())) { d(); }
    while (a && b(c())) { d(); }
    return x;
}
"#,
    );
    assert_eq!(
        c,
        [
            "[IF_TRUE]",
            "b",
            "c",
            "d",
            "[BRANCH_END]",
            "[LOOP]",
            "b",
            "c",
            "d",
            "[LOOP_BACK]",
            "[RETURN]",
        ]
    );
}

/// do-while: condition chạy SAU body → emit sau body, trước `[LOOP_BACK]`.
#[test]
fn c_do_while_condition_after_body() {
    let c = walk(
        "c",
        r#"
int f(int x) {
    do { e(); } while (a() && b(c()));
    return x;
}
"#,
    );
    assert_eq!(c, ["[LOOP]", "e", "a", "b", "c", "[LOOP_BACK]", "[RETURN]"]);
}

/// Text condition của `if` được giữ làm metadata (CallRecord.condition của call
/// trong nhánh) — giờ loop cũng giữ text condition của mình.
#[test]
fn c_condition_text_captured_as_call_metadata() {
    let parser = registry()
        .into_iter()
        .find(|p| p.name() == "c")
        .expect("c parser");
    let res = parser
        .parse_file(
            "golden.test",
            "int f(int x) {\n    if (a() && b(c())) { d(); }\n    while (a() && b(c())) { d(); }\n    return x;\n}\n",
        )
        .expect("parse");
    let d_if = res
        .calls
        .iter()
        .find(|c| c.call_name == "d" && c.line == 2)
        .expect("d call in if");
    assert_eq!(d_if.condition.as_deref(), Some("(a() && b(c()))"));
    // Condition call của if cũng mang text condition.
    let a_if = res
        .calls
        .iter()
        .find(|c| c.call_name == "a")
        .expect("a call");
    assert_eq!(a_if.condition.as_deref(), Some("(a() && b(c()))"));
    // Loop body call giờ mang text condition của loop.
    let d_while = res
        .calls
        .iter()
        .find(|c| c.call_name == "d" && c.line == 3)
        .expect("d call in while");
    assert_eq!(d_while.condition.as_deref(), Some("(a() && b(c()))"));
}

/// Cùng hành vi qua python (and/or trong condition) — spec khác, logic chung.
#[test]
fn python_calls_in_conditions_captured_in_chain() {
    let c = walk(
        "python",
        r#"
def f(x):
    if a() and b(c()):
        d()
    while a() and b(c()):
        d()
    return x
"#,
    );
    assert_eq!(
        c,
        [
            "[IF_TRUE]",
            "a",
            "b",
            "c",
            "d",
            "[BRANCH_END]",
            "[LOOP]",
            "a",
            "b",
            "c",
            "d",
            "[LOOP_BACK]",
            "[RETURN]",
        ]
    );
}

/// Go `for cond { }` (dạng while) — condition calls capture trong [LOOP].
#[test]
fn go_loop_condition_calls_captured() {
    let c = walk(
        "go",
        r#"
package main
func f() {
    for a() && b(c()) {
        d()
    }
    return
}
"#,
    );
    assert_eq!(c, ["[LOOP]", "a", "b", "c", "d", "[LOOP_BACK]", "[RETURN]"]);
}

/// Switch discriminant (`switch (getType(x))`) cũng vào chain trước các case.
#[test]
fn java_switch_discriminant_call_captured() {
    let c = walk(
        "java",
        r#"
class Foo {
    void M(int x) {
        switch (getType(x)) {
            case 1: one(); break;
            default: other();
        }
    }
}
"#,
    );
    assert_eq!(
        c,
        [
            "getType",
            "[SWITCH_CASE]",
            "one",
            "[BREAK]",
            "[SWITCH_END]",
            "[SWITCH_CASE]",
            "other",
            "[SWITCH_END]",
        ]
    );
}

/// Java chain call rồi return thẳng: cả call ngoài (`a.run(abc.class).exec`)
/// lẫn call trong (`a.run`) đều được capture; class literal `abc.class` không
/// phải call (đúng). Cả hai là placeholder `0` trong chain thô — phân biệt
/// bằng CallRecord.position (thứ tự emit: ngoài trước, trong sau).
#[test]
fn java_chained_call_then_return() {
    let c = walk(
        "java",
        r#"
class Foo {
    Object M() {
        return a.run(abc.class).exec();
    }
}
"#,
    );
    assert_eq!(c, ["[RETURN]", "a.run(abc.class).exec", "a.run"]);
}

/// Bug A: string-literal case labels (`case 'optimize_text':`) — dispatch key
/// theo chuỗi không phải identifier call. Emit thành call-name ảo (placeholder
/// `0` + CallRecord) để `search_by_call` index được. `default` không có value →
/// không emit.
#[test]
fn ts_switch_string_case_labels_captured_as_call_names() {
    let c = walk(
        "typescript",
        r#"
function dispatch(name: string): number {
    switch (name) {
        case 'optimize_text': return 1;
        case "get_cached": return 2;
        default: return 0;
    }
}
"#,
    );
    assert_eq!(
        c,
        [
            "[SWITCH_CASE]",
            "optimize_text",
            "[RETURN]",
            "[SWITCH_END]",
            "[SWITCH_CASE]",
            "get_cached",
            "[RETURN]",
            "[SWITCH_END]",
            "[SWITCH_CASE]",
            "[RETURN]",
            "[SWITCH_END]",
        ]
    );
}

/// Bug B: class có chain tối thiểu `[class_id]` — `flow`/`search_flow` không bị
/// "chain not found" (trước đây chỉ có edge function→class từ phía caller).
/// Methods của class vẫn có chain riêng.
#[test]
fn ts_class_has_minimal_chain() {
    let parser = registry()
        .into_iter()
        .find(|p| p.name() == "typescript")
        .expect("ts parser");
    let res = parser
        .parse_file(
            "golden.test",
            r#"
class Store {
    save(k: string): void {}
    get(k: string): string { return ""; }
}
"#,
        )
        .expect("parse");
    let class_id = res
        .symbols
        .iter()
        .find(|s| s.name == "Store" && matches!(s.kind, SymbolKind::Class))
        .expect("Store class symbol")
        .id;
    let chain = res.chains.get(&class_id).expect("class chain");
    assert_eq!(chain, &vec![class_id]);
}

/// Cùng tên method (`save`) trong 2 class khác nhau — `func_index` key theo
/// `(name, line)` nên mỗi method có id riêng (đúng scope_id của class nó); mỗi
/// method và mỗi class đều có chain riêng, không hoà trộn.
#[test]
fn ts_duplicate_method_name_across_two_classes() {
    let parser = registry()
        .into_iter()
        .find(|p| p.name() == "typescript")
        .expect("ts parser");
    let res = parser
        .parse_file(
            "golden.test",
            r#"
class ServiceA {
    save(k: string): void {}
    load(k: string): string { return ""; }
}
class ServiceB {
    save(k: string): void {}
    load(k: string): string { return ""; }
}
"#,
        )
        .expect("parse");

    let classes: Vec<&codegraph_core::Symbol> = res
        .symbols
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Class))
        .collect();
    assert_eq!(classes.len(), 2, "exactly 2 classes");

    // Mỗi method `save`/`load` có id riêng và thuộc đúng class của nó.
    let saves: Vec<&codegraph_core::Symbol> = res
        .symbols
        .iter()
        .filter(|s| s.name == "save" && matches!(s.kind, SymbolKind::Method))
        .collect();
    assert_eq!(saves.len(), 2);
    assert_ne!(saves[0].id, saves[1].id);
    assert_ne!(saves[0].scope_id, saves[1].scope_id);
    assert!([classes[0].id, classes[1].id].contains(&saves[0].scope_id));
    assert!([classes[0].id, classes[1].id].contains(&saves[1].scope_id));

    // Mỗi method đều có chain riêng bắt đầu bằng id chính nó.
    for s in saves {
        let chain = res.chains.get(&s.id).expect("method chain");
        assert_eq!(chain.first(), Some(&s.id));
    }
    // Mỗi class có chain tối thiểu `[class_id]` riêng biệt.
    for c in &classes {
        let chain = res.chains.get(&c.id).expect("class chain");
        assert_eq!(chain, &vec![c.id]);
    }
    assert_ne!(classes[0].id, classes[1].id);
}

/// Cùng tên class (`Registry`) khai báo 2 lần ở 2 line khác nhau — key
/// `(name, line)` phân biệt được; mỗi class có chain riêng.
#[test]
fn ts_duplicate_class_name_different_lines() {
    let parser = registry()
        .into_iter()
        .find(|p| p.name() == "typescript")
        .expect("ts parser");
    let res = parser
        .parse_file(
            "golden.test",
            r#"
class Registry {
    put(k: string): void {}
}
class Registry {
    get(k: string): void {}
}
"#,
        )
        .expect("parse");
    let registries: Vec<&codegraph_core::Symbol> = res
        .symbols
        .iter()
        .filter(|s| s.name == "Registry" && matches!(s.kind, SymbolKind::Class))
        .collect();
    assert_eq!(registries.len(), 2, "both Registry declarations indexed");
    assert_ne!(registries[0].id, registries[1].id);
    let mut chains: Vec<u64> = registries
        .iter()
        .filter_map(|c| res.chains.get(&c.id))
        .map(|chain| chain[0])
        .collect();
    chains.sort_unstable();
    let mut expected = vec![registries[0].id, registries[1].id];
    expected.sort_unstable();
    assert_eq!(chains, expected);
}

/// Mirror `OptimizationStorageTool.run` (Bug A): dispatch theo string-literal
/// operation (`case 'store'` / `case 'retrieve'`) — case label vừa được emit
/// thành call-name ảo, vừa không che member call thật bên trong body
/// (`s.save`/`s.get`) + `break`. `default` không có value → không emit call.
#[test]
fn ts_switch_string_operation_dispatch_with_member_calls() {
    let c = walk(
        "typescript",
        r#"
function runStorage(op: string, s: Store): void {
    switch (op) {
        case 'store': s.save(k); break;
        case 'retrieve': s.get(k); break;
        default: break;
    }
}
"#,
    );
    assert_eq!(
        c,
        [
            "[SWITCH_CASE]",
            "store",
            "s.save",
            "[BREAK]",
            "[SWITCH_END]",
            "[SWITCH_CASE]",
            "retrieve",
            "s.get",
            "[BREAK]",
            "[SWITCH_END]",
            "[SWITCH_CASE]",
            "[BREAK]",
            "[SWITCH_END]",
        ]
    );
}
