//! Phase 4: the time-range history view (`.ai/PHASE_4_TIME_AND_ECOSYSTEM.md`).
//!
//! The timeline merges **recorded evidence** inside a half-open time range
//! (`[since, until)`) from five sources, each labeled with its evidentiary
//! value. It is informational only: it never mutates, never opens a lease,
//! never runs enforcement, and can never initiate a restore. Uncertainty is
//! rendered as uncertainty — unknown intervals and watcher degradations make
//! the range explicitly incomplete, and nothing is ever inferred to fill a
//! gap.
//!
//! The merge is a pure function of the supplied records plus the resolved
//! range, so the same store contents always produce byte-identical JSON.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::db::{BoundaryRow, SnapshotRow, UnknownIntervalRow};
use crate::humantime::format_rfc3339;
use crate::model::OperationRecord;
use crate::watch::model::DegradationReason;

pub const TIMELINE_SCHEMA_VERSION: u64 = 1;

/// Evidence tiers, ordered by the rank used for deterministic tie-breaking
/// of identical timestamps (lower ranks sort first).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Tier {
    Operation,
    Snapshot,
    UnknownInterval,
    PassiveBoundary,
    Watch,
}

impl Tier {
    fn rank(self) -> u8 {
        match self {
            Tier::Operation => 0,
            Tier::Snapshot => 1,
            Tier::UnknownInterval => 2,
            Tier::PassiveBoundary => 3,
            Tier::Watch => 4,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Operation => "OPERATION",
            Tier::Snapshot => "SNAPSHOT",
            Tier::UnknownInterval => "UNKNOWN_INTERVAL",
            Tier::PassiveBoundary => "PASSIVE_BOUNDARY",
            Tier::Watch => "WATCH",
        }
    }
}

/// One timeline entry. `timestamp` is the record's anchor time inside the
/// range; span records carry `end_timestamp` (closed) or `None` (still open)
/// and are shown whole — never trimmed to the range, because trimming would
/// fabricate bounds the record does not contain.
#[derive(Clone, Debug, Serialize)]
pub struct TimelineEntry {
    pub tier: Tier,
    /// Epoch microseconds of the record's anchor time.
    pub timestamp: i64,
    /// Span records only: the recorded end of the span; `None` means the
    /// span is still open. Event-like records have no end.
    pub end_timestamp: Option<i64>,
    pub identifier: String,
    pub summary: String,
    /// As recorded for the operation tier; `None` for every other tier. The
    /// timeline reports what was recorded and never derives undoability.
    pub confidence: Option<String>,
    pub reversibility: Option<String>,
    /// Unknown intervals only: whether the interval is still open.
    pub open: Option<bool>,
}

