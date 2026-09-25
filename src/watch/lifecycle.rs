//! Phase 3: lifecycle derivation for `rewind watch status`.
//!
//! The derived lifecycle is the workspace-facing answer to "is the watcher
//! observing, and can its observations be trusted?" — precedence order per
//! contract §7. Derivation reads only watcher artifacts; it never consults
//! the catalog, so it cannot turn an advisory state into an authoritative
//! one.

use std::time::Duration;

use serde::Serialize;

use super::model::{now_micros, Degradations, StatusKind, WatcherState, HEARTBEAT_MS};

/// The derived watcher/workspace lifecycle. `RECONCILIATION_REQUIRED`
/// dominates because it is the gate every writer honors; it exists until a
/// successful reconciliation consumes the degradation marker.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Lifecycle {
    Stopped,
    Running,
    Degraded,
    ReconciliationRequired,
    Stopping,
    Failed,
}

impl Lifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "STOPPED",
            Self::Running => "RUNNING",
            Self::Degraded => "DEGRADED",
            Self::ReconciliationRequired => "RECONCILIATION_REQUIRED",
            Self::Stopping => "STOPPING",
            Self::Failed => "FAILED",
        }
    }
}

/// A heartbeat is fresh when it is younger than this threshold. Generous on
/// purpose: a suspended machine or starved loop must degrade to
/// `Failed`/gap handling, never to a false `Running`. The threshold is at
/// least ten seconds and at least three heartbeat periods, so it stays
/// correct for any recorded batch window.
pub fn heartbeat_fresh_threshold() -> Duration {
    Duration::from_millis((HEARTBEAT_MS * 5).max(10_000))
}

fn heartbeat_age_micros(state: &WatcherState, now: i64) -> i64 {
    (now - state.heartbeat_at).max(0)
}

/// Public liveness check for the CLI paths (`stop`): is this recorded
/// heartbeat fresh enough to believe a watcher process is still alive?
pub fn heartbeat_is_fresh(state: &WatcherState) -> bool {
    heartbeat_age_micros(state, now_micros()) <= heartbeat_fresh_threshold().as_micros() as i64
}

/// Derives the lifecycle from the recorded state, pending degradations, and
/// the clock. `state` is `None` when no watcher run exists for the
/// workspace.
pub fn derive(state: Option<&WatcherState>, degradations: Option<&Degradations>) -> Lifecycle {
    let pending = degradations.is_some_and(|marker| !marker.records.is_empty());
    if pending {
        return Lifecycle::ReconciliationRequired;
    }
    let Some(state) = state else {
        return Lifecycle::Stopped;
    };
    let fresh = heartbeat_is_fresh(state);
    match state.status {
        StatusKind::Starting | StatusKind::Running => {
            if fresh {
                if state.degraded {
                    Lifecycle::Degraded
                } else {
                    Lifecycle::Running
                }
            } else {
                // Recorded as running but silent: presumed dead. Honest and
                // conservative — the next `watch start` treats everything
                // since the last heartbeat as an unobserved gap.
                Lifecycle::Failed
            }
        }
        StatusKind::Stopping => {
            if fresh {
                Lifecycle::Stopping
            } else {
                Lifecycle::Stopped
            }
        }
        StatusKind::Stopped => Lifecycle::Stopped,
        StatusKind::Failed => Lifecycle::Failed,
    }
}

/// What the user should do next. Advisory text; the authoritative gate is
/// the workspace condition, not this recommendation.
pub fn recommendation(lifecycle: Lifecycle) -> &'static str {
    match lifecycle {
        Lifecycle::ReconciliationRequired => {
            "run `rewind reconcile` to close the unknown interval and restore trust"
        }
        Lifecycle::Degraded => {
            "the watcher is running but degraded; run `rewind reconcile` before \
             relying on workspace trust"
        }
        Lifecycle::Failed => {
            "the watcher appears dead; restart with `rewind watch start` — the \
             unobserved interval will be gated until reconciliation"
        }
        Lifecycle::Stopped | Lifecycle::Running | Lifecycle::Stopping => "ok",
    }
}

/// Exit-code rule shared by the CLI: `3` means "needs operator action",
/// matching the repository's blocked/needs-action convention.
pub fn exit_code(lifecycle: Lifecycle) -> i32 {
    match lifecycle {
        Lifecycle::Stopped | Lifecycle::Running | Lifecycle::Stopping => 0,
        Lifecycle::Degraded | Lifecycle::ReconciliationRequired | Lifecycle::Failed => 3,
    }
}

#[derive(Serialize)]
pub struct StatusView<'a> {
    pub workspace_id: String,
    pub lifecycle: Lifecycle,
    pub recommendation: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<&'a WatcherState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_degradations: Option<&'a Degradations>,
    pub dirty_paths: usize,
    pub dirty_anomalies: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::model::{DegradationReason, DegradationRecord, WATCH_SCHEMA_VERSION};
    use uuid::Uuid;

    fn running_state(heartbeat_age_micros: i64) -> WatcherState {
        let mut state = WatcherState::new(Uuid::new_v4(), Uuid::new_v4(), StatusKind::Running, 250);
        state.heartbeat_at = now_micros() - heartbeat_age_micros;
        state
    }

    #[test]
    fn no_state_is_stopped() {
        assert_eq!(derive(None, None), Lifecycle::Stopped);
    }

    #[test]
    fn pending_degradations_dominate_every_state() {
        let marker = Degradations {
            schema_version: WATCH_SCHEMA_VERSION,
            workspace_id: Uuid::new_v4(),
            records: vec![DegradationRecord::new(DegradationReason::Overflow, "x")],
        };
        let state = running_state(0);
        assert_eq!(
            derive(Some(&state), Some(&marker)),
            Lifecycle::ReconciliationRequired
        );
        // Even a dead watcher cannot outrank the workspace gate.
        let stale = running_state(60_000_000);
        assert_eq!(
            derive(Some(&stale), Some(&marker)),
            Lifecycle::ReconciliationRequired
        );
    }

    #[test]
    fn stale_heartbeat_is_failed_not_running() {
        // 60 s old, threshold is 10 s: presumed dead.
        let state = running_state(60_000_000);
        assert_eq!(derive(Some(&state), None), Lifecycle::Failed);
    }

    #[test]
    fn fresh_heartbeat_is_running_until_degraded() {
        let state = running_state(0);
        assert_eq!(derive(Some(&state), None), Lifecycle::Running);
        let mut degraded = state.clone();
        degraded.degraded = true;
        degraded.degraded_detail = Some("overflow".to_owned());
        assert_eq!(derive(Some(&degraded), None), Lifecycle::Degraded);
    }

    #[test]
    fn recorded_stopped_is_stopped_even_when_old() {
        let mut state = WatcherState::new(Uuid::new_v4(), Uuid::new_v4(), StatusKind::Stopped, 250);
        state.heartbeat_at = now_micros() - 3_600_000_000;
        assert_eq!(derive(Some(&state), None), Lifecycle::Stopped);
    }

    #[test]
    fn failed_lifecycle_needs_action_and_running_does_not() {
        assert_eq!(exit_code(Lifecycle::Running), 0);
        assert_eq!(exit_code(Lifecycle::Stopped), 0);
        assert_eq!(exit_code(Lifecycle::Failed), 3);
        assert_eq!(exit_code(Lifecycle::ReconciliationRequired), 3);
        assert_eq!(exit_code(Lifecycle::Degraded), 3);
        assert_eq!(exit_code(Lifecycle::Stopping), 0);
    }
}
