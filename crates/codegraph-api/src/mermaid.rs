//! Mermaid diagram generators — shared bởi MCP và GraphQL (và mọi frontend).
//!
//! Sinh chuỗi Mermaid (`flowchart` / `graph`) từ dữ liệu graph để visualize
//! code flow trên Dashboard on-prem, không lộ raw source.

use crate::GraphApi;
use codegraph_core::{
    is_marker, marker_name, EffectType, FlowCall, FlowResult, SymbolId, MARKER_BRANCH_END,
    MARKER_BREAK, MARKER_CALL_ENTER, MARKER_CALL_EXIT, MARKER_CONTINUE, MARKER_IF_FALSE,
    MARKER_IF_TRUE, MARKER_LOOP, MARKER_LOOP_BACK, MARKER_RETURN, MARKER_STMT_END,
    MARKER_SWITCH_CASE, MARKER_SWITCH_CLOSE, MARKER_SWITCH_END, MARKER_SWITCH_START, MARKER_THROW,
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
    let labels: HashMap<usize, &str> = flow
        .branch_labels
        .iter()
        .map(|b| (b.position, b.label.as_str()))
        .collect();

    // Skip CALL_ENTER/EXIT, STMT_END when rendering nodes — they're structural only.
    // Build a filtered chain: (filtered_index, original_index, raw_value)
    let mut filtered: Vec<(usize, usize, u64)> = Vec::new();
    for (i, &raw) in flow.chain.iter().enumerate() {
        if raw == MARKER_CALL_ENTER || raw == MARKER_CALL_EXIT || raw == MARKER_STMT_END {
            continue;
        }
        filtered.push((filtered.len(), i, raw));
    }

    let mut out = String::from("flowchart TD\n");
    for (new_i, orig_i, raw) in &filtered {
        let desc = flow
            .chain_desc
            .get(*orig_i)
            .map(String::as_str)
            .unwrap_or("");
        let (open, close, label) = node_style(&flow.chain, *orig_i, *raw, desc, &calls, &labels);
        out.push_str(&format!(
            "  c{new_i}{open}\"{}\"{close}\n",
            sanitize(&label)
        ));
    }

    // Cạnh cấu trúc, sắp xếp để output ổn định (không phụ thuộc thứ tự build).
    // Gom tất cả cạnh từ cả 2 pass rồi dedup một lần để tránh duplicate edges.
    let mut all_edges: Vec<(usize, usize)> = Vec::new();

    // Pass 1: structural edges on original chain, remap to filtered indices.
    let edges = structural_edges(&flow.chain);
    let mut orig_to_filtered: Vec<Option<usize>> = vec![None; flow.chain.len()];
    for (new_i, orig_i, _) in &filtered {
        orig_to_filtered[*orig_i] = Some(*new_i);
    }
    for (a, b) in edges {
        if let (Some(fa), Some(fb)) = (orig_to_filtered[a], orig_to_filtered[b]) {
            all_edges.push((fa, fb));
        }
    }

    // Pass 2: structural edges on filtered chain (preserves structural markers except CALL_ENTER/EXIT/STMT_END).
    let filtered_chain: Vec<u64> = filtered.iter().map(|(_, _, raw)| *raw).collect();
    let filtered_edges = structural_edges(&filtered_chain);
    all_edges.extend(filtered_edges);

    // Method chain / closure body: chain lưu theo thứ tự emit (outer trước, inner
    // sau) nhưng runtime chạy inner trước rồi mới outer. Điều chỉnh cạnh cho đúng
    // thứ tự thực thi — xem `fix_call_edge_order`.
    fix_call_edge_order(&flow.chain, &filtered, &mut all_edges);

    // Dedup toàn bộ cạnh.
    all_edges.sort_unstable();
    all_edges.dedup();
    for (a, b) in all_edges {
        out.push_str(&format!("  c{a} --> c{b}\n"));
    }
    out
}

