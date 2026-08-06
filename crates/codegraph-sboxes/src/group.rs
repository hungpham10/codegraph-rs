//! Load a *group* of functions from the graph: the flows that Piece 1 compiles.
//!
//! A group is the set of symbols we want to turn into real machine code. Calls
//! **between** group members are linked as real compiled calls; every other
//! callee (external, unresolved, or an in-repo symbol outside the group) is
//! dispatched to a Rhai mock at run time.

use codegraph_core::{FlowResult, Result, Symbol};
use codegraph_graph::GraphIndex;
use std::collections::HashSet;

/// One function in the group, ready to be compiled.
#[derive(Debug, Clone)]
pub struct GroupFunc {
    pub id: u64,
    pub symbol: Symbol,
    pub flow: FlowResult,
}

/// Load flows for every id in `ids` from the graph.
pub async fn load_group(index: &GraphIndex, ids: &[u64]) -> Result<Vec<GroupFunc>> {
    let mut out = Vec::with_capacity(ids.len());
    for &id in ids {
        let flow = index.flow(id).await?;
        out.push(GroupFunc {
            id,
            symbol: flow.symbol.clone(),
            flow,
        });
    }
    Ok(out)
}

/// The set of in-group symbol ids (callee ids inside the group compile to real
/// function calls instead of mock dispatches).
pub fn group_ids(group: &[GroupFunc]) -> HashSet<u64> {
    group.iter().map(|f| f.id).collect()
}

/// Flows indexed by symbol id, for link resolution during codegen.
pub fn by_id(group: &[GroupFunc]) -> std::collections::HashMap<u64, &GroupFunc> {
    group.iter().map(|f| (f.id, f)).collect()
}
