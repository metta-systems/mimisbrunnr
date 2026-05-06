//! Implication DAG (DESIGN §3.3).
//!
//! Nodes are [`TagId`]s; edges are directed `from → to` meaning "from implies
//! to". The graph is required to be acyclic — every `add_implication` is
//! followed by a `cycle_check` and rolled back if a cycle would result.
//!
//! Backed by `petgraph::Graph` for the heavy lifting plus a `HashMap` for O(1)
//! `TagId → NodeIndex` lookup; `petgraph` doesn't expose its internal node
//! lookup, so we maintain the index ourselves.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use mimisbrunnr_types::TagId;
use petgraph::{Direction, graph::NodeIndex, prelude::DiGraph, visit::EdgeRef};
use serde::{Deserialize, Serialize};

use crate::error::OntologyError;

/// Directed acyclic graph of tag implications.
#[derive(Debug, Clone, Default)]
pub struct ImplicationDag {
    graph: DiGraph<TagId, ()>,
    nodes: HashMap<TagId, NodeIndex>,
}

impl ImplicationDag {
    /// Construct an empty DAG.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a tag node if absent. Idempotent.
    pub fn add_tag(&mut self, tag: TagId) {
        if !self.nodes.contains_key(&tag) {
            let idx = self.graph.add_node(tag);
            self.nodes.insert(tag, idx);
        }
    }

    /// Returns `true` iff `tag` is a node in the graph.
    pub fn contains(&self, tag: TagId) -> bool {
        self.nodes.contains_key(&tag)
    }

    /// Add a directed edge `from → to` ("from implies to"). Both endpoints
    /// must already be nodes (call [`add_tag`] first). After insertion the
    /// graph is checked for cycles; on cycle the edge is removed and an error
    /// returned.
    pub fn add_implication(&mut self, from: TagId, to: TagId) -> Result<(), OntologyError> {
        let from_idx = *self
            .nodes
            .get(&from)
            .ok_or(OntologyError::TagNotFound(from))?;
        let to_idx = *self.nodes.get(&to).ok_or(OntologyError::TagNotFound(to))?;

        // Skip duplicate edges; petgraph would happily add a parallel edge.
        if self.graph.find_edge(from_idx, to_idx).is_some() {
            return Ok(());
        }

        let edge = self.graph.add_edge(from_idx, to_idx, ());
        if let Err(err) = self.cycle_check() {
            self.graph.remove_edge(edge);
            return Err(err);
        }
        Ok(())
    }

    /// Remove an implication edge if present.
    pub fn remove_implication(&mut self, from: TagId, to: TagId) {
        let (Some(&fi), Some(&ti)) = (self.nodes.get(&from), self.nodes.get(&to)) else {
            return;
        };
        if let Some(e) = self.graph.find_edge(fi, ti) {
            self.graph.remove_edge(e);
        }
    }

    /// Remove a tag node and all incident edges.
    pub fn remove_tag(&mut self, tag: TagId) {
        if let Some(idx) = self.nodes.remove(&tag) {
            // `remove_node` invalidates the highest-index node by swapping it
            // into `idx`'s slot. Patch up our `nodes` table accordingly.
            let last = NodeIndex::new(self.graph.node_count() - 1);
            let last_tag = self.graph[last];
            self.graph.remove_node(idx);
            if last != idx {
                // The tag that previously lived at `last` now lives at `idx`.
                self.nodes.insert(last_tag, idx);
            }
        }
    }

    /// Returns `true` iff `tag` directly or transitively implies `ancestor`.
    /// Equivalent to: there is a directed path `tag → … → ancestor`. By
    /// convention every tag implies itself.
    pub fn is_a(&self, tag: TagId, ancestor: TagId) -> bool {
        if tag == ancestor {
            return self.nodes.contains_key(&tag);
        }
        let Some(&start) = self.nodes.get(&tag) else {
            return false;
        };
        let Some(&target) = self.nodes.get(&ancestor) else {
            return false;
        };
        self.reachable(start, target)
    }

    fn reachable(&self, start: NodeIndex, target: NodeIndex) -> bool {
        let mut visited: HashSet<NodeIndex> = HashSet::new();
        let mut queue: VecDeque<NodeIndex> = VecDeque::new();
        queue.push_back(start);
        visited.insert(start);
        while let Some(node) = queue.pop_front() {
            for neigh in self.graph.neighbors_directed(node, Direction::Outgoing) {
                if neigh == target {
                    return true;
                }
                if visited.insert(neigh) {
                    queue.push_back(neigh);
                }
            }
        }
        false
    }