/// Lập bản đồ scope của từng node trong `filtered` (chỉ số đã lọc):
/// - `parent[i]`: chỉ số node call gần nhất bao lấy node `i` (MAX nếu top-level).
/// - `open[i]`: chỉ số gốc của `CALL_ENTER` mở ra vùng chứa node `i`.
fn call_scope(chain: &[u64], filtered: &[(usize, usize, u64)]) -> (Vec<usize>, Vec<usize>) {
    const NONE: usize = usize::MAX;
    let n = filtered.len();
    let mut parent = vec![NONE; n];
    let mut open = vec![NONE; n];
    // Stack call đang mở; phần tử = chỉ số filtered của call sở hữu vùng đó,
    // NONE = vùng vừa mở chưa gán owner (chờ node call đầu tiên).
    let mut stack: Vec<usize> = Vec::new();
    let mut cur_open = NONE;
    let mut fi = 0usize;

    for (i, &raw) in chain.iter().enumerate() {
        match raw {
            MARKER_CALL_ENTER => {
                cur_open = i;
                stack.push(NONE);
            }
            MARKER_CALL_EXIT => {
                stack.pop();
                if stack.is_empty() {
                    cur_open = NONE;
                }
            }
            MARKER_STMT_END => {}
            _ => {
                if fi >= n {
                    break;
                }
                parent[fi] = stack
                    .iter()
                    .rev()
                    .copied()
                    .find(|&x| x != NONE)
                    .unwrap_or(NONE);
                open[fi] = cur_open;
                if !is_marker(raw) {
                    if let Some(last) = stack.last() {
                        if *last == NONE {
                            let top = stack.len() - 1;
                            stack[top] = fi;
                        }
                    }
                }
                fi += 1;
            }
        }
    }
    (parent, open)
}

/// Call cha ngoài cùng của node `x` (theo filtered index), `NONE` nếu top-level.
fn outermost_call(x: usize, parent: &[usize]) -> usize {
    const NONE: usize = usize::MAX;
    let mut cur = parent[x];
    let mut last = NONE;
    while cur != NONE {
        last = cur;
        cur = parent[cur];
    }
    last
}

/// Sửa thứ tự cạnh giữa các call node cho đúng runtime.
///
/// Chain emit theo pre-order nên node cha (call ngoài cùng) đứng TRƯỚC node con,
/// nhưng `a.b().c()` chạy `a.b()` rồi mới `.c()`. Ba trường hợp:
///
/// 1. **Nested call** (`parent[b] == a`): đảo cạnh → `b → a` (con chạy trước).
/// 2. **Cross-region** (`a`, `b` thuộc 2 vùng con khác nhau của cùng call cha):
///    neo cạnh vào call sở hữu vùng thay vì nối tuần tự vô nghĩa. VD closure
///    `x.clone().unwrap_or_else(|| { ... })` — thân closure phải gắn với
///    `unwrap_or_else`, không phải nối tiếp sau `x.clone()`.
/// 3. **Rời vùng call sang control-flow marker**: neo vào call cha ngoài cùng.
fn fix_call_edge_order(
    chain: &[u64],
    filtered: &[(usize, usize, u64)],
    edges: &mut [(usize, usize)],
) {
    const NONE: usize = usize::MAX;
    let n = filtered.len();
    if n < 2 {
        return;
    }
    let (parent, open) = call_scope(chain, filtered);
    let is_call = |i: usize| i < n && !is_marker(filtered[i].2);
    let is_entry = |i: usize| {
        i < n
            && matches!(
                filtered[i].2,
                MARKER_IF_TRUE | MARKER_LOOP | MARKER_SWITCH_START
            )
    };

    for edge in edges.iter_mut() {
        let (a, b) = (edge.0, edge.1);
        if a >= n || b >= n {
            continue;
        }
        // 1. Call lồng nhau — con chạy trước cha.
        if is_call(a) && is_call(b) && parent[b] == a {
            edge.0 = b;
            edge.1 = a;
            continue;
        }
        // 2. Sang vùng con khác của cùng call cha — neo vào owner của vùng `b`.
        if is_call(a) && is_call(b) && open[a] != open[b] && parent[b] != a {
            let owner = outermost_call(b, &parent);
            if owner != NONE {
                edge.0 = owner;
            }
            continue;
        }
        // 3. Từ trong vùng call ra marker điều khiển — neo vào call cha ngoài cùng.
        if is_call(a) && is_entry(b) && open[a] != NONE && parent[a] != NONE {
            let outer = outermost_call(a, &parent);
            if outer != NONE && outer != a {
                edge.0 = outer;
            }
        }
    }
}

