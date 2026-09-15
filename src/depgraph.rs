//! Phase 2: the dependency graph over recorded history.
//!
//! The graph is a derived, rebuildable **view** over the catalog. It is never
//! persisted as authority, it invents no identity, and it is recomputed on
//! demand (see `.ai/PHASE_2_DEPENDENCY_AWARE_INSPECTION.md` sections 4-5).
//!
//! Two properties matter more than the algorithm:
//!
//! * An edge carries the evidence that produced it, and that evidence decides
//!   whether the edge is `Known` (entailed by recorded durable identity) or
//!   `Advisory` (a correlation). An advisory edge is never a fact and never
//!   widens a rollback closure.
//! * "No edge" and "could not tell" are different answers. Where the inputs
//!   for an edge are missing or unusable, the graph records
//!   [`IncompleteEvidence`] instead of leaving a silent gap.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::error::Result;
use crate::model::{OperationKind, OperationRecord};
use crate::workspace::Workspace;

/// How many shared paths an effect-overlap edge records before truncating its
/// support list. The evidence is the overlap, not the full path list, and the
/// plan output stays readable.
const EFFECT_SUPPORT_LIMIT: usize = 16;

/// A node names an existing durable record. Phase 2 never mints an identity.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NodeId {
    Operation { id: i64 },
    State { id: String },
    Boundary { id: String },
}

impl NodeId {
    pub fn operation(id: i64) -> Self {
        Self::Operation { id }
    }

    /// Canonical, stable text form. Used as a deterministic tie-breaker.
    pub fn as_text(&self) -> String {
        match self {
            Self::Operation { id } => format!("OPERATION:{id}"),
            Self::State { id } => format!("STATE:{id}"),
            Self::Boundary { id } => format!("BOUNDARY:{id}"),
        }
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.as_text())
    }
}

/// What an edge rests on.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EvidenceKind {
    /// `to` began from the state `from` produced: `to.pre_state_id ==
    /// from.post_state_id`. Entailed by recorded identity.
    StateLineage,
    /// `from` and `to` changed at least one common path. A correlation, never
    /// causality: the path may have been rewritten by anything.
    EffectOverlap,
    /// `from` immediately precedes `to` in operation order.
    TemporalOrdering,
}

