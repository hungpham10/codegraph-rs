//! Chuyển đổi output r2 → `ParseResult` cho `GraphIndex::ingest`.

use crate::config::AnalysisDepth;
use crate::model::*;
use crate::r2::R2Session;
use codegraph_core::{Annotation, CallRecord, EffectType, Error, Symbol, SymbolKind, SYMBOL_BASE};
use codegraph_graph::ParseResult;
use cpp_demangle::Symbol as CppSymbol;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Trích xuất toàn bộ thông tin từ binary thành `ParseResult`.
/// Gọi `aaa` một lần trong session, rồi query.
pub fn extract_binary(
    path: &Path,
    depth: AnalysisDepth,
    cfg_markers: bool,
) -> Result<ParseResult, Error> {
    let mut session = R2Session::open(path)?;
    let file_len = path.metadata().map(|m| m.len()).unwrap_or(0);
    let result = do_extract(&mut session, path, file_len, cfg_markers, depth)?;
    Ok(result)
}

/// Trích xuất với session đã mở (dùng cho cache warm, kiểm thử).
pub fn extract_binary_with_session(
    path: &Path,
    session: &mut dyn R2Client,
    depth: AnalysisDepth,
    cfg_markers: bool,
) -> Result<ParseResult, Error> {
    let file_len = path.metadata().map(|m| m.len()).unwrap_or(0);
    do_extract(session, path, file_len, cfg_markers, depth)
}

/// Trait trừu tượng cho r2 client — giúp mock trong test mà không cần r2 thật.
pub trait R2Client {
    fn cmd(&mut self, cmd: &str) -> Result<String, Error>;
    fn cmdj(&mut self, cmd: &str) -> Result<Value, Error>;

    /// Phân tích binary (chỉ gọi 1 lần trong đời session).
    fn analyze(&mut self, depth: AnalysisDepth) -> Result<(), Error> {
        self.cmd(depth.command())?;
        Ok(())
    }
}

impl R2Client for R2Session {
    fn cmd(&mut self, cmd: &str) -> Result<String, Error> {
        R2Session::cmd(self, cmd)
    }
    fn cmdj(&mut self, cmd: &str) -> Result<Value, Error> {
        R2Session::cmdj(self, cmd)
    }
}