/// Phân loại một node → `(open, close, label)`.
fn node_style(
    chain: &[u64],
    i: usize,
    raw: u64,
    desc: &str,
    calls: &HashMap<usize, &FlowCall>,
    labels: &HashMap<usize, &str>,
) -> (&'static str, &'static str, String) {
    if is_marker(raw) {
        let name = marker_name(raw).unwrap_or("MARKER");
        let (open, close) = match name {
            "IF_TRUE" | "IF_FALSE" | "SWITCH_CASE" | "SWITCH_START" => ("{", "}"),
            "RETURN" | "THROW" | "BREAK" | "CONTINUE" | "LOOP" | "LOOP_BACK" | "BRANCH_END"
            | "SWITCH_END" | "SWITCH_CLOSE" | "RECURSIVE_CALL" | "CALL_ENTER" | "CALL_EXIT"
            | "STMT_END" => ("([", "])"),
            _ => ("[", "]"),
        };
        // Nhãn trigger: ưu tiên `branch_labels` (điều kiện/case persist), rồi
        // fallback suy từ guard của call trong nhánh (dữ liệu cũ chưa có label).
        let branch = labels
            .get(&i)
            .map(|s| s.to_string())
            .or_else(|| match name {
                "IF_TRUE" | "IF_FALSE" | "SWITCH_CASE" | "SWITCH_START" | "LOOP" => {
                    guard_for(chain, i, calls)
                }
                _ => None,
            });
        let label = match name {
            "IF_TRUE" | "IF_FALSE" | "SWITCH_CASE" | "SWITCH_START" | "LOOP" => match branch {
                Some(g) if !g.is_empty() => format!("{name}: {g}"),
                _ => name.to_string(),
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

    // Rút gọn label dài của external call: desc/to_name chứa toàn bộ chain với
    // newline + thụt lề (`self\n  .storage\n  .read()...`). Gộp whitespace thành
    // 1 space để đọc được — vẫn giữ NGUYÊN chuỗi method chain, không cắt bớt
    // segment (người đọc cần thấy `open_from_config(root).await.ok()` là gì).
    let label = {
        let collapsed: String = label
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        collapsed
    };
    let mut label = label;
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
        // Thêm biến gán nếu có.
        if let Some(var) = &c.into_var {
            if !var.is_empty() {
                label.push_str(&format!(" → {var}"));
            }
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
    // Ngăn xếp (cond_index, then_end_index, has_else) của node điều kiện (if) đang mở.
    let mut if_stack: Vec<(usize, Option<usize>, bool)> = Vec::new();
    // Ngăn xếp index của header loop đang mở.
    let mut loop_stack: Vec<usize> = Vec::new();
    // Ngăn xếp entry point của các switch đang mở.
    // Mỗi entry lưu (entry_index, vec<SWITCH_END_indices>) để ở SWITCH_CLOSE
    // nối TẤT CẢ case end về node CLOSE, không chỉ case cuối.
    let mut switch_stack: Vec<(usize, Vec<usize>)> = Vec::new();
    // Ngăn xếp depth của call nesting (cho CALL_ENTER/CALL_EXIT).
    let mut call_stack: Vec<usize> = Vec::new();

    let mut i = 1usize;
    while i < n {
        let prev = i - 1;
        let curr = chain[i];
        let curr_is_marker = is_marker(curr);
        let prev_is_marker = is_marker(chain[prev]);

        // Sequential edge rules:
        // - non-marker -> non-marker: statement to statement
        // - marker -> non-marker: marker to first statement in body (but NOT from STMT_END)
        // - non-marker -> marker: for linear entry (IF_TRUE, LOOP, SWITCH_START)
        //   AND for block end markers (BRANCH_END, SWITCH_END, SWITCH_CLOSE)
        // - marker -> marker: NO sequential edge (default)
        let is_linear_entry = matches!(curr, MARKER_IF_TRUE | MARKER_LOOP | MARKER_SWITCH_START);
        let is_block_end = matches!(
            curr,
            MARKER_BRANCH_END | MARKER_SWITCH_END | MARKER_SWITCH_CLOSE
        );
        let is_terminator = matches!(
            curr,
            MARKER_RETURN | MARKER_THROW | MARKER_BREAK | MARKER_CONTINUE
        );
        let prev_is_stmt_end = chain[prev] == MARKER_STMT_END;
        // Prev marker đóng block (có thể nối đến linear-entry marker kế tiếp)
        let prev_closes_block = matches!(
            chain[prev],
            MARKER_BRANCH_END
                | MARKER_SWITCH_END
                | MARKER_SWITCH_CLOSE
                | MARKER_LOOP_BACK
                | MARKER_LOOP
                | MARKER_IF_TRUE
                | MARKER_SWITCH_START
        );

        if !prev_is_marker && !curr_is_marker {
            // stmt -> stmt
            edges.push((prev, i));
        } else if prev_is_marker && !curr_is_marker && !prev_is_stmt_end {
            // marker -> stmt (not from STMT_END)
            edges.push((prev, i));
        } else if !prev_is_marker
            && curr_is_marker
            && (is_linear_entry || is_block_end || is_terminator)
        {
            // stmt -> linear-entry / block-end / terminator
            edges.push((prev, i));
        } else if prev_is_marker && curr_is_marker && is_linear_entry && prev_closes_block {
            // Bug A: marker-closes-block -> linear-entry marker
            // VD: SWITCH_CLOSE -> SWITCH_START, LOOP -> IF_TRUE, BRANCH_END -> IF_TRUE
            edges.push((prev, i));
        } else if prev_is_marker && curr_is_marker && (is_terminator || is_block_end) {
            // marker -> terminator/block-end marker: nhánh if/loop/switch-case mà
            // thân (hoặc phần cuối thân) lại là MARKER, không phải call/statement.
            // VD: `if c { return None; }` → IF_TRUE -> RETURN -> BRANCH_END;
            //     `match { A => return, .. }` → SWITCH_CASE -> RETURN -> SWITCH_END.
            // Thiếu rule này thì RETURN/BRANCH_END/SWITCH_END bị ORPHAN (nổi
            // rời ngoài graph).
            edges.push((prev, i));
        }
        // marker -> marker: no sequential edge (except above)
        // non-marker -> branch marker (IF_FALSE, SWITCH_CASE): no sequential edge
        // STMT_END -> next statement: NO edge (separator)

        match curr {
            MARKER_IF_TRUE => {
                // IF_TRUE already connected from prev (cond) via sequential
                if_stack.push((prev, None, false));
            }
            MARKER_IF_FALSE => {
                // IF_FALSE connects from cond (structural)
                let src = if_stack.last().map(|(s, _, _)| *s).unwrap_or(prev);
                edges.push((src, i));
                if let Some(top) = if_stack.last_mut() {
                    top.1 = Some(prev);
                    top.2 = true;
                }
            }
            MARKER_BRANCH_END => {
                let (cond, then_end, has_else) = if_stack.pop().unwrap_or((prev, None, false));
                // BRANCH_END connected from prev (last body stmt) via sequential
                if has_else {
                    if let Some(then_end_idx) = then_end {
                        edges.push((then_end_idx, i));
                    }
                } else {
                    let mut j = i + 1;
                    while j < n && is_marker(chain[j]) {
                        j += 1;
                    }
                    if j < n {
                        edges.push((cond, j));
                    } else {
                        edges.push((cond, i));
                    }
                }
            }
            MARKER_LOOP => {
                loop_stack.push(i);
            }
            MARKER_LOOP_BACK => {
                edges.push((prev, i));
                if let Some(header) = loop_stack.last().copied() {
                    edges.push((i, header));
                }
            }
            MARKER_SWITCH_START => {
                // Switch start: prev is the discriminant (or previous statement)
                // Sequential edge from prev handled above
                switch_stack.push((i, Vec::new())); // (entry, case_ends)
            }
            MARKER_SWITCH_CASE => {
                if let Some((entry, _)) = switch_stack.last() {
                    edges.push((*entry, i));
                } else {
                    edges.push((prev, i));
                }
            }
            MARKER_SWITCH_END => {
                edges.push((prev, i));
                // Gom SWITCH_END của case này, sẽ nối TẤT CẢ về SWITCH_CLOSE
                if let Some((_entry, ends)) = switch_stack.last_mut() {
                    ends.push(i);
                }
                // Không pop ở đây; đợi SWITCH_CLOSE để pop (chỉ có 1 SWITCH_CLOSE cuối cùng)
            }
            MARKER_SWITCH_CLOSE => {
                edges.push((prev, i));
                // Bug C: Nối TẤT CẢ case ends (SWITCH_END) đến SWITCH_CLOSE
                if let Some((_entry, ends)) = switch_stack.pop() {
                    for end in ends {
                        edges.push((end, i));
                    }
                }
            }
            MARKER_CALL_ENTER => {
                call_stack.push(i);
            }
            MARKER_CALL_EXIT => {
                if !call_stack.is_empty() {
                    call_stack.pop();
                }
            }
            MARKER_STMT_END => {
                // Statement end: acts as a barrier between statements.
                // No structural edges. Sequential edge logic already handles
                // the edge from previous statement to STMT_END (if non-marker -> marker).
                // We DON'T want edge from STMT_END to next statement.
                // The sequential rule `prev_is_marker && !curr_is_marker` would add
                // edge from STMT_END to next statement, which we DON'T want.
                // Fix: treat STMT_END as a marker that should NOT connect to next.
            }
            _ => {
                // Bug B: Terminator (RETURN/THROW/BREAK/CONTINUE) đã nhận cạnh từ prev
                // ở rule trên. Không cần xử lý thêm.
            }
        }
        i += 1;
    }
    edges
}

/// Control-flow của `head` với 2 màu so với `base` (branch review):
///
/// - call/step **chỉ có ở head** (không thấy ở base) → node `added` (xanh lá);
/// - call **chỉ có ở base** (đã bị xoá ở head) → node `removed` (đỏ), nối từ
///   root bằng cạnh nét đứt;
/// - còn lại giữ style mặc định.
///
/// Dùng để render "branch A vs branch B" trong màn review — thay đổi nổi bật
/// bằng 2 màu, vẫn là giao diện mermaid.
pub fn control_flow_diff(head: &FlowResult, base: &FlowResult) -> String {
    let calls: HashMap<usize, &FlowCall> = head.calls.iter().map(|c| (c.position, c)).collect();
    let labels: HashMap<usize, &str> = head
        .branch_labels
        .iter()
        .map(|b| (b.position, b.label.as_str()))
        .collect();
    let base_names: HashSet<&str> = base
        .calls
        .iter()
        .map(|c| c.to_name.as_str())
        .filter(|n| !n.is_empty())
        .collect();
    let head_names: HashSet<&str> = head
        .calls
        .iter()
        .map(|c| c.to_name.as_str())
        .filter(|n| !n.is_empty())
        .collect();

    let mut out = String::from("flowchart TD\n");
    let mut added_idx = Vec::new();
    for (i, &raw) in head.chain.iter().enumerate() {
        let desc = head.chain_desc.get(i).map(String::as_str).unwrap_or("");
        let (open, close, label) = node_style(&head.chain, i, raw, desc, &calls, &labels);
        out.push_str(&format!("  c{i}{open}\"{}\"{close}\n", sanitize(&label)));
        if let Some(c) = calls.get(&i) {
            if !c.to_name.is_empty() && !base_names.contains(c.to_name.as_str()) {
                added_idx.push(i);
            }
        }
    }

    let mut edges = structural_edges(&head.chain);
    edges.sort_unstable();
    edges.dedup();
    for (a, b) in edges {
        out.push_str(&format!("  c{a} --> c{b}\n"));
    }

    // Call có ở base nhưng không còn ở head → node đỏ nét đứt từ root.
    let mut removed_idx = Vec::new();
    let mut k = 0usize;
    for c in &base.calls {
        if !c.to_name.is_empty() && !head_names.contains(c.to_name.as_str()) {
            out.push_str(&format!(
                "  r{k}[[\"{} · removed\"]]\n",
                sanitize(&c.to_name)
            ));
            out.push_str(&format!("  c0 -.-> r{k}\n"));
            removed_idx.push(k);
            k += 1;
        }
    }

    if !added_idx.is_empty() || !removed_idx.is_empty() {
        out.push_str("  classDef added fill:#14532d,stroke:#22c55e,color:#dcfce7\n");
        out.push_str("  classDef removed fill:#450a0a,stroke:#ef4444,color:#fecaca\n");
        for i in &added_idx {
            out.push_str(&format!("  class c{i} added\n"));
        }
        for i in &removed_idx {
            out.push_str(&format!("  class r{i} removed\n"));
        }
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
    let cleaned = s.replace('"', "'").replace(['\n', '\r'], " ");
    let mut out: String = cleaned.chars().take(80).collect();
    if out.chars().count() < cleaned.chars().count() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::{BranchLabel, ScopeLevel, Symbol, SymbolKind, MARKER_SWITCH_CLOSE};

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
            branch_labels: Vec::new(),
        }
    }

    /// flow kèm branch_labels (position, label).
    fn flow_labelled(
        chain: Vec<u64>,
        desc: Vec<&str>,
        calls: Vec<FlowCall>,
        labels: Vec<(usize, &str)>,
    ) -> FlowResult {
        let mut f = flow(chain, desc, calls);
        f.branch_labels = labels
            .into_iter()
            .map(|(position, label)| BranchLabel {
                position,
                label: label.into(),
            })
            .collect();
        f
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
            into_var: None,
            is_new_var: false,
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
        // Cả hai nhánh then/else đều hội tụ tại BRANCH_END, rồi tiếp done.
        assert!(m.contains("c2 --> c5"), "{m}"); // then → BRANCH_END
        assert!(m.contains("c4 --> c5"), "{m}"); // else → BRANCH_END
        assert!(m.contains("c5 --> c6"), "{m}");
        // Không có cạnh trực tiếp cond → BRANCH_END khi có else (false path đi qua IF_FALSE).
        assert!(!m.contains("c0 --> c5"), "{m}");
        assert!(m.contains("c1{\"IF_TRUE\"}"), "{m}");
    }

    #[test]
    fn if_without_else_false_branch_skips_branch_end() {
        // if (cond) { then() } done()  — không có else
        let chain = vec![100, MARKER_IF_TRUE, 0, MARKER_BRANCH_END, 101];
        let f = flow(
            chain,
            vec!["root", "IF_TRUE", "then", "BRANCH_END", "done"],
            vec![call(2, "then", None, 3), call(4, "done", Some(102), 5)],
        );
        let m = control_flow(&f);
        // cond -> IF_TRUE -> then -> BRANCH_END
        assert!(m.contains("c0 --> c1"), "{m}");
        assert!(m.contains("c1 --> c2"), "{m}");
        assert!(m.contains("c2 --> c3"), "{m}");
        // False path: cond -> done (SKIP BRANCH_END)
        assert!(m.contains("c0 --> c4"), "{m}");
        // BRANCH_END -> done (then branch continues)
        assert!(m.contains("c3 --> c4"), "{m}");
        // Không có cond -> BRANCH_END
        assert!(!m.contains("c0 --> c3"), "{m}");
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
            MARKER_SWITCH_START,
            MARKER_SWITCH_CASE,
            0,
            MARKER_SWITCH_END,
            MARKER_SWITCH_CASE,
            0,
            MARKER_SWITCH_END,
            MARKER_SWITCH_CLOSE,
            101,
        ];
        let f = flow(
            chain,
            vec![
                "root",
                "SWITCH_START",
                "SWITCH_CASE",
                "a",
                "SWITCH_END",
                "SWITCH_CASE",
                "b",
                "SWITCH_END",
                "SWITCH_CLOSE",
                "done",
            ],
            vec![
                call(2, "a", None, 2),
                call(5, "b", None, 3),
                call(9, "done", Some(102), 4),
            ],
        );
        let m = control_flow(&f);
        // cả hai case toả từ node entry (c0 - discriminant).
        assert!(m.contains("c1 --> c2"), "{m}"); // SWITCH_START -> first case
        assert!(
            m.contains("c1 --> c5"),
            "second case should fan out from entry: {m}"
        );
        assert!(!m.contains("c4 --> c5"), "{m}"); // first case end NOT -> second case
    }

    #[test]
    fn filter_call_enter_exit_stmt_end_markers() {
        // Chain with CALL_ENTER/EXIT and STMT_END markers that should be filtered out
        let chain = vec![
            100,
            MARKER_CALL_ENTER,
            0,
            MARKER_CALL_EXIT,
            MARKER_STMT_END,
            101,
        ];
        let f = flow(
            chain,
            vec![
                "root",
                "CALL_ENTER",
                "call_fn",
                "CALL_EXIT",
                "STMT_END",
                "done",
            ],
            vec![call(2, "call_fn", None, 3)],
        );
        let m = control_flow(&f);
        eprintln!("Test mermaid output:\n{}", m);
        // CALL_ENTER, CALL_EXIT, STMT_END should be filtered out
        // Result should have: root(0) -> call_fn(1) -> done(2)
        assert!(m.contains("c0[\"root\"]"), "missing root: {m}");
        // call_fn is external (to_id=None) so gets " · ext" suffix
        assert!(m.contains("call_fn · L3 · ext"), "missing call_fn: {m}");
        // done has no line in the test (line 4 is from the chain desc)
        assert!(m.contains("c2[\"done\"]"), "missing done: {m}");
        // No MARKER nodes for CALL_ENTER/EXIT/STMT_END
        assert!(
            !m.contains("MARKER"),
            "should not contain filtered markers: {m}"
        );
        // Edges: root -> call_fn -> done
        assert!(m.contains("c0 --> c1"), "missing edge c0->c1: {m}");
        assert!(m.contains("c1 --> c2"), "missing edge c1->c2: {m}");
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
    fn branch_labels_override_guard_and_label_switch_and_loop() {
        // if + match + while, nhãn persist ở đúng vị trí marker.
        let chain = vec![
            100,
            MARKER_IF_TRUE,
            0,
            MARKER_BRANCH_END,
            MARKER_SWITCH_CASE,
            0,
            MARKER_SWITCH_END,
            MARKER_LOOP,
            0,
            MARKER_LOOP_BACK,
        ];
        let f = flow_labelled(
            chain,
            vec![
                "root",
                "IF_TRUE",
                "a",
                "BRANCH_END",
                "SWITCH_CASE",
                "b",
                "SWITCH_END",
                "LOOP",
                "c",
                "LOOP_BACK",
            ],
            vec![
                call(2, "a", Some(1), 2),
                call(5, "b", Some(2), 5),
                call(8, "c", Some(3), 8),
            ],
            vec![(1, "x > 0"), (4, "Cmd::Init"), (7, "i < n")],
        );
        let m = control_flow(&f);
        assert!(m.contains("c1{\"IF_TRUE: x > 0\"}"), "{m}");
        assert!(m.contains("c4{\"SWITCH_CASE: Cmd::Init\"}"), "{m}");
        assert!(m.contains("c7([\"LOOP: i < n\"])"), "{m}");
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

    #[test]
    fn diff_colors_added_and_removed_calls() {
        // base: root → a(), b()
        let base = flow(
            vec![100, 0, 0],
            vec!["root", "a", "b"],
            vec![call(1, "a", Some(101), 2), call(2, "b", Some(102), 3)],
        );
        // head: root → a(), c()  (b xoá, c thêm)
        let head = flow(
            vec![100, 0, 0],
            vec!["root", "a", "c"],
            vec![call(1, "a", Some(101), 2), call(2, "c", Some(103), 3)],
        );
        let m = control_flow_diff(&head, &base);
        // c (index 2) là added; b (không còn) là removed node r0.
        assert!(m.contains("classDef added"), "{m}");
        assert!(m.contains("class c2 added"), "{m}");
        assert!(m.contains("r0[[\"b · removed\"]]"), "{m}");
        assert!(m.contains("class r0 removed"), "{m}");
        // a không đổi → không được tô.
        assert!(!m.contains("class c1 added"), "{m}");
    }

    #[test]
    fn diff_without_changes_has_no_classes() {
        let f = flow(
            vec![100, 0],
            vec!["root", "a"],
            vec![call(1, "a", Some(101), 2)],
        );
        let m = control_flow_diff(&f, &f);
        assert!(!m.contains("classDef"), "{m}");
    }
}
