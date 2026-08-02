//! GraphIndex — manages multiple CallIndex instances for different edge kinds.
//! Provides fast graph traversal using SearchIndex (RadixTree + KMP) instead of SQLite BFS.

use crate::call_index::{CallIndex, KeyShape};
use codegraph_core::{EdgeKind, NodeId, Result};
use codegraph_db::Db;
use std::collections::HashMap;

/// Edge kinds that we support for indexed traversal.
/// These are the kinds used by Traversal::VIZ_EDGE_KINDS and impact_radius.
const INDEXED_EDGE_KINDS: &[EdgeKind] = &[
    EdgeKind::Calls,
    EdgeKind::Imports,
    EdgeKind::Extends,
    EdgeKind::Implements,
    EdgeKind::References,
    EdgeKind::TypeOf,
    EdgeKind::Instantiates,
    EdgeKind::Overrides,
    EdgeKind::Decorates,
];

/// GraphIndex wraps multiple CallIndex instances, one per edge kind.
/// Uses SearchIndex (RadixTree + KMP) for fast prefix-based traversal.
pub struct GraphIndex {
    /// CallIndex per edge kind. Key: edge kind string.
    indices: HashMap<String, CallIndex>,
    /// Shape used for all indices (Edge or Path).
    shape: KeyShape,
    /// Sharding factor for SearchIndex.
    sharding: usize,
    /// Hard limit for results (matches Traversal::HARD_LIMIT).
    pub hard_limit: usize,
}

impl GraphIndex {
    /// Create a new in-memory GraphIndex with the given shape.
    pub fn in_memory(shape: KeyShape) -> Self {
        let mut indices = HashMap::new();
        for kind in INDEXED_EDGE_KINDS {
            let idx = CallIndex::in_memory(shape);
            indices.insert(kind.as_str().to_string(), idx);
        }
        Self {
            indices,
            shape,
            sharding: 64,
            hard_limit: 5000,
        }
    }

    /// Create a new in-memory GraphIndex with custom sharding.
    pub fn in_memory_sharded(shape: KeyShape, sharding: usize) -> Self {
        let mut indices = HashMap::new();
        for kind in INDEXED_EDGE_KINDS {
            let idx = CallIndex::in_memory_sharded(shape, sharding);
            indices.insert(kind.as_str().to_string(), idx);
        }
        Self {
            indices,
            shape,
            sharding,
            hard_limit: 5000,
        }
    }

    /// Create a new file-backed GraphIndex (requires `sqlite` feature).
    #[cfg(feature = "sqlite")]
    pub fn open(shape: KeyShape, base_path: &str) -> Result<Self> {
        Self::open_sharded(shape, base_path, 64)
    }

    #[cfg(feature = "sqlite")]
    pub fn open_sharded(shape: KeyShape, base_path: &str, sharding: usize) -> Result<Self> {
        let mut indices = HashMap::new();
        for kind in INDEXED_EDGE_KINDS {
            let kind_str = kind.as_str();
            let path = format!("{base_path}.{kind_str}");
            let idx = CallIndex::open_sharded(shape, &path, sharding)?;
            indices.insert(kind_str.to_string(), idx);
        }
        Ok(Self {
            indices,
            shape,
            sharding,
            hard_limit: 5000,
        })
    }

    /// Get the CallIndex for a specific edge kind.
    fn get_index(&self, kind: EdgeKind) -> Option<&CallIndex> {
        self.indices.get(kind.as_str())
    }

    /// Get mutable CallIndex for a specific edge kind.
    fn get_index_mut(&mut self, kind: EdgeKind) -> Option<&mut CallIndex> {
        self.indices.get_mut(kind.as_str())
    }

    /// Set hard limit for all indices.
    pub fn set_hard_limit(&mut self, limit: usize) {
        self.hard_limit = limit;
        for idx in self.indices.values_mut() {
            idx.set_hard_limit(limit);
        }
    }

    /// Rebuild all indices from the database.
    /// Extracts edges for each indexed edge kind and rebuilds the CallIndex.
    pub async fn rebuild_from_db(&mut self, db: &Db) -> Result<()> {
        for kind in INDEXED_EDGE_KINDS {
            let kind_str = kind.as_str();
            let edges = db.edges_by_kind(*kind)?;
            let edge_tuples: Vec<(u64, u64, Vec<u8>)> = edges
                .into_iter()
                .map(|e| (e.from as u64, e.to as u64, Vec::new()))
                .collect();

            if let Some(idx) = self.indices.get_mut(kind_str) {
                idx.rebuild(edge_tuples).await?;
            }
        }
        Ok(())
    }

    /// Reload all indices from storage (for file-backed indices).
    pub async fn reload(&mut self) -> Result<()> {
        for idx in self.indices.values_mut() {
            idx.reload().await?;
        }
        Ok(())
    }

    /// Clear all indices.
    pub async fn clear(&mut self) -> Result<()> {
        for idx in self.indices.values_mut() {
            idx.clear().await?;
        }
        Ok(())
    }

    // ── Traversal methods (using SearchIndex) ──

