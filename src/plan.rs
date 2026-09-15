//! Phase 2: safety-first rollback planning.
//!
//! Multi-select rollback is **not** "run undo for each selected operation". The
//! planner validates the targets, computes the required closure, refuses when
//! the closure cannot be satisfied deterministically, and only then allows the
//! plan to reach the existing Phase 1 mutation engine. Planning itself is
//! read-only: it takes no writer lease and never mutates the workspace
//! (`.ai/PHASE_2_DEPENDENCY_AWARE_INSPECTION.md` sections 6-8).

use std::collections::BTreeSet;

use serde::Serialize;

use crate::depgraph::{
    DependencyEdge, DependencyGraph, EdgeConfidence, EvidenceKind, IncompleteEvidence, NodeId,
};
use crate::error::{Result, RewindError};
use crate::model::{OperationRecord, OperationStatus, Reversibility, WorkspaceCondition};
use crate::rollback::{self, RollbackOutcome};
use crate::workspace::Workspace;

/// How many advisory edges a plan carries as context.
const ADVISORY_CONTEXT_LIMIT: usize = 64;
/// How many conflicts or block reasons a plan records before it stops
/// collecting (the decision is already refusal at that point).
const FINDING_LIMIT: usize = 256;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TargetDirection {
    Undo,
    Redo,
}

impl TargetDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Undo => "UNDO",
            Self::Redo => "REDO",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct RollbackTarget {
    pub operation_id: i64,
    pub direction: TargetDirection,
}

impl RollbackTarget {
    pub fn undo(operation_id: i64) -> Self {
        Self {
            operation_id,
            direction: TargetDirection::Undo,
        }
    }

    pub fn redo(operation_id: i64) -> Self {
        Self {
            operation_id,
            direction: TargetDirection::Redo,
        }
    }
}