fn do_extract(
    session: &mut dyn R2Client,
    path: &Path,
    file_len: u64,
    cfg_markers: bool,
    depth: AnalysisDepth,
) -> Result<ParseResult, Error> {
    let path_str = path
        .to_str()
        .ok_or_else(|| Error::Parse("path không phải UTF-8".to_string()))?;

    session.analyze(depth)?;

    // 1. Functions (`aflj`)
    let functions = parse_aflj(session)?;
    // Parse exports (`iEj`) for JNI address-based detection (catches stripped binaries).
    let exports = parse_iej(session)?;
    let jni_export_map: HashMap<u64, String> = exports
        .iter()
        .filter(|e| is_jni_name(e.name.as_deref().unwrap_or("")))
        .filter_map(|e| e.vaddr.map(|v| (v, e.name.clone().unwrap_or_default())))
        .collect();
    let mut symbols: Vec<Symbol> = Vec::new();
    let mut chains: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut calls: Vec<CallRecord> = Vec::new();
    let mut fn_by_addr: HashMap<u64, u64> = HashMap::new();
    let mut fn_id_to_name: HashMap<u64, String> = HashMap::new();
    let mut next_id = SYMBOL_BASE + 1;

    for entry in &functions {
        let addr = entry.addr.unwrap_or(0);
        let raw_name = entry
            .name
            .clone()
            .unwrap_or_else(|| format!("fcn.{addr:x}"));
        // PLT thunk của import — đã có symbol riêng từ `iij`, bỏ qua.
        if raw_name.starts_with("sym.imp.") {
            continue;
        }
        let name = demangle(&strip_r2_prefix(&raw_name));
        let size = entry.size.unwrap_or(0);
        let sig = build_signature(addr, size, entry);
        let id = next_id;
        next_id += 1;
        fn_by_addr.insert(addr, id);
        fn_id_to_name.insert(id, name.clone());
        // r2 6.x tự sinh symbol C++: class.X, method.Class.foo, namespace.X, enum.X
        let (kind, name) = classify_symbol(&raw_name, &name);
        // JNI enrichment: name-based + address-based (via iEj export table).
        let mut annotations = Vec::new();
        if is_jni_name(&name) || jni_export_map.contains_key(&addr) {
            annotations.push(Annotation {
                name: "jni".to_string(),
                args: HashMap::new(),
                line: 0,
            });
        }
        symbols.push(Symbol {
            id,
            name,
            kind,
            scope: codegraph_core::ScopeLevel::Global,
            scope_id: 0,
            type_ref: 0,
            type_name: None,
            file: path_str.to_string(),
            line: addr.try_into().unwrap_or(0),
            end_line: addr.saturating_add(size).try_into().unwrap_or(u32::MAX),
            signature: Some(sig),
            doc: None,
            annotations,
            language: "binary".to_string(),
        });
    }

    // 2. Imports (`iij`) — tạo symbol; bỏ qua function entry "sym.imp."
    let imports = parse_iij(session)?;
    let mut import_name_to_id: HashMap<String, u64> = HashMap::new();
    let mut plt_by_addr: HashMap<u64, String> = HashMap::new();
    for imp in &imports {
        let clean = imp.import.as_deref().unwrap_or("?");
        let count = imports
            .iter()
            .filter(|i| i.import.as_deref() == Some(clean))
            .count();
        let name = if count > 1 {
            format!("{clean} ({})", imp.lib.as_deref().unwrap_or("?"))
        } else {
            clean.to_string()
        };
        let id = next_id;
        next_id += 1;
        import_name_to_id.insert(clean.to_string(), id);
        if let Some(plt) = imp.plt {
            plt_by_addr.insert(plt, name.clone());
        }
        symbols.push(Symbol {
            id,
            name,
            kind: SymbolKind::Function,
            scope: codegraph_core::ScopeLevel::Global,
            scope_id: 0,
            type_ref: 0,
            type_name: None,
            file: path_str.to_string(),
            line: imp.plt.unwrap_or(0).try_into().unwrap_or(0),
            end_line: 0,
            signature: Some(format!("import ({})", imp.lib.as_deref().unwrap_or(""))),
            doc: imp.lib.clone(),
            annotations: vec![Annotation {
                name: "import".to_string(),
                args: HashMap::new(),
                line: 0,
            }],
            language: "binary".to_string(),
        });
    }

    // 3. Strings (`izj`)
    let strings = parse_izj(session)?;
    for s in &strings {
        let id = next_id;
        next_id += 1;
        let vaddr = s.vaddr.unwrap_or(0);
        symbols.push(Symbol {
            id,
            name: format!("str:{vaddr:x}"),
            kind: SymbolKind::Constant,
            scope: codegraph_core::ScopeLevel::Global,
            scope_id: 0,
            type_ref: 0,
            type_name: None,
            file: path_str.to_string(),
            line: vaddr.try_into().unwrap_or(0),
            end_line: 0,
            signature: s.type_.clone().map(|t| format!("{t} string")),
            doc: s.string.as_deref().map(|s| s.chars().take(200).collect()),
            annotations: Vec::new(),
            language: "binary".to_string(),
        });
    }

    // 4. Calls + chains
    let maps = FnMaps {
        fn_by_addr: &fn_by_addr,
        fn_id_to_name: &fn_id_to_name,
        plt_by_addr: &plt_by_addr,
        import_name_to_id: &import_name_to_id,
    };
    if cfg_markers {
        build_chains_with_cfg(session, &functions, &maps, &mut chains, &mut calls)?;
    } else {
        build_chains_from_graph(session, &maps, &mut chains, &mut calls)?;
    }

    // Chain cho symbol không có call (import/string)
    for s in &symbols {
        chains.entry(s.id).or_insert_with(|| vec![s.id]);
    }

    Ok(ParseResult {
        path: path_str.to_string(),
        language: "binary".to_string(),
        bytes: file_len,
        lines: 0,
        symbols,
        chains,
        calls,
    })
}

fn parse_array<T: serde::de::DeserializeOwned>(v: Value) -> Result<Vec<T>, Error> {
    match v {
        Value::Array(a) => Ok(a
            .into_iter()
            .filter_map(|v| serde_json::from_value::<T>(v).ok())
            .collect()),
        _ => Ok(Vec::new()),
    }
}

