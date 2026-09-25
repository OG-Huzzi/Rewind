//! Phase 3: the platform-neutral watcher event model and its durable
//! artifacts.
//!
//! Everything in this module is **advisory** (see
//! `.ai/PHASE_3_CONTINUOUS_OBSERVATION.md` §4, §6): a watcher event means
//! "this path may have changed", never "this path now equals X". The
//! artifacts defined here live under `<store>/projects/<id>/watch/`, outside
//! the workspace, and are the watcher's *only* durable outputs — the watcher
//! never opens the catalog and never writes inside the workspace root.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{Result, RewindError};

pub const WATCH_SCHEMA_VERSION: u32 = 1;

/// Default coalescing/batch window. One adapter drain per window; the dirty
/// index collapses duplicates across windows anyway, so this only bounds
/// latency and write frequency.
pub const DEFAULT_BATCH_MS: u64 = 250;
pub const MIN_BATCH_MS: u64 = 50;
pub const MAX_BATCH_MS: u64 = 5000;

/// Heartbeat cadence and the freshness threshold derived from it. Liveness
/// is derived from heartbeat freshness, not pid queries: a stale heartbeat
/// means the watcher is presumed dead (conservatively).
pub const HEARTBEAT_MS: u64 = 2000;

/// Dirty-index path cap. At the cap the watcher stops adding paths and
/// degrades: the interval becomes unknown rather than memory unbounded.
pub const DEFAULT_DIRTY_CAP: usize = 100_000;

/// Raw event-log size cap; the log rotates to one `.1` generation.
pub const DEFAULT_LOG_BYTES: u64 = 8 * 1024 * 1024;

pub fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_micros() as i64)
        .unwrap_or(0)
}

/// What a watcher event claims. There is exactly one confidence for watcher
/// evidence — advisory — so unlike Phase 2 edges this is not a field.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EventKind {
    Create,
    Modify,
    Delete,
    Rename,
    Other,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "CREATE",
            Self::Modify => "MODIFY",
            Self::Delete => "DELETE",
            Self::Rename => "RENAME",
            Self::Other => "OTHER",
        }
    }
}

/// The platform mechanism that observed the event (provenance).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EventSource {
    Inotify,
    FsEvents,
    Windows,
    Fake,
}

impl EventSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inotify => "INOTIFY",
            Self::FsEvents => "FSEVENTS",
            Self::Windows => "WINDOWS",
            Self::Fake => "FAKE",
        }
    }
}

/// One advisory filesystem observation.
///
/// `timestamp` is the OBSERVATION time (when Rewind saw the event), not the
/// occurrence time: platform event times are not portable evidence.
/// `sequence` is arrival order within the run identified by `run_id`; it is
/// provenance, not a claim about the order things happened on disk.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FsEvent {
    pub run_id: Uuid,
    pub sequence: u64,
    pub path: String,
    pub kind: EventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_path: Option<String>,
    pub timestamp: i64,
    pub source: EventSource,
}

/// Why the watcher declared itself unreliable. Every variant makes the
/// covered interval unknown until reconciliation closes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DegradationReason {
    /// The platform queue overflowed (events may have been lost).
    Overflow,
    /// The interval since the previous run's last durable heartbeat was not
    /// observed (crash, kill, machine sleep, or simply being stopped).
    WatcherGap,
    /// The adapter itself failed.
    AdapterFailed,
    /// The dirty index hit its cap; beyond it the watcher stops claiming
    /// per-path knowledge.
    DirtyCap,
}

impl DegradationReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Overflow => "OVERFLOW",
            Self::WatcherGap => "WATCHER_GAP",
            Self::AdapterFailed => "ADAPTER_FAILED",
            Self::DirtyCap => "DIRTY_CAP",
        }
    }
}

/// One pending degradation record. This is the immediate conservative record
/// required by the contract (§3 C3): written the moment the watcher stops
/// being able to claim coverage, before anything else happens.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DegradationRecord {
    pub reason: DegradationReason,
    pub detail: String,
    pub recorded_at: i64,
    /// For `WATCHER_GAP`: the unobserved span.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap_from: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap_to: Option<i64>,
}