    /// Direct out-neighbours of `tag` (the tags it directly implies).
    pub fn direct_implies(&self, tag: TagId) -> Vec<TagId> {
        let Some(&idx) = self.nodes.get(&tag) else {
            return Vec::new();
        };
        self.graph
            .neighbors_directed(idx, Direction::Outgoing)
            .map(|n| self.graph[n])
            .collect()
    }

    /// Compute the materialised closure of a set of direct tags: the union of
    /// the direct tags and every tag reachable from any of them via outgoing
    /// edges. Returned in deterministic order (sorted by `TagId`).
    pub fn closure(&self, direct_tags: &[TagId]) -> Vec<TagId> {
        let mut result: BTreeSet<TagId> = BTreeSet::new();
        let mut queue: VecDeque<NodeIndex> = VecDeque::new();
        let mut visited: HashSet<NodeIndex> = HashSet::new();

        for &t in direct_tags {
            if let Some(&idx) = self.nodes.get(&t)
                && visited.insert(idx)
            {
                result.insert(t);
                queue.push_back(idx);
            }
        }

        while let Some(node) = queue.pop_front() {
            for neigh in self.graph.neighbors_directed(node, Direction::Outgoing) {
                if visited.insert(neigh) {
                    result.insert(self.graph[neigh]);
                    queue.push_back(neigh);
                }
            }
        }

        result.into_iter().collect()
    }

    /// Validate that the graph is a DAG. Returns the offending edge endpoints
    /// if a cycle exists.
    pub fn cycle_check(&self) -> Result<(), OntologyError> {
        match petgraph::algo::toposort(&self.graph, None) {
            Ok(_) => Ok(()),
            Err(cyc) => {
                // Cycle node — pick any incoming edge to point at.
                let node = cyc.node_id();
                let to = self.graph[node];
                let from = self
                    .graph
                    .edges_directed(node, Direction::Incoming)
                    .next()
                    .map(|e| self.graph[e.source()])
                    .unwrap_or(to);
                Err(OntologyError::CycleDetected { from, to })
            }
        }
    }

    /// Topological order of all tags. Errors with the offending edge on cycle.
    pub fn topological_sort(&self) -> Result<Vec<TagId>, OntologyError> {
        match petgraph::algo::toposort(&self.graph, None) {
            Ok(order) => Ok(order.into_iter().map(|n| self.graph[n]).collect()),
            Err(cyc) => {
                let node = cyc.node_id();
                let to = self.graph[node];
                let from = self
                    .graph
                    .edges_directed(node, Direction::Incoming)
                    .next()
                    .map(|e| self.graph[e.source()])
                    .unwrap_or(to);
                Err(OntologyError::CycleDetected { from, to })
            }
        }
    }

    /// Iterator over all tag nodes.
    pub fn tags(&self) -> impl Iterator<Item = TagId> + '_ {
        self.graph.node_weights().copied()
    }

    /// Iterator over all `(from, to)` edges.
    pub fn edges(&self) -> impl Iterator<Item = (TagId, TagId)> + '_ {
        self.graph
            .edge_references()
            .map(|e| (self.graph[e.source()], self.graph[e.target()]))
    }

    /// Number of tag nodes.
    pub fn tag_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Number of implication edges.
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }
}

// -------------------------------------------------------------------------
// Persistence shape — the in-memory graph isn't directly serialisable. We
// dump the node and edge lists as flat vectors and rebuild on load.
// -------------------------------------------------------------------------

/// Serialisable snapshot of the implication DAG. Public because it
/// appears in the [`crate::PersistedState`] (R1b-8) signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagSnapshot {
    /// Tag node ids, sorted ascending.
    pub tags: Vec<TagId>,
    /// Directed implication edges `(from, to)`.
    pub edges: Vec<(TagId, TagId)>,
}

impl ImplicationDag {
    /// Build a serialisable snapshot. R1b-8 persists this inside
    /// [`crate::PersistedState`].
    pub fn snapshot(&self) -> DagSnapshot {
        let mut tags: Vec<TagId> = self.graph.node_weights().copied().collect();
        tags.sort();
        let mut edges: Vec<(TagId, TagId)> = self.edges().collect();
        edges.sort();
        DagSnapshot { tags, edges }
    }