impl EvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StateLineage => "STATE_LINEAGE",
            Self::EffectOverlap => "EFFECT_OVERLAP",
            Self::TemporalOrdering => "TEMPORAL_ORDERING",
        }
    }

    /// The confidence an edge of this kind carries. Only lineage is entailed by
    /// durable identity; the other two are heuristics.
    pub fn confidence(self) -> EdgeConfidence {
        match self {
            Self::StateLineage => EdgeConfidence::Known,
            Self::EffectOverlap | Self::TemporalOrdering => EdgeConfidence::Advisory,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EdgeConfidence {
    /// Entailed by recorded durable identity.
    Known,
    /// A heuristic correlation. Reported, never acted on.
    Advisory,
}

const CONFIDENCE_RANK: [EdgeConfidence; 2] = [EdgeConfidence::Known, EdgeConfidence::Advisory];

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DependencyEdge {
    pub from: NodeId,
    pub to: NodeId,
    pub evidence: EvidenceKind,
    pub confidence: EdgeConfidence,
    /// The durable values that justify the edge: the shared state id, or the
    /// shared paths. Empty only for a temporal edge, which rests on order.
    pub support: Vec<String>,
}

impl DependencyEdge {
    pub fn state_lineage(from: i64, to: i64, state_id: &str) -> Self {
        Self {
            from: NodeId::operation(from),
            to: NodeId::operation(to),
            evidence: EvidenceKind::StateLineage,
            confidence: EvidenceKind::StateLineage.confidence(),
            support: vec![format!("state:{state_id}")],
        }
    }

    pub fn effect_overlap(from: i64, to: i64, shared_paths: Vec<String>) -> Self {
        Self {
            from: NodeId::operation(from),
            to: NodeId::operation(to),
            evidence: EvidenceKind::EffectOverlap,
            confidence: EvidenceKind::EffectOverlap.confidence(),
            support: shared_paths,
        }
    }

    pub fn temporal(from: i64, to: i64) -> Self {
        Self {
            from: NodeId::operation(from),
            to: NodeId::operation(to),
            evidence: EvidenceKind::TemporalOrdering,
            confidence: EvidenceKind::TemporalOrdering.confidence(),
            support: Vec::new(),
        }
    }
}

/// Evidence that could not be evaluated. Recorded so that a caller can never
/// mistake "could not tell" for "no dependency" (contract section 4.3).
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct IncompleteEvidence {
    pub operation_id: i64,
    pub evidence: EvidenceKind,
    pub detail: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct DependencyGraph {
    pub workspace_id: String,
    pub nodes: Vec<NodeId>,
    pub edges: Vec<DependencyEdge>,
    pub incomplete: Vec<IncompleteEvidence>,
    /// Cycles found in the graph. Populated only when `finished` is true.
    pub cycles: Vec<Vec<NodeId>>,
    /// False when the graph was constructed directly from edges and has not yet
    /// been normalised. `build` always returns a finished graph.
    pub finished: bool,
}

impl DependencyGraph {
    /// Build the graph from the workspace's own history. Read-only.
    pub fn build(workspace: &Workspace) -> Result<Self> {
        let catalog = &workspace.storage.catalog;
        let mut operations = catalog.list_operations(workspace.id)?;
        // Operation order: (created_at, id) ascending. `id` is AUTOINCREMENT and
        // is therefore a total order even when timestamps collide.
        operations.sort_by_key(|operation| (operation.created_at, operation.id));

        let mut graph = Self {
            workspace_id: workspace.id.to_string(),
            ..Self::default()
        };

        let mut nodes: BTreeSet<NodeId> = BTreeSet::new();
        let mut edges: Vec<DependencyEdge> = Vec::new();
        let mut incomplete: Vec<IncompleteEvidence> = Vec::new();

        // ---- lineage -----------------------------------------------------
        let mut producers: BTreeMap<String, Vec<i64>> = BTreeMap::new();
        let mut consumers: BTreeMap<String, Vec<i64>> = BTreeMap::new();
        for operation in &operations {
            if let Some(state) = &operation.post_state_id {
                producers
                    .entry(state.clone())
                    .or_default()
                    .push(operation.id);
                nodes.insert(NodeId::State { id: state.clone() });
            }
            if let Some(state) = &operation.pre_state_id {
                consumers
                    .entry(state.clone())
                    .or_default()
                    .push(operation.id);
                nodes.insert(NodeId::State { id: state.clone() });
            }
            if expects_state(&operation.kind)
                && (operation.pre_state_id.is_none() || operation.post_state_id.is_none())
            {
                incomplete.push(IncompleteEvidence {
                    operation_id: operation.id,
                    evidence: EvidenceKind::StateLineage,
                    detail: "a captured operation is missing a pre- or post-state, so its place in the chain cannot be established".to_owned(),
                });
            }
        }
        // Referenced states that the catalog does not hold: a dangling
        // reference is unknown evidence, not an absent edge.
        for state_id in producers
            .keys()
            .chain(consumers.keys())
            .collect::<BTreeSet<_>>()
        {
            if catalog.state(state_id).is_err() {
                let mut affected: Vec<i64> = producers
                    .get(state_id)
                    .into_iter()
                    .chain(consumers.get(state_id))
                    .flatten()
                    .copied()
                    .collect();
                affected.sort_unstable();
                affected.dedup();
                for operation_id in affected {
                    incomplete.push(IncompleteEvidence {
                        operation_id,
                        evidence: EvidenceKind::StateLineage,
                        detail: format!(
                            "referenced state {state_id} is not present in the catalog"
                        ),
                    });
                }
            }
        }
        for (state, produced_by) in &producers {
            let Some(consumed_by) = consumers.get(state) else {
                continue;
            };
            for producer in produced_by {
                for consumer in consumed_by {
                    // An operation that reports the same state before and after
                    // is a no-change capture, not a self-dependency; the graph
                    // does not synthesise a self-edge for it. A self-edge
                    // supplied directly is still detected as a cycle.
                    if producer == consumer {
                        continue;
                    }
                    edges.push(DependencyEdge::state_lineage(*producer, *consumer, state));
                }
            }
        }

        // ---- effect overlap ----------------------------------------------
        let mut by_path: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();
        for operation in &operations {
            if !effect_evidence_available(operation) {
                incomplete.push(IncompleteEvidence {
                    operation_id: operation.id,
                    evidence: EvidenceKind::EffectOverlap,
                    detail: format!(
                        "operation kind {} records no effects, so an empty effect list does not mean it changed nothing",
                        operation.kind.as_str()
                    ),
                });
                continue;
            }
            for effect in &operation.effects {
                by_path
                    .entry(effect.path.clone())
                    .or_default()
                    .insert(operation.id);
            }
        }
        let order = operation_order(&operations);
        let mut overlaps: BTreeMap<(i64, i64), Vec<String>> = BTreeMap::new();
        for (path, holders) in &by_path {
            if holders.len() < 2 {
                continue;
            }
            let holders: Vec<i64> = holders.iter().copied().collect();
            for (index, first) in holders.iter().enumerate() {
                for second in holders.iter().skip(index + 1) {
                    let (early, late) = match (order.get(first), order.get(second)) {
                        (Some(left), Some(right)) if left <= right => (*first, *second),
                        (Some(_), Some(_)) => (*second, *first),
                        _ => continue,
                    };
                    overlaps
                        .entry((early, late))
                        .or_default()
                        .push(path.clone());
                }
            }
        }
        for ((early, late), mut paths) in overlaps {
            paths.sort();
            paths.dedup();
            if paths.len() > EFFECT_SUPPORT_LIMIT {
                let remaining = paths.len() - EFFECT_SUPPORT_LIMIT;
                paths.truncate(EFFECT_SUPPORT_LIMIT);
                paths.push(format!("... ({remaining} more)"));
            }
            edges.push(DependencyEdge::effect_overlap(early, late, paths));
        }

        // ---- temporal order ----------------------------------------------
        for pair in operations.windows(2) {
            let (early, late) = (&pair[0], &pair[1]);
            let already_linked = edges.iter().any(|edge| {
                edge.evidence == EvidenceKind::StateLineage
                    && edge.from == NodeId::operation(early.id)
                    && edge.to == NodeId::operation(late.id)
            });
            if !already_linked {
                edges.push(DependencyEdge::temporal(early.id, late.id));
            }
        }

        // ---- boundaries ---------------------------------------------------
        for boundary in catalog.boundaries(workspace.id)? {
            nodes.insert(NodeId::Boundary { id: boundary.id });
        }
        for operation in &operations {
            nodes.insert(NodeId::Operation { id: operation.id });
        }

        graph.nodes = nodes.into_iter().collect();
        graph.edges = edges;
        graph.incomplete = incomplete;
        graph.normalise();
        Ok(graph)
    }

    /// Construct a graph directly from edges. Used by tests and by callers that
    /// already hold a normalised edge set; the result is normalised here.
    pub fn from_edges(workspace_id: impl Into<String>, edges: Vec<DependencyEdge>) -> Self {
        let mut graph = Self {
            workspace_id: workspace_id.into(),
            edges,
            ..Self::default()
        };
        graph.normalise();
        graph
    }

    /// Sort everything deterministically and detect cycles. Determinism is a
    /// contract requirement, so the order of `nodes`, `edges` and `incomplete`
    /// is defined and stable (contract section 10).
    pub fn normalise(&mut self) {
        let mut nodes: BTreeSet<NodeId> = BTreeSet::new();
        for edge in &self.edges {
            nodes.insert(edge.from.clone());
            nodes.insert(edge.to.clone());
        }
        for node in &self.nodes {
            nodes.insert(node.clone());
        }
        self.nodes = nodes.into_iter().collect();

        self.edges.sort_by_key(|edge| {
            (
                edge.from.clone(),
                edge.to.clone(),
                edge.evidence,
                edge.confidence,
            )
        });
        self.edges.dedup();

        self.incomplete.sort_by(|left, right| {
            (left.operation_id, left.evidence, &left.detail).cmp(&(
                right.operation_id,
                right.evidence,
                &right.detail,
            ))
        });
        self.incomplete.dedup();

        let adjacency = self.adjacency();
        self.cycles = detect_cycles(&self.nodes, &adjacency);
        self.finished = true;
    }

    fn adjacency(&self) -> BTreeMap<NodeId, Vec<NodeId>> {
        let mut adjacency: BTreeMap<NodeId, BTreeSet<NodeId>> = BTreeMap::new();
        for edge in &self.edges {
            adjacency
                .entry(edge.from.clone())
                .or_default()
                .insert(edge.to.clone());
        }
        adjacency
            .into_iter()
            .map(|(node, targets)| (node, targets.into_iter().collect()))
            .collect()
    }

    /// Every edge incident to `node`, in deterministic order.
    pub fn incident(&self, node: &NodeId) -> Vec<&DependencyEdge> {
        self.edges
            .iter()
            .filter(|edge| &edge.from == node || &edge.to == node)
            .collect()
    }

    /// Reachable using known (identity-entailed) edges only. This is the
    /// relation the rollback closure is computed from; an advisory edge can
    /// never make an operation reachable here.
    pub fn reaches_known(&self, from: &NodeId, to: &NodeId) -> bool {
        self.reaches_with(from, to, true)
    }

    /// Reachable using edges of any confidence. Used to describe context, never
    /// to decide a plan.
    pub fn reaches_any(&self, from: &NodeId, to: &NodeId) -> bool {
        self.reaches_with(from, to, false)
    }

    /// `known_only` selects whether advisory edges are traversable.
    fn reaches_with(&self, from: &NodeId, to: &NodeId, known_only: bool) -> bool {
        let mut adjacency: BTreeMap<&NodeId, Vec<&NodeId>> = BTreeMap::new();
        for edge in &self.edges {
            if known_only && edge.confidence != EdgeConfidence::Known {
                continue;
            }
            adjacency.entry(&edge.from).or_default().push(&edge.to);
        }
        let mut stack: Vec<&NodeId> = vec![from];
        let mut seen: BTreeSet<&NodeId> = BTreeSet::new();
        while let Some(node) = stack.pop() {
            if node == to {
                return true;
            }
            if !seen.insert(node) {
                continue;
            }
            if let Some(next) = adjacency.get(node) {
                stack.extend(next.iter().copied());
            }
        }
        false
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }
}

fn expects_state(kind: &OperationKind) -> bool {
    matches!(
        kind,
        OperationKind::Strong
            | OperationKind::PassiveObservation
            | OperationKind::Restore
            | OperationKind::Reconciliation
    )
}

/// A `BOUNDARY_ONLY` or `CAPTURE_FAILED` operation records no effects by
/// construction, so its empty effect list is missing evidence rather than
/// evidence of absence (contract section 9).
fn effect_evidence_available(operation: &OperationRecord) -> bool {
    if matches!(
        operation.kind,
        OperationKind::BoundaryOnly | OperationKind::CaptureFailed
    ) {
        return false;
    }
    !operation.effects.is_empty()
}

fn operation_order(operations: &[OperationRecord]) -> BTreeMap<i64, usize> {
    operations
        .iter()
        .enumerate()
        .map(|(index, operation)| (operation.id, index))
        .collect()
}

/// Every cycle closed by a back edge of a depth-first traversal from the
/// deterministic node order. Each cycle is rotated to start at its smallest
/// node and the set is deduplicated, so the result is stable for identical
/// input. Cycles are reported, never broken (contract section 5).
fn detect_cycles(nodes: &[NodeId], adjacency: &BTreeMap<NodeId, Vec<NodeId>>) -> Vec<Vec<NodeId>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Colour {
        White,
        Grey,
        Black,
    }

    let mut colour: BTreeMap<NodeId, Colour> = nodes
        .iter()
        .cloned()
        .map(|node| (node, Colour::White))
        .collect();
    let mut found: BTreeSet<Vec<NodeId>> = BTreeSet::new();

    for start in nodes {
        if colour.get(start) != Some(&Colour::White) {
            continue;
        }
        colour.insert(start.clone(), Colour::Grey);
        let mut stack: Vec<(NodeId, usize)> = vec![(start.clone(), 0)];
        let mut path: Vec<NodeId> = vec![start.clone()];

        while !stack.is_empty() {
            let (node, index) = {
                let top = stack.last().expect("non-empty");
                (top.0.clone(), top.1)
            };
            let empty: Vec<NodeId> = Vec::new();
            let children = adjacency.get(&node).unwrap_or(&empty);
            if index >= children.len() {
                colour.insert(node, Colour::Black);
                stack.pop();
                path.pop();
                continue;
            }
            if let Some(top) = stack.last_mut() {
                top.1 += 1;
            }
            let child = children[index].clone();
            match colour.get(&child).copied().unwrap_or(Colour::White) {
                Colour::White => {
                    colour.insert(child.clone(), Colour::Grey);
                    stack.push((child.clone(), 0));
                    path.push(child);
                }
                Colour::Grey => {
                    if let Some(position) = path.iter().position(|node| node == &child) {
                        let mut cycle: Vec<NodeId> = path[position..].to_vec();
                        if let Some(offset) = cycle
                            .iter()
                            .enumerate()
                            .min_by_key(|(_, node)| *node)
                            .map(|(index, _)| index)
                        {
                            cycle.rotate_left(offset);
                        }
                        found.insert(cycle);
                    }
                }
                Colour::Black => {}
            }
        }
    }

    found.into_iter().collect()
}

/// Rank used by callers that order evidence by strength. `Known` sorts first.
pub fn confidence_rank(confidence: EdgeConfidence) -> usize {
    CONFIDENCE_RANK
        .iter()
        .position(|candidate| *candidate == confidence)
        .unwrap_or(CONFIDENCE_RANK.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lineage(from: i64, to: i64) -> DependencyEdge {
        DependencyEdge::state_lineage(from, to, "shared-state")
    }

    #[test]
    fn detects_a_simple_cycle() {
        let graph =
            DependencyGraph::from_edges("w", vec![lineage(1, 2), lineage(2, 3), lineage(3, 1)]);
        assert_eq!(graph.cycles.len(), 1);
        assert_eq!(
            graph.cycles[0],
            vec![
                NodeId::operation(1),
                NodeId::operation(2),
                NodeId::operation(3)
            ]
        );
    }

    #[test]
    fn detects_a_longer_cycle() {
        let graph = DependencyGraph::from_edges(
            "w",
            vec![lineage(1, 2), lineage(2, 3), lineage(3, 4), lineage(4, 2)],
        );
        assert_eq!(graph.cycles.len(), 1);
        assert_eq!(
            graph.cycles[0],
            vec![
                NodeId::operation(2),
                NodeId::operation(3),
                NodeId::operation(4)
            ],
            "a cycle is reported rotated to its smallest node"
        );
    }

    #[test]
    fn detects_a_self_edge() {
        let graph = DependencyGraph::from_edges("w", vec![lineage(7, 7)]);
        assert_eq!(graph.cycles, vec![vec![NodeId::operation(7)]]);
    }

    #[test]
    fn detects_two_independent_cycles() {
        let graph = DependencyGraph::from_edges(
            "w",
            vec![
                lineage(1, 2),
                lineage(2, 3),
                lineage(3, 1),
                lineage(10, 11),
                lineage(11, 12),
                lineage(12, 10),
            ],
        );
        assert_eq!(graph.cycles.len(), 2);
        assert_eq!(graph.cycles[0][0], NodeId::operation(1));
        assert_eq!(graph.cycles[1][0], NodeId::operation(10));
    }

    #[test]
    fn a_cycle_next_to_unrelated_edges_is_still_reported() {
        let graph =
            DependencyGraph::from_edges("w", vec![lineage(1, 2), lineage(2, 1), lineage(3, 4)]);
        assert_eq!(graph.cycles.len(), 1);
        assert_eq!(
            graph.cycles[0],
            vec![NodeId::operation(1), NodeId::operation(2)]
        );
    }

    #[test]
    fn normalisation_is_deterministic_under_shuffled_input() {
        let forward = vec![lineage(1, 2), lineage(2, 3), lineage(3, 1)];
        let backward = vec![lineage(3, 1), lineage(1, 2), lineage(2, 3)];
        let ordered = DependencyGraph::from_edges("w", forward);
        let shuffled = DependencyGraph::from_edges("w", backward);
        assert_eq!(ordered.nodes, shuffled.nodes);
        assert_eq!(ordered.edges, shuffled.edges);
        assert_eq!(ordered.cycles, shuffled.cycles);
        assert_eq!(
            ordered.to_json().expect("json"),
            shuffled.to_json().expect("json")
        );
    }

    #[test]
    fn only_lineage_evidence_is_known() {
        assert_eq!(
            EvidenceKind::StateLineage.confidence(),
            EdgeConfidence::Known
        );
        assert_eq!(
            EvidenceKind::EffectOverlap.confidence(),
            EdgeConfidence::Advisory
        );
        assert_eq!(
            EvidenceKind::TemporalOrdering.confidence(),
            EdgeConfidence::Advisory
        );
        assert!(confidence_rank(EdgeConfidence::Known) < confidence_rank(EdgeConfidence::Advisory));
    }

    #[test]
    fn reachability_respects_confidence() {
        let graph = DependencyGraph::from_edges(
            "w",
            vec![
                lineage(1, 2),
                DependencyEdge::effect_overlap(2, 3, vec!["a.txt".to_owned()]),
            ],
        );
        assert!(graph.reaches_known(&NodeId::operation(1), &NodeId::operation(2)));
        assert!(!graph.reaches_known(&NodeId::operation(1), &NodeId::operation(3)));
        assert!(graph.reaches_any(&NodeId::operation(1), &NodeId::operation(3)));
    }
}