    /// Get direct callees (1 hop) for a specific edge kind.
    pub async fn direct_callees(&self, kind: EdgeKind, from: NodeId) -> Result<Vec<NodeId>> {
        if let Some(idx) = self.get_index(kind) {
            let callees = idx.direct_callees(from as u64).await?;
            Ok(callees.into_iter().map(|id| id as NodeId).collect())
        } else {
            Ok(Vec::new())
        }
    }

    /// Get direct callers (1 hop) for a specific edge kind.
    pub async fn direct_callers(&self, kind: EdgeKind, to: NodeId) -> Result<Vec<NodeId>> {
        if let Some(idx) = self.get_index(kind) {
            let callers = idx.direct_callers(to as u64).await?;
            Ok(callers.into_iter().map(|id| id as NodeId).collect())
        } else {
            Ok(Vec::new())
        }
    }

    /// Get all callees within depth hops for a specific edge kind.
    pub async fn callees(&self, kind: EdgeKind, from: NodeId, depth: usize) -> Result<Vec<NodeId>> {
        if let Some(idx) = self.get_index(kind) {
            let callees = idx.callees(from as u64, depth).await?;
            Ok(callees.into_iter().map(|id| id as NodeId).collect())
        } else {
            Ok(Vec::new())
        }
    }

    /// Get all callers within depth hops for a specific edge kind.
    pub async fn callers(&self, kind: EdgeKind, to: NodeId, depth: usize) -> Result<Vec<NodeId>> {
        if let Some(idx) = self.get_index(kind) {
            let callers = idx.callers(to as u64, depth).await?;
            Ok(callers.into_iter().map(|id| id as NodeId).collect())
        } else {
            Ok(Vec::new())
        }
    }

    /// Neighborhood traversal for a specific edge kind (both directions).
    /// Returns (callers, callees) within depth.
    pub async fn neighborhood(
        &self,
        kind: EdgeKind,
        id: NodeId,
        depth: usize,
    ) -> Result<(Vec<NodeId>, Vec<NodeId>)> {
        let callers = self.callers(kind, id, depth).await?;
        let callees = self.callees(kind, id, depth).await?;
        Ok((callers, callees))
    }

    /// Multi-kind neighborhood: union of callers/callees across kinds.
    pub async fn multi_neighborhood(
        &self,
        kinds: &[EdgeKind],
        id: NodeId,
        depth: usize,
    ) -> Result<(Vec<NodeId>, Vec<NodeId>)> {
        let mut all_callers = Vec::new();
        let mut all_callees = Vec::new();

        for kind in kinds {
            let (callers, callees) = self.neighborhood(*kind, id, depth).await?;
            all_callers.extend(callers);
            all_callees.extend(callees);
        }

        // Deduplicate
        all_callers.sort_unstable();
        all_callers.dedup();
        all_callees.sort_unstable();
        all_callees.dedup();

        // Apply hard limit
        if all_callers.len() > self.hard_limit {
            all_callers.truncate(self.hard_limit);
        }
        if all_callees.len() > self.hard_limit {
            all_callees.truncate(self.hard_limit);
        }

        Ok((all_callers, all_callees))
    }

    /// Impact radius: all nodes reachable via outgoing edges across kinds.
    /// Returns (direct, transitive) where direct = depth 1, transitive = depth > 1.
    pub async fn impact_radius(
        &self,
        kinds: &[EdgeKind],
        id: NodeId,
        max_depth: usize,
    ) -> Result<(Vec<NodeId>, Vec<NodeId>)> {
        let mut all_direct = Vec::new();
        let mut all_transitive = Vec::new();

        for kind in kinds {
            if let Some(idx) = self.get_index(*kind) {
                // Get all callees up to max_depth
                let callees = idx.callees(id as u64, max_depth).await?;

                // Separate direct (depth 1) from transitive
                let direct = idx.direct_callees(id as u64).await?;
                let direct_set: std::collections::HashSet<u64> = direct.into_iter().collect();

                for c in callees {
                    if direct_set.contains(&c) {
                        all_direct.push(c as NodeId);
                    } else {
                        all_transitive.push(c as NodeId);
                    }
                }
            }
        }

        // Deduplicate
        all_direct.sort_unstable();
        all_direct.dedup();
        all_transitive.sort_unstable();
        all_transitive.dedup();

        // Apply hard limit
        if all_direct.len() > self.hard_limit {
            all_direct.truncate(self.hard_limit);
        }
        if all_transitive.len() > self.hard_limit {
            all_transitive.truncate(self.hard_limit);
        }

        Ok((all_direct, all_transitive))
    }

    /// References: all nodes that have edges TO the given node across kinds.
    pub async fn references(
        &self,
        kinds: &[EdgeKind],
        id: NodeId,
    ) -> Result<HashMap<String, Vec<NodeId>>> {
        let mut by_kind = HashMap::new();

        for kind in kinds {
            let callers = self.direct_callers(*kind, id).await?;
            if !callers.is_empty() {
                by_kind.insert(kind.as_str().to_string(), callers);
            }
        }

        Ok(by_kind)
    }
}