    /// Rebuild the DAG from a [`DagSnapshot`]. Errors if the encoded
    /// edges form a cycle (no longer satisfies the DAG invariant).
    pub fn from_snapshot(snap: DagSnapshot) -> Result<Self, OntologyError> {
        let mut dag = ImplicationDag::new();
        for t in snap.tags {
            dag.add_tag(t);
        }
        for (from, to) in snap.edges {
            dag.add_implication(from, to)?;
        }
        Ok(dag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: u32) -> TagId {
        TagId::new(id)
    }

    #[test]
    fn add_tag_idempotent() {
        let mut d = ImplicationDag::new();
        d.add_tag(t(1));
        d.add_tag(t(1));
        assert_eq!(d.tag_count(), 1);
    }

    #[test]
    fn implication_unknown_tag_is_error() {
        let mut d = ImplicationDag::new();
        d.add_tag(t(1));
        assert!(matches!(
            d.add_implication(t(1), t(2)),
            Err(OntologyError::TagNotFound(_))
        ));
    }

    #[test]
    fn closure_direct_and_transitive() {
        let mut d = ImplicationDag::new();
        for id in 1..=3 {
            d.add_tag(t(id));
        }
        d.add_implication(t(1), t(2)).unwrap(); // car → vehicle
        d.add_implication(t(2), t(3)).unwrap(); // vehicle → physical_object

        let c = d.closure(&[t(1)]);
        assert_eq!(c, vec![t(1), t(2), t(3)]);
    }

    #[test]
    fn closure_deduplicates() {
        let mut d = ImplicationDag::new();
        for id in 1..=4 {
            d.add_tag(t(id));
        }
        // diamond
        d.add_implication(t(1), t(2)).unwrap();
        d.add_implication(t(1), t(3)).unwrap();
        d.add_implication(t(2), t(4)).unwrap();
        d.add_implication(t(3), t(4)).unwrap();

        let c = d.closure(&[t(1)]);
        assert_eq!(c, vec![t(1), t(2), t(3), t(4)]);
    }

    #[test]
    fn cycle_detected_and_rolled_back() {
        let mut d = ImplicationDag::new();
        for id in 1..=3 {
            d.add_tag(t(id));
        }
        d.add_implication(t(1), t(2)).unwrap();
        d.add_implication(t(2), t(3)).unwrap();
        let err = d.add_implication(t(3), t(1)).unwrap_err();
        assert!(matches!(err, OntologyError::CycleDetected { .. }));
        // edge must not have been left behind
        assert_eq!(d.edge_count(), 2);
    }

    #[test]
    fn self_loop_rejected() {
        let mut d = ImplicationDag::new();
        d.add_tag(t(1));
        assert!(matches!(
            d.add_implication(t(1), t(1)),
            Err(OntologyError::CycleDetected { .. })
        ));
    }

    #[test]
    fn is_a_basics() {
        let mut d = ImplicationDag::new();
        for id in 1..=3 {
            d.add_tag(t(id));
        }
        d.add_implication(t(1), t(2)).unwrap();
        d.add_implication(t(2), t(3)).unwrap();

        assert!(d.is_a(t(1), t(1))); // reflexive
        assert!(d.is_a(t(1), t(2)));
        assert!(d.is_a(t(1), t(3)));
        assert!(!d.is_a(t(3), t(1)));
        assert!(!d.is_a(t(1), t(99))); // unknown ancestor
    }

    #[test]
    fn topological_sort_works() {
        let mut d = ImplicationDag::new();
        for id in 1..=3 {
            d.add_tag(t(id));
        }
        d.add_implication(t(1), t(2)).unwrap();
        d.add_implication(t(2), t(3)).unwrap();
        let order = d.topological_sort().unwrap();
        let pos = |x: TagId| order.iter().position(|y| *y == x).unwrap();
        assert!(pos(t(1)) < pos(t(2)));
        assert!(pos(t(2)) < pos(t(3)));
    }

    #[test]
    fn remove_tag_drops_incident_edges() {
        let mut d = ImplicationDag::new();
        for id in 1..=3 {
            d.add_tag(t(id));
        }
        d.add_implication(t(1), t(2)).unwrap();
        d.add_implication(t(2), t(3)).unwrap();
        d.remove_tag(t(2));
        assert_eq!(d.tag_count(), 2);
        assert_eq!(d.edge_count(), 0);
        assert!(!d.contains(t(2)));
    }

    #[test]
    fn duplicate_implication_idempotent() {
        let mut d = ImplicationDag::new();
        d.add_tag(t(1));
        d.add_tag(t(2));
        d.add_implication(t(1), t(2)).unwrap();
        d.add_implication(t(1), t(2)).unwrap();
        assert_eq!(d.edge_count(), 1);
    }

    #[test]
    fn snapshot_round_trip() {
        let mut d = ImplicationDag::new();
        for id in 1..=4 {
            d.add_tag(t(id));
        }
        d.add_implication(t(1), t(2)).unwrap();
        d.add_implication(t(2), t(3)).unwrap();
        d.add_implication(t(2), t(4)).unwrap();

        let snap = d.snapshot();
        let d2 = ImplicationDag::from_snapshot(snap).unwrap();
        assert_eq!(d.tag_count(), d2.tag_count());
        assert_eq!(d.edge_count(), d2.edge_count());
        assert_eq!(d.closure(&[t(1)]), d2.closure(&[t(1)]));
    }
}
