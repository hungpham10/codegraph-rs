//! Mermaid diagram generators — shared bởi MCP và GraphQL (và mọi frontend).
//!
//! Sinh chuỗi Mermaid (`flowchart` / `graph`) từ dữ liệu graph để visualize
//! code flow trên Dashboard on-prem, không lộ raw source.

use crate::GraphApi;
use codegraph_core::{
    is_marker, marker_name, EffectType, FlowCall, FlowResult, SymbolId, MARKER_BRANCH_END,
    MARKER_IF_FALSE, MARKER_IF_TRUE, MARKER_LOOP, MARKER_LOOP_BACK, MARKER_SWITCH_CASE,
    MARKER_SWITCH_END,
};
use std::collections::{HashMap, HashSet};

/// Control-flow của một hàm (từ [`FlowResult`]): `flowchart TD` thể hiện **logic
/// hàm làm gì** và **call đi đâu** — không còn là một chuỗi node nối tiếp.
///
/// Mỗi element trong `flow.chain` là một node:
/// - **điều kiện** (`IF_TRUE` / `IF_FALSE` / `SWITCH_CASE`) → hình thoi, label kèm
///   guard text lấy từ call record trong nhánh;
/// - **vòng lặp / kết thúc** (`LOOP`, `LOOP_BACK`, `RETURN`, `THROW`, …) → stadium;
/// - **call / statement** → hộp chữ nhật, label gồm tên callee, `· L<line>`,
///   `· if <condition>` và effect (`http_call`, `sql_query`, …); call không
///   resolve được (ngoài repo) dùng hộp subroutine và gắn `· ext`.
///
/// Cạnh được suy từ marker (không còn nối tuyến tính mọi node):
/// `IF_FALSE` nối từ điều kiện `if`; `BRANCH_END` hợp nhất các nhánh;
/// `LOOP_BACK` vẽ cạnh quay về header; `SWITCH_CASE` toả ra từ node trước switch.
pub fn control_flow(flow: &FlowResult) -> String {
    let calls: HashMap<usize, &FlowCall> = flow.calls.iter().map(|c| (c.position, c)).collect();

    let mut out = String::from("flowchart TD\n");
    for (i, &raw) in flow.chain.iter().enumerate() {
        let desc = flow.chain_desc.get(i).map(String::as_str).unwrap_or("");
        let (open, close, label) = node_style(&flow.chain, i, raw, desc, &calls);
        out.push_str(&format!("  c{i}{open}\"{}\"{close}\n", sanitize(&label)));
    }

    // Cạnh cấu trúc, sắp xếp để output ổn định (không phụ thuộc thứ tự build).
    let mut edges = structural_edges(&flow.chain);
    edges.sort_unstable();
    edges.dedup();
    for (a, b) in edges {
        out.push_str(&format!("  c{a} --> c{b}\n"));
    }
    out
}

/// Phân loại một node → `(open, close, label)`.
fn node_style(
    chain: &[u64],
    i: usize,
    raw: u64,
    desc: &str,
    calls: &HashMap<usize, &FlowCall>,
) -> (&'static str, &'static str, String) {
    if is_marker(raw) {
        let name = marker_name(raw).unwrap_or("MARKER");
        let (open, close) = match name {
            "IF_TRUE" | "IF_FALSE" | "SWITCH_CASE" => ("{", "}"),
            "RETURN" | "THROW" | "BREAK" | "CONTINUE" | "LOOP" | "LOOP_BACK" | "BRANCH_END"
            | "SWITCH_END" | "RECURSIVE_CALL" => ("([", "])"),
            _ => ("[", "]"),
        };
        let label = match name {
            "IF_TRUE" | "IF_FALSE" | "SWITCH_CASE" => match guard_for(chain, i, calls) {
                Some(g) => format!("{name}: {g}"),
                None => name.to_string(),
            },
            _ => name.to_string(),
        };
        return (open, close, label);
    }

    // Call / statement thường.
    let call = calls.get(&i).copied();
    let mut label = call
        .map(|c| c.to_name.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| desc.to_string());
    if let Some(c) = call {
        if c.line > 0 {
            label.push_str(&format!(" · L{}", c.line));
        }
        if let Some(cond) = &c.condition {
            if !cond.is_empty() {
                label.push_str(&format!(" · if {cond}"));
            }
        }
        if c.effect != EffectType::None {
            label.push_str(" · ");
            label.push_str(c.effect.as_str());
        }
        if c.to_id.is_none() {
            label.push_str(" · ext");
        }
    }
    let external = call.map(|c| c.to_id.is_none()).unwrap_or(false);
    if external {
        ("[[", "]]", label)
    } else {
        ("[", "]", label)
    }
}