/// Watcher advisory evidence for the range: bounded summaries, never a raw
/// event dump. The watcher is not proof of filesystem state, and its log
/// rotates, so its coverage is stated explicitly.
#[derive(Clone, Debug, Default, Serialize)]
pub struct WatcherEvidence {
    /// Degradation records whose `recorded_at` falls inside the range.
    pub degradations: Vec<DegradationSummary>,
    /// Watcher event-log generations that exist on disk (0, 1, or 2:
    /// `events.log.1` then `events.log`).
    pub log_generations: u8,
    /// The oldest timestamp actually present in the log files, if any. A
    /// value after `since` means the log says nothing about the range's
    /// earlier portion — absence of events is not proof of absence of change.
    pub log_oldest_observation: Option<i64>,
    /// Watcher events inside the range, counted per kind.
    pub events_in_range: BTreeMap<String, u64>,
    pub first_event_in_range: Option<i64>,
    pub last_event_in_range: Option<i64>,
    /// Lines that could not be parsed as events (e.g. anomaly markers).
    /// Never silently dropped: counted and reported.
    pub unparseable_log_lines: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct DegradationSummary {
    pub reason: DegradationReason,
    pub detail: String,
    pub recorded_at: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct TimelineReport {
    pub schema_version: u64,
    /// The resolved half-open range in epoch microseconds…
    pub since: i64,
    pub until: i64,
    /// …and the same bounds as RFC 3339 UTC text. When `--until` was
    /// omitted, `until` *is* the invocation's now, so the output is always
    /// self-describing without a second wall-clock field (byte-identical
    /// JSON for identical inputs).
    pub since_text: String,
    pub until_text: String,
    /// True only when no unknown interval and no watcher degradation
    /// intersects the range. Any uncertainty makes it false.
    pub history_complete: bool,
    pub entries: Vec<TimelineEntry>,
    pub watcher: WatcherEvidence,
}

/// Everything the merge needs, resolved by the caller. Keeping this pure
/// makes ordering, boundaries, and honesty unit-testable without a catalog.
pub struct TimelineInput {
    pub since: i64,
    pub until: i64,
    /// The invocation's current time, used only as the effective end of open
    /// unknown intervals and the echoed `generated_at`.
    pub now: i64,
    pub operations: Vec<OperationRecord>,
    pub snapshots: Vec<SnapshotRow>,
    pub boundaries: Vec<BoundaryRow>,
    pub unknown_intervals: Vec<UnknownIntervalRow>,
    pub watcher: WatcherEvidence,
}

impl TimelineInput {
    /// Filters and merges every source into the deterministic report.
    pub fn build(self) -> TimelineReport {
        let TimelineInput {
            since,
            until,
            now,
            operations,
            snapshots,
            boundaries,
            unknown_intervals,
            watcher,
        } = self;

        let mut entries: Vec<TimelineEntry> = Vec::new();

        for operation in operations {
            if operation.created_at < since || operation.created_at >= until {
                continue;
            }
            entries.push(TimelineEntry {
                tier: Tier::Operation,
                timestamp: operation.created_at,
                end_timestamp: None,
                identifier: format!("operation {}", operation.id),
                summary: operation
                    .command
                    .clone()
                    .unwrap_or_else(|| format!("{:?} (no command)", operation.kind)),
                confidence: Some(operation.confidence.as_str().to_owned()),
                reversibility: Some(operation.reversibility.as_str().to_owned()),
                open: None,
            });
        }

        for snapshot in snapshots {
            if snapshot.created_at < since || snapshot.created_at >= until {
                continue;
            }
            entries.push(TimelineEntry {
                tier: Tier::Snapshot,
                timestamp: snapshot.created_at,
                end_timestamp: None,
                identifier: format!("snapshot {}", snapshot.name),
                summary: format!("state {}", snapshot.state_id),
                confidence: None,
                reversibility: None,
                open: None,
            });
        }

        for interval in unknown_intervals {
            // Wall-clock bounds for an interval are the *recorded* times:
            // opened at `created_at`, closed at `closed_at`, still extending
            // to the invocation's now when open. These are the best available
            // times, not proven event times, which is exactly why the entry
            // is rendered as uncertainty.
            let effective_end = interval.closed_at.unwrap_or(now);
            if interval.created_at < until && effective_end > since {
                entries.push(TimelineEntry {
                    tier: Tier::UnknownInterval,
                    timestamp: interval.created_at,
                    end_timestamp: interval.closed_at,
                    identifier: format!("unknown interval {}", interval.id),
                    summary: interval.reason.clone(),
                    confidence: None,
                    reversibility: None,
                    open: Some(interval.is_open),
                });
            }
        }

        for boundary in boundaries {
            // A boundary spans [started_at, ended_at]; an unaccounted
            // boundary is shown with no end rather than an invented one.
            let effective_end = boundary.ended_at.unwrap_or(boundary.started_at);
            if boundary.started_at < until && effective_end >= since {
                entries.push(TimelineEntry {
                    tier: Tier::PassiveBoundary,
                    timestamp: boundary.started_at,
                    end_timestamp: boundary.ended_at,
                    identifier: format!("boundary {}", boundary.id),
                    summary: boundary.command.clone(),
                    confidence: Some("LOW".to_owned()),
                    reversibility: Some("NOT_REVERSIBLE".to_owned()),
                    open: Some(boundary.ended_at.is_none()),
                });
            }
        }

        for degradation in &watcher.degradations {
            // Defense in depth: the evidence reader pre-filters, but the
            // merge itself enforces the range so a hand-built summary can
            // never leak an out-of-range record into the report.
            if degradation.recorded_at < since || degradation.recorded_at >= until {
                continue;
            }
            entries.push(TimelineEntry {
                tier: Tier::Watch,
                timestamp: degradation.recorded_at,
                end_timestamp: None,
                identifier: format!("watcher degradation {}", degradation.reason.as_str()),
                summary: degradation.detail.clone(),
                confidence: None,
                reversibility: None,
                open: None,
            });
        }

        // Deterministic ordering: (timestamp, tier rank, identifier).
        entries.sort_by(|left, right| {
            (left.timestamp, left.tier.rank(), left.identifier.as_str()).cmp(&(
                right.timestamp,
                right.tier.rank(),
                right.identifier.as_str(),
            ))
        });

        let any_unknown = watcher
            .degradations
            .iter()
            .any(|degradation| degradation.recorded_at >= since && degradation.recorded_at < until);
        let history_complete = !entries
            .iter()
            .any(|entry| entry.tier == Tier::UnknownInterval)
            && !any_unknown;

        TimelineReport {
            schema_version: TIMELINE_SCHEMA_VERSION,
            since,
            until,
            since_text: format_rfc3339(since),
            until_text: format_rfc3339(until),
            history_complete,
            entries,
            watcher,
        }
    }
}

/// Reads the watcher's advisory event logs (`events.log.1`, then
/// `events.log`) and summarizes what falls inside the range. Read-only:
/// missing logs are an empty summary, malformed lines are counted, and the
/// raw log is never treated as complete history.
pub fn read_watcher_evidence(
    events: &std::path::Path,
    events_previous: &std::path::Path,
    degradations_path: &std::path::Path,
    since: i64,
    until: i64,
) -> WatcherEvidence {
    let mut evidence = WatcherEvidence::default();
    for (path, exists) in [
        (events_previous, std::fs::metadata(events_previous).is_ok()),
        (events, std::fs::metadata(events).is_ok()),
    ] {
        if exists {
            evidence.log_generations += 1;
        }
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in content.lines() {
            let Ok(event) = serde_json::from_str::<crate::watch::model::FsEvent>(line) else {
                // Anomaly markers and rotated partial lines are counted, not
                // silently discarded.
                evidence.unparseable_log_lines += 1;
                continue;
            };
            if event.timestamp < evidence.log_oldest_observation.unwrap_or(i64::MAX) {
                evidence.log_oldest_observation = Some(event.timestamp);
            }
            if event.timestamp >= since && event.timestamp < until {
                let counter = evidence
                    .events_in_range
                    .entry(event.kind.as_str().to_owned())
                    .or_insert(0);
                *counter += 1;
                if evidence
                    .first_event_in_range
                    .is_none_or(|first| event.timestamp < first)
                {
                    evidence.first_event_in_range = Some(event.timestamp);
                }
                if evidence
                    .last_event_in_range
                    .is_none_or(|last| event.timestamp >= last)
                {
                    evidence.last_event_in_range = Some(event.timestamp);
                }
            }
        }
    }

    if let Some(marker) = crate::watch::model::read_degradations(degradations_path) {
        for record in marker.records {
            if record.recorded_at >= since && record.recorded_at < until {
                evidence.degradations.push(DegradationSummary {
                    reason: record.reason,
                    detail: record.detail,
                    recorded_at: record.recorded_at,
                });
            }
        }
    }
    evidence
}

/// True when the watcher log cannot speak for the whole range: either it
/// holds nothing at all, or its oldest observation postdates `since`.
pub fn watcher_log_may_predate_range(evidence: &WatcherEvidence, since: i64) -> bool {
    evidence
        .log_oldest_observation
        .is_none_or(|oldest| oldest > since)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Effect, OperationKind, OperationStatus, Reversibility, TrackingConfidence};
    use uuid::Uuid;

    const SINCE: i64 = 1_000_000_000;
    const UNTIL: i64 = 2_000_000_000;
    const NOW: i64 = 3_000_000_000;

    fn operation(id: i64, created_at: i64) -> OperationRecord {
        OperationRecord {
            id,
            workspace_id: Uuid::nil(),
            kind: OperationKind::Strong,
            status: OperationStatus::Completed,
            pre_state_id: Some("pre".to_owned()),
            post_state_id: Some("post".to_owned()),
            command: Some(format!("cmd-{id}")),
            cwd: None,
            exit_code: Some(0),
            confidence: TrackingConfidence::Tracked,
            reversibility: Reversibility::FullyReversible,
            error: None,
            created_at,
            effects: Vec::<Effect>::new(),
        }
    }

    fn boundary(id: &str, started_at: i64, ended_at: Option<i64>) -> BoundaryRow {
        BoundaryRow {
            id: id.to_owned(),
            session_id: "session".to_owned(),
            command: format!("shell-{id}"),
            cwd: "/ws".to_owned(),
            started_at,
            ended_at,
            exit_code: Some(0),
            consumed: true,
        }
    }

    fn interval(
        id: i64,
        created_at: i64,
        closed_at: Option<i64>,
        is_open: bool,
    ) -> UnknownIntervalRow {
        UnknownIntervalRow {
            id,
            start_state_id: None,
            end_state_id: None,
            reason: "watcher degradation: OVERFLOW".to_owned(),
            is_open,
            created_at,
            closed_at,
        }
    }

    fn snapshot(name: &str, created_at: i64) -> SnapshotRow {
        SnapshotRow {
            name: name.to_owned(),
            state_id: "state".to_owned(),
            created_at,
        }
    }

    fn degradation(recorded_at: i64) -> DegradationSummary {
        DegradationSummary {
            reason: DegradationReason::Overflow,
            detail: "platform watcher queue overflow".to_owned(),
            recorded_at,
        }
    }

    fn input(
        operations: Vec<OperationRecord>,
        snapshots: Vec<SnapshotRow>,
        boundaries: Vec<BoundaryRow>,
        intervals: Vec<UnknownIntervalRow>,
        watcher: WatcherEvidence,
    ) -> TimelineInput {
        TimelineInput {
            since: SINCE,
            until: UNTIL,
            now: NOW,
            operations,
            snapshots,
            boundaries,
            unknown_intervals: intervals,
            watcher,
        }
    }

    /// AC1: empty history produces a valid, complete, empty timeline.
    #[test]
    fn empty_history_is_a_valid_complete_empty_timeline() {
        let report = input(vec![], vec![], vec![], vec![], WatcherEvidence::default()).build();
        assert!(report.entries.is_empty());
        assert!(report.history_complete);
        assert_eq!(report.schema_version, TIMELINE_SCHEMA_VERSION);
        assert_eq!(report.since_text, format_rfc3339(SINCE));
    }

    /// AC2: since is inclusive, until is exclusive; adjacent ranges tile.
    #[test]
    fn range_boundaries_are_since_inclusive_and_until_exclusive() {
        let report = input(
            vec![
                operation(1, SINCE),     // exactly at since: included
                operation(2, UNTIL),     // exactly at until: excluded
                operation(3, UNTIL - 1), // one micro before until: included
                operation(4, SINCE - 1), // one micro before since: excluded
            ],
            vec![],
            vec![],
            vec![],
            WatcherEvidence::default(),
        )
        .build();
        let ids: Vec<String> = report
            .entries
            .iter()
            .map(|e| e.identifier.clone())
            .collect();
        assert_eq!(ids, vec!["operation 1", "operation 3"]);

        // Tiling: [SINCE, MID) ∪ [MID, UNTIL) contains each record once.
        let mid = 1_500_000_000;
        let first_half = TimelineInput {
            since: SINCE,
            until: mid,
            ..input(
                vec![operation(1, SINCE), operation(2, mid)],
                vec![],
                vec![],
                vec![],
                WatcherEvidence::default(),
            )
        }
        .build();
        let second_half = TimelineInput {
            since: mid,
            until: UNTIL,
            ..input(
                vec![operation(1, SINCE), operation(2, mid)],
                vec![],
                vec![],
                vec![],
                WatcherEvidence::default(),
            )
        }
        .build();
        assert_eq!(first_half.entries.len(), 1);
        assert_eq!(first_half.entries[0].identifier, "operation 1");
        assert_eq!(second_half.entries.len(), 1);
        assert_eq!(second_half.entries[0].identifier, "operation 2");
    }

    /// AC3: deterministic ordering by (timestamp, tier rank, identifier);
    /// byte-identical JSON for identical inputs.
    #[test]
    fn ordering_is_deterministic_and_json_is_stable() {
        let make = || {
            input(
                vec![operation(7, 1_500_000_000), operation(3, 1_200_000_000)],
                vec![snapshot("snap", 1_500_000_000)],
                vec![boundary("b", 1_400_000_000, Some(1_450_000_000))],
                vec![],
                WatcherEvidence::default(),
            )
        };
        let first = make().build();
        let second = make().build();
        let json_first = serde_json::to_string_pretty(&first).expect("json");
        let json_second = serde_json::to_string_pretty(&second).expect("json");
        assert_eq!(json_first, json_second);

        let tiers_and_stamps: Vec<(i64, Tier)> = first
            .entries
            .iter()
            .map(|e| (e.timestamp, e.tier))
            .collect();
        assert_eq!(
            tiers_and_stamps,
            vec![
                (1_200_000_000, Tier::Operation),
                (1_400_000_000, Tier::PassiveBoundary),
                // Same timestamp: snapshot ranks before operation? No —
                // OPERATION (0) ranks before SNAPSHOT (1).
                (1_500_000_000, Tier::Operation),
                (1_500_000_000, Tier::Snapshot),
            ]
        );
    }

    /// AC4: an unknown interval intersecting the range appears (whole, not
    /// trimmed) and forces history_complete false; an outside one does not.
    #[test]
    fn unknown_intervals_render_as_uncertainty_and_break_completeness() {
        // Open interval opened before the range, extending through now:
        // intersects the range and is shown whole with no invented end.
        let report = input(
            vec![],
            vec![],
            vec![],
            vec![interval(1, SINCE - 5_000_000, None, true)],
            WatcherEvidence::default(),
        )
        .build();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].tier, Tier::UnknownInterval);
        assert_eq!(report.entries[0].timestamp, SINCE - 5_000_000);
        assert_eq!(report.entries[0].end_timestamp, None);
        assert_eq!(report.entries[0].open, Some(true));
        assert!(!report.history_complete);