impl DegradationRecord {
    pub fn new(reason: DegradationReason, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
            recorded_at: now_micros(),
            gap_from: None,
            gap_to: None,
        }
    }

    pub fn gap(reason: DegradationReason, from: i64, to: i64, detail: impl Into<String>) -> Self {
        Self {
            gap_from: Some(from),
            gap_to: Some(to),
            ..Self::new(reason, detail)
        }
    }
}

/// The pending degradation marker file. Read by the enforcement point
/// (`crate::watch::enforce_watch_degradations`), never by the watcher itself
/// once written.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Degradations {
    pub schema_version: u32,
    pub workspace_id: Uuid,
    pub records: Vec<DegradationRecord>,
}

/// Recorded watcher status. Written by the watcher process; `FAILED` is also
/// *derived* for a stale heartbeat while the recorded status says RUNNING
/// (the derivation lives in `super::lifecycle`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StatusKind {
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
}

impl StatusKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "STARTING",
            Self::Running => "RUNNING",
            Self::Stopping => "STOPPING",
            Self::Stopped => "STOPPED",
            Self::Failed => "FAILED",
        }
    }
}

/// The watcher's state/heartbeat file. The pid is diagnostic only — liveness
/// is heartbeat freshness (§6).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WatcherState {
    pub schema_version: u32,
    pub workspace_id: Uuid,
    pub run_id: Uuid,
    pub pid: u32,
    pub status: StatusKind,
    pub started_at: i64,
    pub heartbeat_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<i64>,
    /// When this run's observation coverage begins. Nothing earlier is ever
    /// claimed observed.
    pub coverage_started_at: i64,
    pub batch_ms: u64,
    pub degraded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded_detail: Option<String>,
    pub last_sequence: u64,
}

impl WatcherState {
    pub fn new(workspace_id: Uuid, run_id: Uuid, status: StatusKind, batch_ms: u64) -> Self {
        let now = now_micros();
        Self {
            schema_version: WATCH_SCHEMA_VERSION,
            workspace_id,
            run_id,
            pid: std::process::id(),
            status,
            started_at: now,
            heartbeat_at: now,
            stopped_at: None,
            coverage_started_at: now,
            batch_ms,
            degraded: false,
            degraded_detail: None,
            last_sequence: 0,
        }
    }

    /// Marks the state degraded, keeping the earliest cause visible.
    pub fn degrade(&mut self, detail: &str) {
        self.degraded = true;
        let detail_line = format!("{}: {}", chrono_display(now_micros()), detail);
        match &self.degraded_detail {
            Some(existing) => {
                self.degraded_detail = Some(format!("{existing}; {detail_line}"));
            }
            None => self.degraded_detail = Some(detail_line),
        }
    }
}

/// The coalesced dirty-path index: the union of workspace-relative paths that
/// *may have changed*. Cumulative across watcher runs until a consumer clears
/// it; serialized deterministically (sorted set) per contract C7.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DirtyIndex {
    pub schema_version: u32,
    pub workspace_id: Uuid,
    pub run_id: Uuid,
    pub updated_at: i64,
    pub paths: BTreeSet<String>,
    pub anomalies: u64,
}

impl DirtyIndex {
    pub fn new(workspace_id: Uuid) -> Self {
        Self {
            schema_version: WATCH_SCHEMA_VERSION,
            workspace_id,
            run_id: Uuid::nil(),
            updated_at: 0,
            paths: BTreeSet::new(),
            anomalies: 0,
        }
    }
}

