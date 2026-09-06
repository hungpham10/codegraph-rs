//! Unit test cho extract mapping với mock R2Client — không cần r2 thật.

use codegraph_binary::config::AnalysisDepth;
use codegraph_binary::extract::{extract_binary_with_session, R2Client};
use codegraph_core::{SymbolKind, MARKER_IF_TRUE, MARKER_RETURN};
use codegraph_graph::ParseResult;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Mock r2 client trả fixture JSON theo command.
struct MockR2 {
    responses: HashMap<String, Value>,
}

impl MockR2 {
    fn new() -> Self {
        let mut responses = HashMap::new();
        // aflj: main + helper + PLT stub của puts
        responses.insert(
            "aflj".to_string(),
            json!([
                {"offset": 4198496, "name": "main", "size": 64, "cc": 1.0, "calltype": "cdecl"},
                {"offset": 4198560, "name": "fcn.00401160", "size": 32, "cc": 2.0},
                {"offset": 4196112, "name": "sym.imp.LIBC.so.6_puts", "size": 16}
            ]),
        );
        // iij: 1 import puts
        responses.insert(
            "iij".to_string(),
            json!([
                {"import": "puts", "bind": "NONE", "type": "FUNC", "lib": "LIBC.so.6", "plt": 4196112}
            ]),
        );
        // izj: 1 string
        responses.insert(
            "izj".to_string(),
            json!([
                {"vaddr": 4202496, "paddr": 8192, "size": 14, "type": "ascii", "string": "hello world\n"}
            ]),
        );
        // agCj: main → helper, main → puts(plt)
        responses.insert(
            "agCj".to_string(),
            json!({"edges": [
                {"from": 4198496, "to": 4198560},
                {"from": 4198496, "to": 4196112}
            ]}),
        );
        // pdfj main: call + return + branch
        responses.insert(
            "pdfj @ 4198496".to_string(),
            json!({
                "name": "main", "offset": 4198496, "size": 64,
                "ops": [
                    {"offset": 4198496, "type": "push", "disasm": "push rbp"},
                    {"offset": 4198500, "type": "cjmp", "jump": 4198520, "fail": 4198512, "disasm": "je 0x401018"},
                    {"offset": 4198504, "type": "call", "jump": 4196112, "disasm": "call sym.imp.LIBC.so.6_puts"},
                    {"offset": 4198510, "type": "jmp", "jump": 4198496, "disasm": "jmp 0x401000"},
                    {"offset": 4198560, "type": "ret", "disasm": "ret"}
                ]
            }),
        );
        Self { responses }
    }
}

impl R2Client for MockR2 {
    fn cmd(&mut self, cmd: &str) -> Result<String, codegraph_core::Error> {
        Ok(self
            .responses
            .get(cmd)
            .map(|v| v.to_string())
            .unwrap_or_default())
    }

    fn cmdj(&mut self, cmd: &str) -> Result<Value, codegraph_core::Error> {
        Ok(self.responses.get(cmd).cloned().unwrap_or(Value::Null))
    }
}

#[test]
fn extract_maps_functions_imports_strings() {
    let dir = tempfile::tempdir().unwrap();
    let bin_path = dir.path().join("app");
    std::fs::write(&bin_path, b"\x7fELF\x02\x01\x01fake").unwrap();

    let mut mock = MockR2::new();
    let result: ParseResult =
        extract_binary_with_session(&bin_path, &mut mock, AnalysisDepth::Aaa, false).unwrap();

    assert_eq!(result.language, "binary");
    assert_eq!(result.path, bin_path.to_str().unwrap());

    // functions + imports + strings
    let funcs: Vec<_> = result
        .symbols
        .iter()
        .filter(|s| {
            s.kind == SymbolKind::Function
                && !s
                    .signature
                    .as_deref()
                    .is_some_and(|sig| sig.starts_with("import"))
        })
        .collect();
    assert_eq!(funcs.len(), 2, "2 hàm thật (main + fcn), PLT bị bỏ qua");

    let imports: Vec<_> = result
        .symbols
        .iter()
        .filter(|s| s.annotations.iter().any(|a| a.name == "import"))
        .collect();
    assert_eq!(imports.len(), 1);
    assert_eq!(imports[0].name, "puts");
    assert_eq!(imports[0].doc.as_deref(), Some("LIBC.so.6"));

    let strings: Vec<_> = result
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Constant)
        .collect();
    assert_eq!(strings.len(), 1);
    assert!(strings[0].name.starts_with("str:"));
    assert_eq!(strings[0].doc.as_deref(), Some("hello world\n"));
}

