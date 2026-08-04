//! Smoke test: parse source từ stdin theo tên ngôn ngữ, in symbols + chains + calls.
//!
//! ```sh
//! printf 'def f(x):\n    if x:\n        g()\n' | cargo run -q -p codegraph-extract --example smoke -- python
//! ```

use codegraph_extract::registry;
use std::io::Read;

fn main() {
    let lang = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: smoke <lang> < input");
        std::process::exit(1);
    });
    let mut src = String::new();
    std::io::stdin().read_to_string(&mut src).expect("read stdin");

    let parser = registry()
        .into_iter()
        .find(|p| p.name() == lang)
        .unwrap_or_else(|| panic!("no parser for {lang}"));

    let res = parser.parse_file("smoke.test", &src).expect("parse");
    println!("== symbols ({}) ==", res.symbols.len());
    for s in &res.symbols {
        println!(
            "  {:<4} {:<28} {:?} {:?} scope={} L{}",
            s.id,
            s.name,
            s.kind,
            s.scope,
            s.scope_id,
            s.line
        );
    }
    println!("== chains ({}) ==", res.chains.len());
    for (func_id, chain) in &res.chains {
        let names: Vec<String> = chain
            .iter()
            .map(|id| match codegraph_core::marker_name(*id) {
                Some(m) => format!("[{m}]"),
                None => res
                    .symbols
                    .iter()
                    .find(|s| s.id == *id)
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| format!("?{id}")),
            })
            .collect();
        let fname = res
            .symbols
            .iter()
            .find(|s| s.id == *func_id)
            .map(|s| s.name.clone())
            .unwrap_or_default();
        println!("  {fname} [{func_id}] -> {}", names.join(" "));
    }
    println!("== calls ({}) ==", res.calls.len());
    for c in &res.calls {
        println!(
            "  L{:<3} {} (effect={:?})",
            c.line,
            c.call_name,
            c.effect
        );
    }
}