/// Renders a microsecond timestamp for human diagnostics only.
fn chrono_display(micros: i64) -> String {
    let secs = micros / 1_000_000;
    // Civil-from-days (Howard Hinnant's algorithm); display only.
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (h, m, s) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

pub fn write_json_atomic(path: &std::path::Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    crate::paths::atomic_write(path, &bytes)
}

pub fn read_json<T: for<'de> Deserialize<'de>>(path: &std::path::Path) -> Option<T> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Reads the pending degradation records, treating an *unreadable* marker as
/// pending (conservative): the watcher recorded something this process can no
/// longer parse, and that uncertainty must reach the gate, not vanish.
pub fn read_degradations(path: &std::path::Path) -> Option<Degradations> {
    let bytes = std::fs::read(path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(parsed) => Some(parsed),
        Err(_) => Some(Degradations {
            schema_version: WATCH_SCHEMA_VERSION,
            workspace_id: Uuid::nil(),
            records: vec![DegradationRecord::new(
                DegradationReason::AdapterFailed,
                "degradation marker file unreadable; treated as pending",
            )],
        }),
    }
}

pub fn marker_missing_error(workspace_root: &std::path::Path) -> RewindError {
    RewindError::WorkspaceNotInitialized
        .context(format!("watcher context for {}", workspace_root.display()))
}

/// Helper trait so the watcher can attach context to existing errors without
/// introducing a new error enum.
pub trait ErrorContext {
    fn context(self, detail: impl Into<String>) -> Self;
}

impl ErrorContext for RewindError {
    fn context(self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        match self {
            // Keep typed safety errors intact; annotate the transport-ish ones.
            RewindError::Storage(inner) => RewindError::Storage(format!("{detail}: {inner}")),
            RewindError::Io(inner) => RewindError::Storage(format!("{detail}: {inner}")),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirty_index_serialization_is_deterministic() {
        let mut first = DirtyIndex::new(Uuid::nil());
        first.paths.insert("b.txt".to_owned());
        first.paths.insert("a.txt".to_owned());
        first.updated_at = 42;
        let mut second = DirtyIndex::new(Uuid::nil());
        second.paths.insert("a.txt".to_owned());
        second.paths.insert("b.txt".to_owned());
        second.updated_at = 42;
        let left = serde_json::to_string(&first).expect("serialize");
        let right = serde_json::to_string(&second).expect("serialize");
        assert_eq!(
            left, right,
            "BTreeSet ordering must make output byte-identical"
        );
    }

    #[test]
    fn degradation_record_serializes_reason_and_gap() {
        let record = DegradationRecord::gap(
            DegradationReason::WatcherGap,
            1_000,
            2_000,
            "previous run last seen",
        );
        let text = serde_json::to_string(&record).expect("serialize");
        assert!(text.contains("WATCHER_GAP"));
        assert!(text.contains("gap_from"));
        let parsed: DegradationRecord = serde_json::from_str(&text).expect("round trip");
        assert_eq!(parsed.gap_from, Some(1_000));
        assert_eq!(parsed.gap_to, Some(2_000));
    }

    #[test]
    fn unreadable_degradation_marker_reads_as_pending() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("degraded.json");
        std::fs::write(&path, b"{not json").expect("corrupt marker");
        let parsed = read_degradations(&path).expect("pending degradation");
        assert_eq!(parsed.records.len(), 1);
        assert!(parsed.records[0].detail.contains("unreadable"));
    }

    #[test]
    fn state_degrade_keeps_earliest_cause_visible() {
        let mut state = WatcherState::new(Uuid::new_v4(), Uuid::new_v4(), StatusKind::Running, 250);
        state.degrade("queue overflow");
        state.degrade("another cause");
        let detail = state.degraded_detail.expect("detail recorded");
        assert!(detail.contains("queue overflow"));
        assert!(detail.contains("another cause"));
    }

    #[test]
    fn chrono_display_formats_civil_time() {
        assert_eq!(chrono_display(0), "1970-01-01T00:00:00Z");
        // 2026-09-24T00:00:00Z (leap years included in the civil algorithm).
        assert_eq!(
            chrono_display(1_790_208_000 * 1_000_000),
            "2026-09-24T00:00:00Z"
        );
    }
}