/// Guard text của một marker: ưu tiên call record cùng vị trí, rồi call đầu tiên
/// trong nhánh (trước marker kế tiếp) có `condition`.
fn guard_for(chain: &[u64], marker_at: usize, calls: &HashMap<usize, &FlowCall>) -> Option<String> {
    if let Some(cond) = calls.get(&marker_at).and_then(|c| c.condition.as_ref()) {
        if !cond.is_empty() {
            return Some(cond.clone());
        }
    }
    let mut j = marker_at + 1;
    while j < chain.len() && !is_marker(chain[j]) {
        if let Some(cond) = calls.get(&j).and_then(|c| c.condition.as_ref()) {
            if !cond.is_empty() {
                return Some(cond.clone());
            }
        }
        j += 1;
    }
    None
}

/// Suy cạnh cấu trúc từ chain + marker (if/else, loop, switch). Node `c0` là
/// root (không có cạnh vào); node thường nối tuần tự với node liền trước.
fn structural_edges(chain: &[u64]) -> Vec<(usize, usize)> {
    let n = chain.len();
    let mut edges: Vec<(usize, usize)> = Vec::new();
    // Ngăn xếp index của node điều kiện (if) đang mở.
    let mut if_stack: Vec<usize> = Vec::new();
    // Ngăn xếp index của header loop đang mở.
    let mut loop_stack: Vec<usize> = Vec::new();
    // Node trước switch đầu tiên của switch đang mở (các case toả từ đây).
    let mut switch_entry: Option<usize> = None;

    let mut i = 1usize;
    while i < n {
        let prev = i - 1;
        match chain[i] {
            MARKER_IF_TRUE => {
                edges.push((prev, i));
                if_stack.push(prev);
            }
            MARKER_IF_FALSE => {
                // else rẽ từ chính điều kiện `if`.
                let src = if_stack.last().copied().unwrap_or(prev);
                edges.push((src, i));
            }
            MARKER_BRANCH_END => {
                let cond = if_stack.pop().unwrap_or(prev);
                edges.push((cond, i)); // đường điều kiện sai (không có else)
                edges.push((prev, i)); // cuối nhánh
            }
            MARKER_LOOP => {
                edges.push((prev, i));
                loop_stack.push(i);
            }
            MARKER_LOOP_BACK => {
                edges.push((prev, i));
                if let Some(header) = loop_stack.last().copied() {
                    edges.push((i, header)); // back edge
                }
            }
            MARKER_SWITCH_CASE => match switch_entry {
                Some(entry) if entry != prev => edges.push((entry, i)),
                _ => {
                    switch_entry = Some(prev);
                    edges.push((prev, i));
                }
            },
            MARKER_SWITCH_END => {
                edges.push((prev, i));
                if chain.get(i + 1) != Some(&MARKER_SWITCH_CASE) {
                    switch_entry = None;
                }
            }
            _ => edges.push((prev, i)),
        }
        i += 1;
    }
    edges
}

/// Call graph (callers + callees) quanh một symbol, BFS tới `depth` hop.
pub async fn call_graph(api: &GraphApi, start: SymbolId, depth: u32) -> anyhow::Result<String> {
    let (nodes, edges) = build_call_graph(api, start, depth, None).await?;
    Ok(render_graph_lr(&nodes, &edges, start))
}

/// Callers (upstream) tới `depth` hop, dạng Mermaid `graph LR`.
pub async fn callers_mermaid(
    api: &GraphApi,
    start: SymbolId,
    depth: u32,
) -> anyhow::Result<String> {
    let (nodes, edges) = build_call_graph(api, start, depth, Some(false)).await?;
    Ok(render_graph_lr(&nodes, &edges, start))
}

/// Callees (downstream) tới `depth` hop, dạng Mermaid `graph LR`.
pub async fn callees_mermaid(
    api: &GraphApi,
    start: SymbolId,
    depth: u32,
) -> anyhow::Result<String> {
    let (nodes, edges) = build_call_graph(api, start, depth, Some(true)).await?;
    Ok(render_graph_lr(&nodes, &edges, start))
}

/// Impact (callers transitive) tới `max_depth` hop, dạng Mermaid `graph LR`.
pub async fn impact_mermaid(
    api: &GraphApi,
    start: SymbolId,
    max_depth: u32,
) -> anyhow::Result<String> {
    let (nodes, edges) = build_call_graph(api, start, max_depth, Some(false)).await?;
    Ok(render_graph_lr(&nodes, &edges, start))
}

