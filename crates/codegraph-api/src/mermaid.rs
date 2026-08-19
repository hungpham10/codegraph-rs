//! Mermaid diagram generators — shared bởi MCP và GraphQL (và mọi frontend).
//!
//! Sinh chuỗi Mermaid (`flowchart` / `graph`) từ dữ liệu graph để visualize
//! code flow trên Dashboard on-prem, không lộ raw source.

use crate::GraphApi;
use codegraph_core::{is_marker, marker_name, FlowResult, SymbolId};
use std::collections::{HashMap, HashSet};

/// Control-flow của một hàm (từ [`FlowResult`]): `flowchart TD`, mỗi element
/// trong `chain` là một node, marker thành node kiểu quyết định / vòng lặp.
pub fn control_flow(flow: &FlowResult) -> String {
    let mut out = String::from("flowchart TD\n");
    let mut nodes: Vec<String> = Vec::new();
    for (i, desc) in flow.chain_desc.iter().enumerate() {
        let node_id = format!("c{i}");
        let raw = flow.chain.get(i).copied().unwrap_or(0);
        let (open, close) = if is_marker(raw) {
            match marker_name(raw) {
                Some("IF_TRUE") | Some("IF_FALSE") | Some("BRANCH_END") => ("{", "}"),
                Some("LOOP") | Some("LOOP_BACK") => ("([", "])"),
                Some("RETURN") | Some("BREAK") | Some("CONTINUE") | Some("THROW") => ("([", "])"),
                _ => ("[", "]"),
            }
        } else {
            ("[", "]")
        };
        let label = sanitize(desc);
        out.push_str(&format!("  {node_id}{open}\"{label}\"{close}\n"));
        nodes.push(node_id);
    }
    for w in nodes.windows(2) {
        out.push_str(&format!("  {} --> {}\n", w[0], w[1]));
    }
    out
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