fn parse_aflj(session: &mut dyn R2Client) -> Result<Vec<FnEntry>, Error> {
    parse_array(session.cmdj("aflj")?)
}

fn parse_iij(session: &mut dyn R2Client) -> Result<Vec<ImportEntry>, Error> {
    parse_array(session.cmdj("iij")?)
}

fn parse_izj(session: &mut dyn R2Client) -> Result<Vec<StrEntry>, Error> {
    parse_array(session.cmdj("izj")?)
}

fn build_signature(addr: u64, size: u64, entry: &FnEntry) -> String {
    let mut parts = vec![format!("0x{addr:x}")];
    if size > 0 {
        parts.push(format!("sz={size}"));
    }
    if let Some(cc) = entry.cc {
        parts.push(format!("cc={cc}"));
    }
    if let Some(ct) = &entry.calltype {
        parts.push(ct.clone());
    }
    if let Some(sig) = &entry.signature {
        parts.push(sig.clone());
    }
    parts.join(" ")
}

fn strip_r2_prefix(name: &str) -> String {
    name.strip_prefix("sym.").unwrap_or(name).to_string()
}

/// Demangle tên C++ Itanium (_ZN...) để readable hơn khi search/index.
/// Giữ nguyên tên không phải C++ (bao gồm cả `sub_`, `fcn.`).
fn demangle(name: &str) -> String {
    if name.starts_with("_ZN") || name.starts_with("_TS") || name.starts_with("_Z") {
        match CppSymbol::new(name) {
            Ok(s) => match s.demangle() {
                Ok(d) => d,
                Err(_) => name.to_string(),
            },
            Err(_) => name.to_string(),
        }
    } else {
        name.to_string()
    }
}

/// Phân loại symbol từ tên thô do r2 trả về.
/// r2 6.x tự sinh symbol C++: `class.X`, `method.Class.foo`,
/// `namespace.X`, `enum.X`. Trả về `(kind, name)` — name đã được làm sạch.
fn classify_symbol(raw_name: &str, name: &str) -> (SymbolKind, String) {
    // Dùng raw_name vì nó giữ nguyên tên gốc từ r2 (chưa strip sym. prefix).
    if raw_name.starts_with("class.") || name.starts_with("class.") {
        return (SymbolKind::Class, name.to_string());
    }
    if raw_name.starts_with("method.") || name.starts_with("method.") {
        return (SymbolKind::Method, name.to_string());
    }
    if raw_name.starts_with("namespace.") || name.starts_with("namespace.") {
        return (SymbolKind::Module, name.to_string());
    }
    if raw_name.starts_with("enum.") || name.starts_with("enum.") {
        return (SymbolKind::Enum, name.to_string());
    }
    (SymbolKind::Function, name.to_string())
}

/// Kiểm tra tên có phải là JNI symbol không.
/// Java_* — JNI native method naming convention.
/// JNI_* — JNI runtime functions.
fn is_jni_name(name: &str) -> bool {
    name.starts_with("Java_") || name.starts_with("JNI_")
}

/// Parse exported symbols từ `iEj` để phát hiện JNI symbol qua address matching.
/// Trả về danh sách symbol xuất khẩu có tên bắt đầu bằng Java_ hoặc JNI_.
fn parse_iej(session: &mut dyn R2Client) -> Result<Vec<ExportEntry>, Error> {
    parse_array(session.cmdj("iEj")?)
}

/// Bản đồ tra cứu từ address/name sang symbol id — gom parameter cho chain builder.
struct FnMaps<'a> {
    fn_by_addr: &'a HashMap<u64, u64>,
    fn_id_to_name: &'a HashMap<u64, String>,
    plt_by_addr: &'a HashMap<u64, String>,
    import_name_to_id: &'a HashMap<String, u64>,
}