/// BFS một hoặc cả hai hướng từ `start`, thu thập nodes + edges.
///
/// `direction`: `None` = cả hai hướng (call graph), `Some(true)` = chỉ
/// downstream (callees), `Some(false)` = chỉ upstream (callers/impact).
async fn build_call_graph(
    api: &GraphApi,
    start: SymbolId,
    depth: u32,
    direction: Option<bool>,
) -> anyhow::Result<(HashMap<SymbolId, String>, HashSet<(SymbolId, SymbolId)>)> {
    let start_sym = api
        .symbol_by_id(start)
        .await
        .ok_or_else(|| anyhow::anyhow!("symbol {start} not found"))?;
    let mut nodes: HashMap<SymbolId, String> = HashMap::new();
    let mut edges: HashSet<(SymbolId, SymbolId)> = HashSet::new();
    nodes.insert(start, start_sym.name.clone());
    match direction {
        Some(true) => bfs(api, start, depth, true, &mut nodes, &mut edges).await,
        Some(false) => bfs(api, start, depth, false, &mut nodes, &mut edges).await,
        None => {
            bfs(api, start, depth, true, &mut nodes, &mut edges).await;
            bfs(api, start, depth, false, &mut nodes, &mut edges).await;
        }
    }
    Ok((nodes, edges))
}

/// Render nodes + edges thành Mermaid `graph LR`, đánh dấu `root` = `start`.
fn render_graph_lr(
    nodes: &HashMap<SymbolId, String>,
    edges: &HashSet<(SymbolId, SymbolId)>,
    start: SymbolId,
) -> String {
    let mut out = String::from("graph LR\n");
    for (id, name) in nodes {
        if *id == start {
            out.push_str(&format!("  n{}[\"{} (root)\"]\n", id, sanitize(name)));
        } else {
            out.push_str(&format!("  n{}[\"{}\"]\n", id, sanitize(name)));
        }
    }
    for (a, b) in edges {
        out.push_str(&format!("  n{} --> n{}\n", a, b));
    }
    out
}

async fn bfs(
    api: &GraphApi,
    start: SymbolId,
    depth: u32,
    downstream: bool,
    nodes: &mut HashMap<SymbolId, String>,
    edges: &mut HashSet<(SymbolId, SymbolId)>,
) {
    let mut stack = vec![(start, 0u32)];
    let mut visited = HashSet::new();
    visited.insert(start);
    while let Some((id, d)) = stack.pop() {
        if d >= depth {
            continue;
        }
        let nexts = if downstream {
            api.callees(id).await.unwrap_or_default()
        } else {
            api.callers(id, 1).await.unwrap_or_default()
        };
        for n in nexts {
            nodes.entry(n.id).or_insert(n.name.clone());
            if downstream {
                edges.insert((id, n.id));
            } else {
                edges.insert((n.id, id));
            }
            if visited.insert(n.id) {
                stack.push((n.id, d + 1));
            }
        }
    }
}

