//! Chuyển đổi output r2 → `ParseResult` cho `GraphIndex::ingest`.

use crate::config::AnalysisDepth;
use crate::model::*;
use crate::r2::R2Session;
use codegraph_core::{Annotation, CallRecord, EffectType, Error, Symbol, SymbolKind, SYMBOL_BASE};
use codegraph_graph::ParseResult;
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
    let mut symbols: Vec<Symbol> = Vec::new();
    let mut chains: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut calls: Vec<CallRecord> = Vec::new();
    let mut fn_by_addr: HashMap<u64, u64> = HashMap::new();
    let mut fn_id_to_name: HashMap<u64, String> = HashMap::new();
    let mut next_id = SYMBOL_BASE + 1;

    for entry in &functions {
        let addr = entry.offset.unwrap_or(0);
        let raw_name = entry
            .name
            .clone()
            .unwrap_or_else(|| format!("fcn.{addr:x}"));
        // PLT thunk của import — đã có symbol riêng từ `iij`, bỏ qua.
        if raw_name.starts_with("sym.imp.") {
            continue;
        }
        let name = strip_r2_prefix(&raw_name);
        let size = entry.size.unwrap_or(0);
        let sig = build_signature(addr, size, entry);
        let id = next_id;
        next_id += 1;
        fn_by_addr.insert(addr, id);
        fn_id_to_name.insert(id, name.clone());
        symbols.push(Symbol {
            id,
            name,
            kind: SymbolKind::Function,
            scope: codegraph_core::ScopeLevel::Global,
            scope_id: 0,
            type_ref: 0,
            type_name: None,
            file: path_str.to_string(),
            line: addr.try_into().unwrap_or(0),
            end_line: addr.saturating_add(size).try_into().unwrap_or(u32::MAX),
            signature: Some(sig),
            doc: None,
            annotations: Vec::new(),
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
        build_chains_from_graph(session, &functions, &maps, &mut chains, &mut calls)?;
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
        let addr = entry.offset.unwrap_or(0);
        let Some(&func_id) = maps.fn_by_addr.get(&addr) else {
            continue;
        };
        let ops: Vec<DisasmOp> = session
            .cmdj(&format!("pdfj @ {addr}"))?
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
            let off = op.offset.unwrap_or(0);
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

/// Xây chain nhẹ từ `agCj` (call graph edges) — không có marker CFG.
fn build_chains_from_graph(
    session: &mut dyn R2Client,
    functions: &[FnEntry],
    maps: &FnMaps,
    chains: &mut HashMap<u64, Vec<u64>>,
    calls: &mut Vec<CallRecord>,
) -> Result<(), Error> {
    let edges: Vec<CallGraphEdge> = session
        .cmdj("agCj")?
        .get("edges")
        .and_then(|e| e.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| serde_json::from_value::<CallGraphEdge>(v).ok())
        .collect();

    let mut by_caller: HashMap<u64, Vec<u64>> = HashMap::new();
    for edge in &edges {
        let from = edge.from.unwrap_or(0);
        let to = edge.to.unwrap_or(0);
        by_caller.entry(from).or_default().push(to);
    }

    for entry in functions {
        let addr = entry.offset.unwrap_or(0);
        let Some(&func_id) = maps.fn_by_addr.get(&addr) else {
            continue;
        };
        let mut chain = vec![func_id];
        for &to in by_caller.get(&addr).into_iter().flat_map(|v| v.iter()) {
            let call_name = resolve_call_name(to, maps);
            let pos = chain.len();
            chain.push(0);
            calls.push(CallRecord {
                caller_id: func_id,
                call_name,
                position: pos,
                arg_exprs: Vec::new(),
                line: addr.try_into().unwrap_or(0),
                condition: None,
                is_loop_body: false,
                effect: EffectType::None,
                effect_desc: None,
                target_class: None,
                target_method: None,
            });
        }
        chains.insert(func_id, chain);
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

fn resolve_call_name(addr: u64, maps: &FnMaps) -> String {
    if let Some(name) = maps.plt_by_addr.get(&addr) {
        return name.clone();
    }
    if let Some(&fid) = maps.fn_by_addr.get(&addr) {
        return maps
            .fn_id_to_name
            .get(&fid)
            .cloned()
            .unwrap_or_else(|| format!("sub_{addr:x}"));
    }
    format!("sub_{addr:x}")
}