#[test]
fn extract_resolves_calls_via_callgraph() {
    let dir = tempfile::tempdir().unwrap();
    let bin_path = dir.path().join("app");
    std::fs::write(&bin_path, b"\x7fELF\x02\x01\x01fake").unwrap();

    let mut mock = MockR2::new();
    let result =
        extract_binary_with_session(&bin_path, &mut mock, AnalysisDepth::Aaa, false).unwrap();

    // main có 2 call: helper (fcn) + puts (import)
    let main = result.symbols.iter().find(|s| s.name == "main").unwrap();
    let chain = result.chains.get(&main.id).unwrap();
    assert_eq!(chain[0], main.id);
    assert_eq!(chain.len(), 3, "main → 2 placeholder call");

    let main_calls: Vec<_> = result
        .calls
        .iter()
        .filter(|c| c.caller_id == main.id)
        .collect();
    assert_eq!(main_calls.len(), 2);
    let names: Vec<_> = main_calls.iter().map(|c| c.call_name.as_str()).collect();
    assert!(
        names.contains(&"fcn.00401160"),
        "call nội bộ theo name r2: {names:?}"
    );
    assert!(
        names.contains(&"puts"),
        "call import theo tên sạch: {names:?}"
    );
}

#[test]
fn extract_with_cfg_markers() {
    let dir = tempfile::tempdir().unwrap();
    let bin_path = dir.path().join("app");
    std::fs::write(&bin_path, b"\x7fELF\x02\x01\x01fake").unwrap();

    let mut mock = MockR2::new();
    let result =
        extract_binary_with_session(&bin_path, &mut mock, AnalysisDepth::Aaa, true).unwrap();

    let main = result.symbols.iter().find(|s| s.name == "main").unwrap();
    let chain = result.chains.get(&main.id).unwrap();
    // chain: [main, IF_TRUE, call(puts placeholder), ...]
    assert!(chain.contains(&MARKER_IF_TRUE), "cjmp → IF_TRUE: {chain:?}");
    assert!(chain.contains(&MARKER_RETURN), "ret → RETURN: {chain:?}");
    // call tới import trong chain-with-cfg dùng plt addr → "puts"
    let main_calls: Vec<_> = result
        .calls
        .iter()
        .filter(|c| c.caller_id == main.id)
        .collect();
    assert!(main_calls.iter().any(|c| c.call_name == "puts"));
}

#[test]
fn depth_commands() {
    assert_eq!(AnalysisDepth::Aaa.command(), "aaa");
    assert_eq!(AnalysisDepth::Fast.command(), "af; aar; aac");
}

// Integration thật với r2 — bỏ qua nếu không có r2 trong PATH.
#[test]
#[ignore = "cần radare2 trong PATH"]
fn integration_with_real_r2() {
    if !codegraph_binary::r2_available() {
        eprintln!("r2 không có trong PATH — bỏ qua");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let bin_path = dir.path().join("hello");
    // /bin/ls là ELF/Mach-O có sẵn trên hệ thống
    std::fs::copy("/bin/ls", &bin_path).unwrap();

    let result = codegraph_binary::extract_binary(&bin_path, AnalysisDepth::Fast, true).unwrap();
    assert!(
        !result.symbols.is_empty(),
        "phải tìm được symbol trong /bin/ls"
    );
}