/// Làm sạch label Mermaid: bỏ dấu ngoặc kép / xuống dòng, giới hạn 80 ký tự.
fn sanitize(s: &str) -> String {
    s.replace('"', "'")
        .replace(['\n', '\r'], " ")
        .chars()
        .take(80)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::{ScopeLevel, Symbol, SymbolKind};

    fn flow(chain: Vec<u64>, desc: Vec<&str>, calls: Vec<FlowCall>) -> FlowResult {
        FlowResult {
            symbol: Symbol {
                id: chain[0],
                name: "root".into(),
                kind: SymbolKind::Function,
                scope: ScopeLevel::Global,
                scope_id: 0,
                type_ref: 0,
                type_name: None,
                file: "a.ts".into(),
                line: 1,
                end_line: 10,
                signature: None,
                doc: None,
                annotations: Vec::new(),
                language: "typescript".into(),
            },
            chain,
            chain_desc: desc.into_iter().map(String::from).collect(),
            calls,
        }
    }

    fn call(pos: usize, name: &str, to_id: Option<u64>, line: u32) -> FlowCall {
        FlowCall {
            position: pos,
            to_name: name.into(),
            to_id,
            line,
            condition: None,
            effect: EffectType::None,
            effect_desc: None,
            args: Vec::new(),
        }
    }

    #[test]
    fn if_else_branches_do_not_chain_linearly() {
        // if (cond) { then() } else { other() } done()
        let chain = vec![
            100,
            MARKER_IF_TRUE,
            0,
            MARKER_IF_FALSE,
            0,
            MARKER_BRANCH_END,
            101,
        ];
        let f = flow(
            chain,
            vec![
                "root",
                "IF_TRUE",
                "then",
                "IF_FALSE",
                "other",
                "BRANCH_END",
                "done",
            ],
            vec![
                call(2, "then", None, 3),
                call(4, "other", None, 5),
                call(6, "done", Some(102), 7),
            ],
        );
        let m = control_flow(&f);
        // c1 (if) rẽ vào c2 (then); c3 (else) rẽ từ điều kiện c0, không từ c2.
        assert!(m.contains("c1 --> c2"), "{m}");
        assert!(m.contains("c0 --> c3"), "{m}");
        assert!(!m.contains("c2 --> c3"), "{m}");
        // cuối nhánh else nhập về BRANCH_END, và BRANCH_END nối tiếp done.
        assert!(m.contains("c4 --> c5"), "{m}");
        assert!(m.contains("c0 --> c5"), "{m}");
        assert!(m.contains("c5 --> c6"), "{m}");
        assert!(m.contains("c1{\"IF_TRUE\"}"), "{m}");
    }

    #[test]
    fn loop_back_edge_returns_to_header() {
        // while (x) { poll() }
        let chain = vec![100, MARKER_LOOP, 0, MARKER_LOOP_BACK, 0];
        let f = flow(
            chain,
            vec!["root", "LOOP", "poll", "LOOP_BACK", "done"],
            vec![call(2, "poll", None, 3), call(4, "done", Some(101), 4)],
        );
        let m = control_flow(&f);
        assert!(m.contains("c1([\"LOOP\"])"), "{m}");
        assert!(m.contains("c3([\"LOOP_BACK\"])"), "{m}");
        assert!(m.contains("c3 --> c1"), "back edge missing: {m}");
    }

    #[test]
    fn switch_cases_fan_out_from_entry() {
        // switch { case a(); case b(); } done()
        let chain = vec![
            100,
            MARKER_SWITCH_CASE,
            0,
            MARKER_SWITCH_END,
            MARKER_SWITCH_CASE,
            0,
            MARKER_SWITCH_END,
            101,
        ];
        let f = flow(
            chain,
            vec![
                "root",
                "SWITCH_CASE",
                "a",
                "SWITCH_END",
                "SWITCH_CASE",
                "b",
                "SWITCH_END",
                "done",
            ],
            vec![
                call(2, "a", None, 2),
                call(5, "b", None, 3),
                call(7, "done", Some(102), 4),
            ],
        );
        let m = control_flow(&f);
        // cả hai case toả từ node entry (c0).
        assert!(m.contains("c0 --> c1"), "{m}");
        assert!(
            m.contains("c0 --> c4"),
            "second case should fan out from entry: {m}"
        );
        assert!(!m.contains("c3 --> c4"), "{m}");
    }

    #[test]
    fn call_label_carries_line_condition_effect_and_ext() {
        let chain = vec![100, 0, 0];
        let mut c1 = call(1, "fetch", Some(101), 12);
        c1.condition = Some("ok".into());
        c1.effect = EffectType::HttpCall;
        let f = flow(
            chain,
            vec!["root", "fetch", "log"],
            vec![c1, call(2, "log", None, 13)],
        );
        let m = control_flow(&f);
        assert!(m.contains("c1[\"fetch · L12 · if ok · http_call\"]"), "{m}");
        // unresolved → subroutine box + `· ext`.
        assert!(m.contains("c2[[\"log · L13 · ext\"]]"), "{m}");
    }

    #[test]
    fn condition_marker_uses_branch_guard() {
        let chain = vec![100, MARKER_IF_TRUE, 0, MARKER_BRANCH_END];
        let mut then = call(2, "save", None, 5);
        then.condition = Some("dirty".into());
        let f = flow(
            chain,
            vec!["root", "IF_TRUE", "save", "BRANCH_END"],
            vec![then],
        );
        let m = control_flow(&f);
        assert!(m.contains("c1{\"IF_TRUE: dirty\"}"), "{m}");
    }

    #[test]
    fn root_has_no_incoming_edge() {
        let chain = vec![100, 0];
        let f = flow(
            chain,
            vec!["root", "work"],
            vec![call(1, "work", Some(101), 2)],
        );
        let m = control_flow(&f);
        assert!(!m.contains("--> c0\n"), "{m}");
        assert!(m.contains("c0 --> c1"), "{m}");
    }
}