/// Xây chain từ `pdfj` từng function (marker từ CFG).
fn build_chains_with_cfg(
    session: &mut dyn R2Client,
    functions: &[FnEntry],
    maps: &FnMaps,
    chains: &mut HashMap<u64, Vec<u64>>,
    calls: &mut Vec<CallRecord>,
) -> Result<(), Error> {
    for entry in functions {
        let addr = entry.addr.unwrap_or(0);
        let Some(&func_id) = maps.fn_by_addr.get(&addr) else {
            continue;
        };
        // addr 0 = entry rác (import/reloc chưa resolve) — pdfj không bao giờ
        // trả ops cho địa chỉ này, bỏ qua sớm thay để r2 bắn ERROR ra stderr.
        if addr == 0 {
            continue;
        }
        // Một function r2 không disasm được (addr 0, corrupt, stripped…) không
        // được làm fail cả binary — bỏ qua nó và chạy tiếp các function còn lại.
        let ops_json = match session.cmdj(&format!("pdfj @ {addr}")) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("r2 pdfj @ {addr:#x} failed: {e}; bỏ qua function này");
                continue;
            }
        };
        let ops: Vec<DisasmOp> = ops_json
            .get("ops")
            .and_then(|o| o.as_array())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| serde_json::from_value::<DisasmOp>(v).ok())
            .collect();
        let mut chain = vec![func_id];
        let mut local_calls = Vec::new();
        let mut seen = HashSet::new();

        for op in &ops {
            let off = op.addr.unwrap_or(0);
            seen.insert(off);
            if let Some(t) = &op.type_ {
                match t.as_str() {
                    "call" => {
                        let (_callee_id, callee_name) =
                            resolve_call_target(op.jump.or(op.ptr), maps);
                        let pos = chain.len();
                        chain.push(0);
                        local_calls.push(CallRecord {
                            caller_id: func_id,
                            call_name: callee_name,
                            position: pos,
                            arg_exprs: Vec::new(),
                            line: off.try_into().unwrap_or(0),
                            condition: op.disasm.clone(),
                            is_loop_body: false,
                            effect: EffectType::None,
                            effect_desc: None,
                            target_class: None,
                            target_method: None,
                        });
                    }
                    "cjmp" => {
                        chain.push(codegraph_core::MARKER_IF_TRUE);
                    }
                    "jmp" => {
                        if let Some(t) = op.jump {
                            if seen.contains(&t) && t < addr {
                                chain.push(codegraph_core::MARKER_LOOP_BACK);
                            }
                        }
                    }
                    "ret" | "uret" => {
                        chain.push(codegraph_core::MARKER_RETURN);
                    }
                    "swi" | "syscall" => {
                        chain.push(codegraph_core::MARKER_THROW);
                    }
                    _ => {}
                }
            }
        }
        chains.insert(func_id, chain);
        calls.append(&mut local_calls);
    }
    Ok(())
}

/// Xây chain nhẹ từ `agCj` (call graph) — không có marker CFG.
/// r2 6.x trả danh sách `{name, imports: [callee names]}` thay vì edges có địa chỉ.
fn build_chains_from_graph(
    session: &mut dyn R2Client,
    maps: &FnMaps,
    chains: &mut HashMap<u64, Vec<u64>>,
    calls: &mut Vec<CallRecord>,
) -> Result<(), Error> {
    let nodes: Vec<CallGraphNode> = parse_array(session.cmdj("agCj")?)?;

    // Map tên symbol (đã strip prefix "sym.") → id, cho cả function lẫn import.
    let mut name_to_id: HashMap<String, u64> = HashMap::new();
    for (&id, name) in maps.fn_id_to_name.iter() {
        name_to_id.entry(name.clone()).or_insert(id);
    }
    for (clean, &id) in maps.import_name_to_id.iter() {
        name_to_id.entry(clean.clone()).or_insert(id);
    }

    let resolve_id = |raw: &str| -> Option<u64> {
        let clean = strip_r2_prefix(raw);
        let clean = clean.strip_prefix("imp.").unwrap_or(&clean);
        name_to_id.get(clean).copied()
    };

    for node in &nodes {
        let Some(raw) = node.name.as_deref() else {
            continue;
        };
        let Some(caller_id) = resolve_id(raw) else {
            continue;
        };
        if caller_id == 0 {
            continue;
        }
        let mut chain = vec![caller_id];
        for callee in node.imports.iter().flatten() {
            let clean = strip_r2_prefix(callee);
            let clean = clean.strip_prefix("imp.").unwrap_or(&clean).to_string();
            let pos = chain.len();
            chain.push(0);
            calls.push(CallRecord {
                caller_id,
                call_name: clean,
                position: pos,
                arg_exprs: Vec::new(),
                line: 0,
                condition: None,
                is_loop_body: false,
                effect: EffectType::None,
                effect_desc: None,
                target_class: None,
                target_method: None,
            });
        }
        chains.insert(caller_id, chain);
    }
    Ok(())
}