        // Closed interval entirely before the range: absent, completeness
        // unaffected.
        let report = input(
            vec![],
            vec![],
            vec![],
            vec![interval(2, SINCE - 9_000_000, Some(SINCE - 1), false)],
            WatcherEvidence::default(),
        )
        .build();
        assert!(report.entries.is_empty());
        assert!(report.history_complete);

        // Closed interval entirely after the range: absent.
        let report = input(
            vec![],
            vec![],
            vec![],
            vec![interval(3, UNTIL, Some(UNTIL + 1), false)],
            WatcherEvidence::default(),
        )
        .build();
        assert!(report.entries.is_empty());
        assert!(report.history_complete);
    }

    /// AC5: tier honesty — operations carry recorded confidence and
    /// reversibility; passive boundaries are labeled LOW and not reversible.
    #[test]
    fn tiers_carry_their_recorded_evidentiary_value() {
        let report = input(
            vec![operation(1, SINCE + 1)],
            vec![],
            vec![boundary("b1", SINCE + 2, None)],
            vec![],
            WatcherEvidence::default(),
        )
        .build();
        let operation_entry = report
            .entries
            .iter()
            .find(|e| e.tier == Tier::Operation)
            .expect("operation entry");
        assert_eq!(operation_entry.confidence.as_deref(), Some("TRACKED"));
        assert_eq!(
            operation_entry.reversibility.as_deref(),
            Some("FULLY_REVERSIBLE")
        );
        let boundary_entry = report
            .entries
            .iter()
            .find(|e| e.tier == Tier::PassiveBoundary)
            .expect("boundary entry");
        assert_eq!(boundary_entry.confidence.as_deref(), Some("LOW"));
        assert_eq!(
            boundary_entry.reversibility.as_deref(),
            Some("NOT_REVERSIBLE")
        );
        assert_eq!(
            boundary_entry.end_timestamp, None,
            "unaccounted boundary stays open"
        );
    }

    /// AC6: watcher degradations in range render as entries and break
    /// completeness; the event summary bounds itself to the range.
    #[test]
    fn watcher_degradations_break_completeness_and_events_summarize() {
        let watcher = WatcherEvidence {
            degradations: vec![degradation(SINCE + 100), degradation(UNTIL + 5)],
            events_in_range: BTreeMap::from([("CREATE".to_owned(), 3), ("MODIFY".to_owned(), 1)]),
            first_event_in_range: Some(SINCE + 10),
            last_event_in_range: Some(UNTIL - 10),
            log_generations: 2,
            log_oldest_observation: Some(SINCE - 60_000_000),
            unparseable_log_lines: 0,
        };

        let report = input(vec![], vec![], vec![], vec![], watcher).build();
        assert_eq!(report.entries.len(), 1, "only the in-range degradation");
        assert_eq!(report.entries[0].tier, Tier::Watch);
        assert!(!report.history_complete);
        assert_eq!(
            report.watcher.events_in_range.get("CREATE"),
            Some(&3),
            "BTreeMap keeps kind counts deterministic"
        );
        assert!(!watcher_log_may_predate_range(&report.watcher, SINCE));

        // A log whose oldest observation postdates `since` may be silent
        // about the range's start — and an empty log certainly is.
        let truncated = WatcherEvidence {
            log_oldest_observation: Some(SINCE + 60_000_000),
            ..WatcherEvidence::default()
        };
        assert!(watcher_log_may_predate_range(&truncated, SINCE));
        assert!(watcher_log_may_predate_range(
            &WatcherEvidence::default(),
            SINCE
        ));
    }

    /// AC4/AC2 interaction: an interval that ends exactly at `since` does
    /// not intersect the half-open range.
    #[test]
    fn interval_ending_exactly_at_since_does_not_intersect() {
        let report = input(
            vec![],
            vec![],
            vec![],
            vec![interval(1, SINCE - 100, Some(SINCE), false)],
            WatcherEvidence::default(),
        )
        .build();
        assert!(report.entries.is_empty());
        assert!(report.history_complete);
    }
}