/// A structured conflict. Never a formatted string: each variant must be
/// distinguishable programmatically and carry the durable ids that caused it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "conflict", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RollbackConflict {
    TargetNotEligible {
        operation_id: i64,
        status: String,
        reversibility: String,
        direction: TargetDirection,
    },
    ConflictingTargets {
        operation_ids: Vec<i64>,
        detail: String,
    },
    DependencyUnknown {
        operation_id: i64,
        evidence: EvidenceKind,
    },
    UnknownInterval {
        interval_id: i64,
        reason: String,
    },
    UnsupportedObject {
        operation_id: i64,
        path: String,
        descriptor: String,
    },
    MissingHistoricalState {
        operation_id: i64,
        state_id: String,
    },
    IncompatibleOrdering {
        operation_ids: Vec<i64>,
    },
    CycleDetected {
        nodes: Vec<String>,
    },
    LiveStateMismatch {
        expected_state_id: String,
        found_state_id: String,
    },
    /// The live state could not be read at all, so the mismatch question is
    /// unanswered rather than answered negatively.
    LiveStateUnavailable {
        detail: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "block", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BlockReason {
    NotReversible {
        operation_id: i64,
        reversibility: String,
    },
    UnknownInterval {
        interval_id: i64,
        detail: String,
    },
    MissingHistoricalState {
        operation_id: i64,
        state_id: String,
    },
    UnsupportedObject {
        operation_id: i64,
        path: String,
    },
    WorkspaceNotHealthy {
        condition: String,
        pending_bypass: bool,
    },
    RecoveryRequired {
        transaction_id: String,
    },
    ConflictingTargets {
        operation_ids: Vec<i64>,
        detail: String,
    },
    CycleInClosure {
        nodes: Vec<String>,
    },
    LiveStateMismatch {
        expected_state_id: String,
        found_state_id: String,
    },
    LiveStateUnavailable {
        detail: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "reason", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IncludedReason {
    Selected,
    RequiredBy { operation_id: i64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct PlannedStep {
    pub operation_id: i64,
    pub direction: TargetDirection,
    pub reason: IncludedReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PlanDecision {
    Executable,
    Refused,
}

#[derive(Clone, Debug, Serialize)]
pub struct RollbackPlan {
    pub workspace_id: String,
    pub targets: Vec<RollbackTarget>,
    pub decision: PlanDecision,
    /// The deterministic execution order: what will happen, and why.
    pub order: Vec<PlannedStep>,
    pub conflicts: Vec<RollbackConflict>,
    pub block_reasons: Vec<BlockReason>,
    /// Evidence that could not be evaluated for operations in the closure.
    pub unknowns: Vec<IncompleteEvidence>,
    /// Advisory (heuristic) edges touching the closure. Context only: no step
    /// in `order` was caused by one, and none may be.
    pub advisory_context: Vec<DependencyEdge>,
    pub live_state_checked: bool,
    /// True when the plan rests only on evidence that was evaluated.
    pub complete: bool,
}

impl RollbackPlan {
    pub fn is_executable(&self) -> bool {
        self.decision == PlanDecision::Executable
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }
}

/// Build a plan for `targets`. Read-only.
pub fn plan_rollback(workspace: &Workspace, targets: &[RollbackTarget]) -> Result<RollbackPlan> {
    if targets.is_empty() {
        return Err(RewindError::InvalidCommand(
            "at least one rollback target is required".to_owned(),
        ));
    }

    let catalog = &workspace.storage.catalog;
    let mut operations = catalog.list_operations(workspace.id)?;
    operations.sort_by_key(|operation| (operation.created_at, operation.id));

    let mut canonical: Vec<RollbackTarget> = targets.to_vec();
    canonical.sort_by_key(|target| (target.direction, target.operation_id));
    canonical.dedup();

    let mut conflicts: Vec<RollbackConflict> = Vec::new();
    let mut block_reasons: Vec<BlockReason> = Vec::new();

    // ---- 1. every target must exist and be eligible, by the Phase 1 rules ----
    for target in &canonical {
        let Some(operation) = operations
            .iter()
            .find(|operation| operation.id == target.operation_id)
        else {
            return Err(RewindError::NotFound(format!(
                "operation {}",
                target.operation_id
            )));
        };
        let eligible = match target.direction {
            TargetDirection::Undo => {
                operation.status == OperationStatus::Completed
                    && operation.reversibility == Reversibility::FullyReversible
            }
            TargetDirection::Redo => {
                operation.status == OperationStatus::Undone
                    && operation.reversibility == Reversibility::FullyReversible
            }
        };
        if !eligible {
            conflicts.push(RollbackConflict::TargetNotEligible {
                operation_id: operation.id,
                status: operation.status.as_str().to_owned(),
                reversibility: operation.reversibility.as_str().to_owned(),
                direction: target.direction,
            });
        }
    }

    // ---- 2. one direction per plan ----
    let directions: BTreeSet<TargetDirection> =
        canonical.iter().map(|target| target.direction).collect();
    if directions.len() > 1 {
        conflicts.push(RollbackConflict::ConflictingTargets {
            operation_ids: canonical.iter().map(|t| t.operation_id).collect(),
            detail:
                "Phase 2 executes one direction per plan; undo and redo targets cannot be combined"
                    .to_owned(),
        });
    }
    let redo_targets = canonical
        .iter()
        .filter(|target| target.direction == TargetDirection::Redo)
        .count();
    if redo_targets > 1 {
        conflicts.push(RollbackConflict::ConflictingTargets {
            operation_ids: canonical.iter().map(|t| t.operation_id).collect(),
            detail: "Phase 2 executes at most one redo target per plan".to_owned(),
        });
    }
    // The same operation selected twice in opposite directions.
    for (position, first) in canonical.iter().enumerate() {
        for second in canonical.iter().skip(position + 1) {
            if first.operation_id == second.operation_id {
                conflicts.push(RollbackConflict::ConflictingTargets {
                    operation_ids: vec![first.operation_id],
                    detail: "the same operation is a target in two directions".to_owned(),
                });
            }
        }
    }

    // ---- 3. workspace gates, using the Phase 1 predicates ----
    let row = workspace.row()?;
    if let Some((transaction_id, _, _)) = catalog
        .unfinished_transactions(workspace.id)?
        .into_iter()
        .next()
    {
        block_reasons.push(BlockReason::RecoveryRequired {
            transaction_id: transaction_id.to_string(),
        });
    }
    let pending_bypass = catalog.has_pending_bypass(workspace.id)?;
    if row.condition != WorkspaceCondition::Healthy || pending_bypass {
        block_reasons.push(BlockReason::WorkspaceNotHealthy {
            condition: row.condition.as_str().to_owned(),
            pending_bypass,
        });
    }

    // ---- 4. the closure, computed on known lineage order only ----
    let mut order: Vec<PlannedStep> = Vec::new();
    let mut included: Vec<&OperationRecord> = Vec::new();
    let selected: BTreeSet<i64> = canonical.iter().map(|t| t.operation_id).collect();

    if directions.len() == 1 && !canonical.is_empty() {
        match canonical[0].direction {
            TargetDirection::Undo => {
                let oldest = canonical
                    .iter()
                    .filter_map(|target| {
                        operations
                            .iter()
                            .position(|operation| operation.id == target.operation_id)
                    })
                    .min();
                if let Some(oldest) = oldest {
                    let anchor = operations[oldest].id;
                    for operation in operations.iter().skip(oldest) {
                        if operation.status == OperationStatus::Undone {
                            continue;
                        }
                        if operation.status != OperationStatus::Completed
                            || operation.reversibility != Reversibility::FullyReversible
                        {
                            block_reasons.push(BlockReason::NotReversible {
                                operation_id: operation.id,
                                reversibility: operation.reversibility.as_str().to_owned(),
                            });
                            included.push(operation);
                            continue;
                        }
                        included.push(operation);
                        let reason = if selected.contains(&operation.id) {
                            IncludedReason::Selected
                        } else {
                            IncludedReason::RequiredBy {
                                operation_id: anchor,
                            }
                        };
                        order.push(PlannedStep {
                            operation_id: operation.id,
                            direction: TargetDirection::Undo,
                            reason,
                        });
                    }
                    // Newest first: the live state walks back through them.
                    order.reverse();
                }
            }
            TargetDirection::Redo => {
                if let Some(target) = canonical.first() {
                    if let Some(operation) = operations
                        .iter()
                        .find(|operation| operation.id == target.operation_id)
                    {
                        included.push(operation);
                        order.push(PlannedStep {
                            operation_id: operation.id,
                            direction: TargetDirection::Redo,
                            reason: IncludedReason::Selected,
                        });
                    }
                }
            }
        }
    }

    // ---- 5. unknown intervals are a hard boundary ----
    // An open unknown interval means the trusted baseline does not cover the
    // present, so *any* rollback is undecidable: Phase 1 refuses too (its
    // healthy gate blocks undo while reconciliation is required). An interval
    // is therefore a hard boundary, never "probably irrelevant". The closure is
    // additionally reported when it reaches into the interval.
    let newest_included = included.iter().map(|operation| operation.created_at).max();
    for interval in catalog
        .unknown_intervals(workspace.id)?
        .into_iter()
        .filter(|interval| interval.is_open)
    {
        let reaches_into = newest_included
            .map(|newest| newest >= interval.created_at)
            .unwrap_or(false);
        block_reasons.push(BlockReason::UnknownInterval {
            interval_id: interval.id,
            detail: format!(
                "open unknown interval from state {} ({}) has not been reconciled{}",
                interval
                    .start_state_id
                    .as_deref()
                    .unwrap_or("<unrecorded state>"),
                interval.reason,
                if reaches_into {
                    "; the rollback closure reaches into the interval"
                } else {
                    "; the trusted baseline does not cover the present"
                }
            ),
        });
        conflicts.push(RollbackConflict::UnknownInterval {
            interval_id: interval.id,
            reason: interval.reason.clone(),
        });
    }
    // ---- 6. historical state and unsupported objects in the closure ----
    for operation in &included {
        for state_id in [&operation.pre_state_id, &operation.post_state_id]
            .into_iter()
            .flatten()
        {
            if catalog.state(state_id).is_err() {
                block_reasons.push(BlockReason::MissingHistoricalState {
                    operation_id: operation.id,
                    state_id: state_id.clone(),
                });
                conflicts.push(RollbackConflict::MissingHistoricalState {
                    operation_id: operation.id,
                    state_id: state_id.clone(),
                });
            }
        }
        for effect in &operation.effects {
            if !effect.pre.is_supported_for_restore() || !effect.post.is_supported_for_restore() {
                block_reasons.push(BlockReason::UnsupportedObject {
                    operation_id: operation.id,
                    path: effect.path.clone(),
                });
                conflicts.push(RollbackConflict::UnsupportedObject {
                    operation_id: operation.id,
                    path: effect.path.clone(),
                    descriptor: format!("{} -> {}", effect.pre.describe(), effect.post.describe()),
                });
            }
        }
    }

    // ---- 7. cycles that intersect the closure ----
    let graph = DependencyGraph::build(workspace)?;
    let closure_nodes: BTreeSet<NodeId> = included
        .iter()
        .map(|operation| NodeId::operation(operation.id))
        .collect();
    for cycle in &graph.cycles {
        if cycle.iter().all(|node| closure_nodes.contains(node)) {
            let nodes: Vec<String> = cycle.iter().map(|node| node.as_text()).collect();
            conflicts.push(RollbackConflict::CycleDetected {
                nodes: nodes.clone(),
            });
            block_reasons.push(BlockReason::CycleInClosure { nodes });
        }
    }

    // ---- 8. live-state check, read-only ----
    let expected_state = match canonical.first().map(|target| target.direction) {
        Some(TargetDirection::Redo) => included.first().and_then(|o| o.pre_state_id.clone()),
        _ => order.first().and_then(|step| {
            operations
                .iter()
                .find(|operation| operation.id == step.operation_id)
                .and_then(|operation| operation.post_state_id.clone())
        }),
    };
    let mut live_state_checked = false;
    if canonical.len() == 1 {
        match workspace.observe(None) {
            Ok(scan) => {
                live_state_checked = true;
                if let Some(expected) = expected_state {
                    if scan.state_id != expected {
                        conflicts.push(RollbackConflict::LiveStateMismatch {
                            expected_state_id: expected.clone(),
                            found_state_id: scan.state_id.clone(),
                        });
                        block_reasons.push(BlockReason::LiveStateMismatch {
                            expected_state_id: expected,
                            found_state_id: scan.state_id,
                        });
                    }
                }
            }
            Err(error) => {
                let detail = error.to_string();
                conflicts.push(RollbackConflict::LiveStateUnavailable {
                    detail: detail.clone(),
                });
                block_reasons.push(BlockReason::LiveStateUnavailable { detail });
            }
        }
    }

    // ---- 9. unknown evidence for closure members ----
    let unknowns: Vec<IncompleteEvidence> = graph
        .incomplete
        .iter()
        .filter(|record| closure_nodes.contains(&NodeId::operation(record.operation_id)))
        .cloned()
        .collect();
    for unknown in &unknowns {
        conflicts.push(RollbackConflict::DependencyUnknown {
            operation_id: unknown.operation_id,
            evidence: unknown.evidence,
        });
    }
    let complete = unknowns.is_empty();

    // ---- 10. advisory context, never a step ----
    let advisory_context: Vec<DependencyEdge> = graph
        .edges
        .iter()
        .filter(|edge| {
            edge.confidence == EdgeConfidence::Advisory
                && (closure_nodes.contains(&edge.from) || closure_nodes.contains(&edge.to))
        })
        .take(ADVISORY_CONTEXT_LIMIT)
        .cloned()
        .collect();

    // ---- 11. deterministic finding order ----
    conflicts.sort_by_key(|conflict| format!("{conflict:?}"));
    conflicts.dedup();
    block_reasons.sort_by_key(|reason| format!("{reason:?}"));
    block_reasons.dedup();
    conflicts.truncate(FINDING_LIMIT);
    block_reasons.truncate(FINDING_LIMIT);

    let decision = if conflicts.is_empty() && block_reasons.is_empty() {
        PlanDecision::Executable
    } else {
        PlanDecision::Refused
    };

    Ok(RollbackPlan {
        workspace_id: workspace.id.to_string(),
        targets: canonical,
        decision,
        order,
        conflicts,
        block_reasons,
        unknowns,
        advisory_context,
        live_state_checked,
        complete,
    })
}

/// Execute an approved plan through the Phase 1 engine.
///
/// The plan is a proposal, never a capability: this re-plans from the same
/// targets and refuses if the result differs, and every step still goes through
/// `rollback::undo`/`rollback::redo`, which take the writer lease, run
/// `recover_locked`, enforce the pending safety gate and require a healthy
/// workspace.
pub fn execute(workspace: &Workspace, plan: &RollbackPlan) -> Result<Vec<RollbackOutcome>> {
    if !plan.is_executable() {
        return Err(RewindError::ConditionBlocked(
            "the plan is refused; nothing was executed".to_owned(),
        ));
    }
    let current = plan_rollback(workspace, &plan.targets)?;
    if !current.is_executable() || current.order != plan.order {
        return Err(RewindError::ConditionBlocked(
            "the plan is stale: the workspace or history changed since it was built; re-plan before executing"
                .to_owned(),
        ));
    }

    let mut outcomes = Vec::new();
    for step in &current.order {
        let outcome = match step.direction {
            TargetDirection::Undo => rollback::undo(workspace, Some(step.operation_id), false)?,
            TargetDirection::Redo => rollback::redo(workspace, Some(step.operation_id))?,
        };
        outcomes.push(outcome);
    }
    Ok(outcomes)
}