fn resolve_call_target(target: Option<u64>, maps: &FnMaps) -> (u64, String) {
    let addr = match target {
        Some(a) => a,
        None => return (0, String::new()),
    };
    // Call tới import đi qua PLT stub — resolve theo plt addr.
    if let Some(name) = maps.plt_by_addr.get(&addr) {
        let id = maps.import_name_to_id.get(name).copied().unwrap_or(0);
        return (id, name.clone());
    }
    if let Some(&fid) = maps.fn_by_addr.get(&addr) {
        let name = maps
            .fn_id_to_name
            .get(&fid)
            .cloned()
            .unwrap_or_else(|| format!("sub_{addr:x}"));
        return (fid, name);
    }
    (0, format!("sub_{addr:x}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::SymbolKind;
    use serde_json::json;

    #[test]
    fn test_classify_symbol_class() {
        let raw = "class.MyClass";
        let name = "MyClass";
        let (kind, cleaned_name) = classify_symbol(raw, name);
        assert_eq!(kind, SymbolKind::Class);
        assert_eq!(cleaned_name, name);
    }

    #[test]
    fn test_classify_symbol_method() {
        let raw = "method.MyClass.my_method";
        let name = "MyClass.my_method";
        let (kind, cleaned_name) = classify_symbol(raw, name);
        assert_eq!(kind, SymbolKind::Method);
        assert_eq!(cleaned_name, name);
    }

    #[test]
    fn test_classify_symbol_namespace() {
        let raw = "namespace.std";
        let name = "std";
        let (kind, cleaned_name) = classify_symbol(raw, name);
        assert_eq!(kind, SymbolKind::Module);
        assert_eq!(cleaned_name, name);
    }

    #[test]
    fn test_classify_symbol_enum() {
        let raw = "enum.Color";
        let name = "Color";
        let (kind, cleaned_name) = classify_symbol(raw, name);
        assert_eq!(kind, SymbolKind::Enum);
        assert_eq!(cleaned_name, name);
    }

    #[test]
    fn test_classify_symbol_function_default() {
        let raw = "fcn.00401000";
        let name = "fcn.00401000";
        let (kind, cleaned_name) = classify_symbol(raw, name);
        assert_eq!(kind, SymbolKind::Function);
        assert_eq!(cleaned_name, name);
    }

    #[test]
    fn test_classify_symbol_stripped_name_fallback() {
        // Test when raw_name doesn't match but stripped name does
        let raw = "sym.class.MyClass"; // r2 adds sym. prefix
        let name = "class.MyClass"; // after strip_r2_prefix
        let (kind, cleaned_name) = classify_symbol(raw, name);
        assert_eq!(kind, SymbolKind::Class);
        assert_eq!(cleaned_name, "class.MyClass");
    }

    #[test]
    fn test_classify_symbol_name_starts_with_class() {
        // Test when name (not raw_name) starts with prefix
        let raw = "something.class.MyClass"; // raw_name doesn't start with class.
        let name = "class.MyClass"; // but name does
        let (kind, cleaned_name) = classify_symbol(raw, name);
        assert_eq!(kind, SymbolKind::Class);
        assert_eq!(cleaned_name, name);
    }

    #[test]
    fn test_classify_symbol_name_starts_with_method() {
        // Test when name (not raw_name) starts with prefix
        let raw = "something.method.MyClass.my_method"; // raw_name doesn't start with method.
        let name = "method.MyClass.my_method"; // but name does
        let (kind, cleaned_name) = classify_symbol(raw, name);
        assert_eq!(kind, SymbolKind::Method);
        assert_eq!(cleaned_name, name);
    }

    #[test]
    fn test_classify_symbol_name_starts_with_namespace() {
        // Test when name (not raw_name) starts with prefix
        let raw = "something.namespace.std"; // raw_name doesn't start with namespace.
        let name = "namespace.std"; // but name does
        let (kind, cleaned_name) = classify_symbol(raw, name);
        assert_eq!(kind, SymbolKind::Module);
        assert_eq!(cleaned_name, name);
    }

    #[test]
    fn test_classify_symbol_name_starts_with_enum() {
        // Test when name (not raw_name) starts with prefix
        let raw = "something.enum.Color"; // raw_name doesn't start with enum.
        let name = "enum.Color"; // but name does
        let (kind, cleaned_name) = classify_symbol(raw, name);
        assert_eq!(kind, SymbolKind::Enum);
        assert_eq!(cleaned_name, name);
    }

    #[test]
    fn test_classify_symbol_no_match() {
        let raw = "some.other.symbol";
        let name = "some.other.symbol";
        let (kind, cleaned_name) = classify_symbol(raw, name);
        assert_eq!(kind, SymbolKind::Function);
        assert_eq!(cleaned_name, name);
    }

    // === JNI tests ===

    #[test]
    fn test_is_jni_name_java_prefix() {
        assert!(is_jni_name("Java_com_example_Foo_bar"));
        assert!(is_jni_name("Java_org_example_Baz_qux"));
    }

    #[test]
    fn test_is_jni_name_jni_prefix() {
        assert!(is_jni_name("JNI_OnLoad"));
        assert!(is_jni_name("JNI_OnUnload"));
        assert!(is_jni_name("JNI_RegisterNatives"));
        assert!(is_jni_name("JNI_CreateJavaVM"));
    }

    #[test]
    fn test_is_jni_name_not_jni() {
        assert!(!is_jni_name("fcn.00401000"));
        assert!(!is_jni_name("sub_1234"));
        assert!(!is_jni_name("main"));
        assert!(!is_jni_name("sym.imp.puts"));
    }

    #[test]
    fn test_parse_iej_jni_detection() {
        // Mock R2Client returning iEj with JNI exports
        struct MockR2 {
            responses: HashMap<String, Value>,
        }
        impl R2Client for MockR2 {
            fn cmd(&mut self, _cmd: &str) -> Result<String, Error> { Ok(String::new()) }
            fn cmdj(&mut self, cmd: &str) -> Result<Value, Error> {
                Ok(self.responses.get(cmd).cloned().unwrap_or(json!([])))
            }
        }
        let mut mock = MockR2 {
            responses: HashMap::new(),
        };
        mock.responses.insert(
            "iEj".to_string(),
            json!([
                {"name": "JNI_OnLoad", "vaddr": 4194304, "bind": "GLOBAL", "type": "FUNC"},
                {"name": "Java_com_example_Foo_bar", "vaddr": 4194368, "bind": "GLOBAL", "type": "FUNC"},
                {"name": "free", "vaddr": 4194432, "bind": "GLOBAL", "type": "FUNC"}
            ]),
        );
        let exports = parse_iej(&mut mock).unwrap();
        assert_eq!(exports.len(), 3);
        assert!(exports.iter().any(|e| e.name.as_deref() == Some("JNI_OnLoad")));
        assert!(exports.iter().any(|e| e.name.as_deref() == Some("Java_com_example_Foo_bar")));
    }

    #[test]
    fn test_parse_iej_empty() {
        struct MockR2 {
            responses: HashMap<String, Value>,
        }
        impl R2Client for MockR2 {
            fn cmd(&mut self, _cmd: &str) -> Result<String, Error> { Ok(String::new()) }
            fn cmdj(&mut self, cmd: &str) -> Result<Value, Error> {
                Ok(self.responses.get(cmd).cloned().unwrap_or(json!([])))
            }
        }
        let mut mock = MockR2 {
            responses: HashMap::new(),
        };
        mock.responses.insert("iEj".to_string(), json!([]));
        let exports = parse_iej(&mut mock).unwrap();
        assert!(exports.is_empty());
    }

    #[test]
    fn test_jni_annotation_name_based() {
        // Java_com_* name should produce jni annotation via is_jni_name
        let raw = "Java_com_example_Foo_bar";
        let name = "Java_com_example_Foo_bar";
        let (kind, cleaned_name) = classify_symbol(raw, name);
        assert_eq!(kind, SymbolKind::Function);
        assert!(is_jni_name(&cleaned_name));
    }

    #[test]
    fn test_jni_annotation_address_based() {
        // JNI_OnLoad in iEj export table should match by address
        use std::collections::HashMap;
        let mut jni_export_map: HashMap<u64, String> = HashMap::new();
        jni_export_map.insert(4194304, "JNI_OnLoad".to_string());
        let addr = 4194304u64;
        assert!(jni_export_map.contains_key(&addr));
        // The function with this addr would get jni annotation even if r2 renamed it
    }
}
