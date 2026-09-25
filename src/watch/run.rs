//! Phase 3: the watcher serve loop.
//!
//! The loop is the only place the watcher turns adapter output into durable
//! artifacts. Its steady state does no scanning, no hashing, and no CAS
//! work (contract §3 C8): it appends raw evidence to `events.log`, coalesces
//! confined paths into `dirty.json`, heartbeats `state.json`, and appends
//! degradation records the moment its coverage claim breaks. It never opens
//! the catalog, never acquires the lease, and never writes inside the
//! workspace root.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::adapter::{AdapterOutput, EventAdapter, RawEvent, RawKind};
use super::model::{
    now_micros, read_json, write_json_atomic, DegradationReason, DegradationRecord, Degradations,
    DirtyIndex, EventKind, EventSource, FsEvent, StatusKind, WatcherState, DEFAULT_BATCH_MS,
    DEFAULT_DIRTY_CAP, DEFAULT_LOG_BYTES, HEARTBEAT_MS, MAX_BATCH_MS, MIN_BATCH_MS,
    WATCH_SCHEMA_VERSION,
};
use crate::error::{Result, RewindError};
use crate::paths::normalize_relative;

/// Watcher tuning, read once at serve start and recorded in `state.json` so
/// `watch status` can report what is actually in force.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchConfig {
    pub batch: Duration,
    pub dirty_cap: usize,
    pub log_bytes: u64,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            batch: Duration::from_millis(DEFAULT_BATCH_MS),
            dirty_cap: DEFAULT_DIRTY_CAP,
            log_bytes: DEFAULT_LOG_BYTES,
        }
    }
}

impl WatchConfig {
    /// Reads the documented environment knobs, clamped to the contract's
    /// bounds. Unparseable values fall back to defaults (the watcher must
    /// not refuse to run over a diagnostic knob).
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Some(value) = env_u64("REWIND_WATCH_BATCH_MS") {
            config.batch = Duration::from_millis(value.clamp(MIN_BATCH_MS, MAX_BATCH_MS));
        }
        if let Some(value) = env_u64("REWIND_WATCH_DIRTY_CAP") {
            config.dirty_cap = value.max(1) as usize;
        }
        if let Some(value) = env_u64("REWIND_WATCH_LOG_BYTES") {
            config.log_bytes = value.max(1024);
        }
        config
    }
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.trim().parse().ok()
}

/// The watcher's file set for one workspace.
#[derive(Clone, Debug)]
pub struct WatchPaths {
    pub dir: PathBuf,
    pub state: PathBuf,
    pub dirty: PathBuf,
    pub events: PathBuf,
    pub events_previous: PathBuf,
    pub degradations: PathBuf,
    pub stop_flag: PathBuf,
}

impl WatchPaths {
    pub fn new(project_root: &Path) -> Self {
        let dir = project_root.join("watch");
        Self {
            state: dir.join("state.json"),
            dirty: dir.join("dirty.json"),
            events: dir.join("events.log"),
            events_previous: dir.join("events.log.1"),
            degradations: dir.join("degraded.json"),
            stop_flag: dir.join("stop.flag"),
            dir,
        }
    }
}

/// Maps and confines one raw adapter event against the workspace root.
///
/// Returns the advisory events to record and the dirty paths they imply.
/// Events that cannot be confined (outside the root, non-Unicode) are
/// dropped and reported as anomalies — they are never followed, expanded, or
/// guessed at (contract §8). The root `.rewind` metadata directory is
/// excluded so watcher scope equals scanner scope.
pub fn confine_raw(
    raw: &RawEvent,
    root: &Path,
    run_id: Uuid,
    sequence: u64,
    source: EventSource,
    timestamp: i64,
) -> (Vec<FsEvent>, Vec<String>, Vec<String>) {
    let mut events = Vec::new();
    let mut dirty = Vec::new();
    let mut anomalies = Vec::new();

    let kind = match raw.kind {
        RawKind::Create => EventKind::Create,
        RawKind::Modify => EventKind::Modify,
        RawKind::Delete | RawKind::RenameFrom => EventKind::Delete,
        RawKind::RenameTo => EventKind::Create,
        RawKind::RenameBoth => EventKind::Rename,
        RawKind::Other => EventKind::Other,
    };

    // RenameBoth arrives as [from, to]; everything else is single-path.
    let (from_relative, primary) = if raw.kind == RawKind::RenameBoth {
        let from = raw.paths.first().and_then(|path| confine_path(root, path));
        let to = match raw.paths.get(1) {
            Some(path) => path,
            None => return (events, dirty, anomalies),
        };
        (from, to)
    } else {
        (
            None,
            match raw.paths.first() {
                Some(path) => path,
                None => return (events, dirty, anomalies),
            },
        )
    };

    let Some(relative) = confine_path(root, primary) else {
        anomalies.push(format!(
            "dropped unconfineable event path {}",
            primary.display()
        ));
        return (events, dirty, anomalies);
    };
    if relative == ".rewind" || relative.starts_with(".rewind/") {
        return (events, dirty, anomalies);
    }

    events.push(FsEvent {
        run_id,
        sequence,
        path: relative.clone(),
        kind,
        from_path: from_relative.clone(),
        timestamp,
        source,
    });
    dirty.push(relative);
    if let Some(from) = from_relative {
        dirty.push(from);
    }
    (events, dirty, anomalies)
}

/// Confines an absolute event path to the workspace root and returns its
/// normalized workspace-relative form.
fn confine_path(root: &Path, path: &Path) -> Option<String> {
    if !path.starts_with(root) {
        return None;
    }
    normalize_relative(root, path).ok()
}

/// The outcome of the serve loop.
#[derive(Debug, PartialEq, Eq)]
pub enum LoopExit {
    /// The stop flag was honored; final state is `STOPPED`.
    Stopped,
    /// The adapter failed past its restart attempt; final state is `FAILED`.
    /// The loop never exits with the workspace apparently fine *because of*
    /// the watcher: the degradation record is durable before this return.
    Failed,
}

/// Runs the watcher loop until the stop flag is seen or the adapter fails.
pub fn serve_loop(
    paths: &WatchPaths,
    root: &Path,
    workspace_id: Uuid,
    adapter: &mut dyn EventAdapter,
    config: &WatchConfig,
) -> Result<LoopExit> {
    crate::paths::ensure_directory(&paths.dir)?;
    let run_id = Uuid::new_v4();
    let started = now_micros();

    // The dirty index is cumulative across runs until a consumer clears it:
    // a restart must not erase what a previous run recorded.
    let mut index =
        read_json::<DirtyIndex>(&paths.dirty).unwrap_or_else(|| DirtyIndex::new(workspace_id));
    if index.schema_version != WATCH_SCHEMA_VERSION {
        index = DirtyIndex::new(workspace_id);
    }
    index.workspace_id = workspace_id;
    index.run_id = run_id;
    let mut dirty_set: BTreeSet<String> = index.paths.clone();

    let mut state = WatcherState::new(workspace_id, run_id, StatusKind::Running, {
        config.batch.as_millis() as u64
    });
    state.coverage_started_at = started;
    write_json_atomic(&paths.state, &state)?;

    let mut sequence: u64 = 0;
    let mut dirty_changed = false;
    let mut dirty_capped = false;
    let mut last_state_write = Instant::now();

    loop {
        match adapter.poll(config.batch) {
            AdapterOutput::Idle => {}
            AdapterOutput::Event(raw) => {
                sequence += 1;
                let (events, new_dirty, anomalies) =
                    confine_raw(&raw, root, run_id, sequence, event_source(), now_micros());
                for anomaly in anomalies {
                    index.anomalies += 1;
                    append_event_line(
                        &paths.events,
                        &paths.events_previous,
                        config.log_bytes,
                        &format!(
                            "{{\"anomaly\":{},\"ts\":{}}}",
                            json_string(&anomaly),
                            now_micros()
                        ),
                    )?;
                }
                for event in &events {
                    append_event_line(
                        &paths.events,
                        &paths.events_previous,
                        config.log_bytes,
                        &serde_json::to_string(event)?,
                    )?;
                }
                for path in new_dirty {
                    if dirty_set.len() >= config.dirty_cap {
                        if !dirty_capped {
                            dirty_capped = true;
                            let detail = format!(
                                "dirty index cap of {} reached; per-path knowledge \
                                 stops here until reconciliation",
                                config.dirty_cap
                            );
                            append_degradation(
                                paths,
                                workspace_id,
                                DegradationRecord::new(DegradationReason::DirtyCap, detail.clone()),
                            )?;
                            state.degrade(&detail);
                        }
                    } else if dirty_set.insert(path) {
                        dirty_changed = true;
                    }
                }
            }
            AdapterOutput::Overflow => {
                let detail =
                    "platform watcher queue overflow; events may have been lost".to_owned();
                append_degradation(
                    paths,
                    workspace_id,
                    DegradationRecord::new(DegradationReason::Overflow, detail.clone()),
                )?;
                state.degrade(&detail);
                // Resume observation for everything after this point. The
                // degradation record, not the restart, is what keeps the
                // covered interval unknown.
                if let Err(error) = adapter.restart() {
                    return fail_loop(adapter, paths, workspace_id, &mut state, &error.to_string());
                }
                write_json_atomic(&paths.state, &state)?;
                last_state_write = Instant::now();
            }
            AdapterOutput::Failed(detail) => {
                let detail = format!("watcher adapter failed: {detail}");
                append_degradation(
                    paths,
                    workspace_id,
                    DegradationRecord::new(DegradationReason::AdapterFailed, detail.clone()),
                )?;
                state.degrade(&detail);
                if adapter.restart().is_err() {
                    return fail_loop(adapter, paths, workspace_id, &mut state, &detail);
                }
                write_json_atomic(&paths.state, &state)?;
                last_state_write = Instant::now();
            }
        }

        if paths.stop_flag.exists() {
            state.status = StatusKind::Stopping;
            state.heartbeat_at = now_micros();
            write_json_atomic(&paths.state, &state)?;
            break;
        }

        if dirty_changed {
            index.paths = dirty_set.clone();
            index.updated_at = now_micros();
            write_json_atomic(&paths.dirty, &index)?;
            dirty_changed = false;
        }

        if last_state_write.elapsed() >= Duration::from_millis(HEARTBEAT_MS) {
            state.heartbeat_at = now_micros();
            state.last_sequence = sequence;
            write_json_atomic(&paths.state, &state)?;
            last_state_write = Instant::now();
        }
    }

    adapter.shutdown();
    state.status = StatusKind::Stopped;
    state.heartbeat_at = now_micros();
    state.stopped_at = Some(now_micros());
    state.last_sequence = sequence;
    write_json_atomic(&paths.state, &state)?;
    index.paths = dirty_set;
    index.updated_at = now_micros();
    write_json_atomic(&paths.dirty, &index)?;
    let _ = std::fs::remove_file(&paths.stop_flag);
    Ok(LoopExit::Stopped)
}

/// Terminates the loop after an unrecoverable adapter failure: the final
/// state is `FAILED` and the degradation record is already durable.
fn fail_loop(
    adapter: &mut dyn EventAdapter,
    paths: &WatchPaths,
    workspace_id: Uuid,
    state: &mut WatcherState,
    detail: &str,
) -> Result<LoopExit> {
    adapter.shutdown();
    append_degradation(
        paths,
        workspace_id,
        DegradationRecord::new(
            DegradationReason::AdapterFailed,
            format!("watcher stopped after unrecoverable adapter failure: {detail}"),
        ),
    )?;
    state.status = StatusKind::Failed;
    state.heartbeat_at = now_micros();
    write_json_atomic(&paths.state, state)?;
    Ok(LoopExit::Failed)
}

fn event_source() -> EventSource {
    if cfg!(target_os = "linux") {
        EventSource::Inotify
    } else if cfg!(target_os = "macos") {
        EventSource::FsEvents
    } else if cfg!(windows) {
        EventSource::Windows
    } else {
        EventSource::Fake
    }
}

fn append_event_line(
    events: &Path,
    events_previous: &Path,
    log_bytes: u64,
    line: &str,
) -> Result<()> {
    let incoming = line.len() as u64 + 1;
    if let Ok(metadata) = std::fs::metadata(events) {
        if metadata.len() + incoming > log_bytes {
            // One rotation generation: raw evidence is preserved up to the
            // documented cap; the catalog remains the durable history.
            let _ = std::fs::remove_file(events_previous);
            std::fs::rename(events, events_previous).map_err(|error| {
                RewindError::Storage(format!("rotate watcher event log: {error}"))
            })?;
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(events)?;
    writeln!(file, "{line}")?;
    file.flush()?;
    Ok(())
}

pub fn append_degradation(
    paths: &WatchPaths,
    workspace_id: Uuid,
    record: DegradationRecord,
) -> Result<()> {
    crate::paths::ensure_directory(&paths.dir)?;
    let mut degradations = read_json::<Degradations>(&paths.degradations).unwrap_or(Degradations {
        schema_version: WATCH_SCHEMA_VERSION,
        workspace_id,
        records: Vec::new(),
    });
    degradations.schema_version = WATCH_SCHEMA_VERSION;
    degradations.workspace_id = workspace_id;
    degradations.records.push(record);
    write_json_atomic(&paths.degradations, &degradations)
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

/// Reads the recorded watcher state, if any.
pub fn read_state(paths: &WatchPaths) -> Option<WatcherState> {
    read_json(&paths.state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/ws")
    }

    fn raw(kind: RawKind, paths: &[&str]) -> RawEvent {
        RawEvent {
            paths: paths.iter().map(PathBuf::from).collect(),
            kind,
        }
    }

    #[test]
    fn confines_inside_paths_to_workspace_relative() {
        let (events, dirty, anomalies) = confine_raw(
            &raw(RawKind::Modify, &["/ws/a/b.txt"]),
            &root(),
            Uuid::nil(),
            1,
            EventSource::Fake,
            100,
        );
        assert!(anomalies.is_empty());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].path, "a/b.txt");
        assert_eq!(events[0].kind, EventKind::Modify);
        assert_eq!(dirty, vec!["a/b.txt".to_owned()]);
    }

    #[test]
    fn drops_paths_outside_the_root_as_anomalies() {
        let (events, dirty, anomalies) = confine_raw(
            &raw(RawKind::Modify, &["/elsewhere/a.txt"]),
            &root(),
            Uuid::nil(),
            1,
            EventSource::Fake,
            100,
        );
        assert!(events.is_empty());
        assert!(dirty.is_empty());
        assert_eq!(anomalies.len(), 1);
        assert!(anomalies[0].contains("unconfineable"));
    }

    #[test]
    fn drops_rewind_metadata_events() {
        for path in ["/ws/.rewind", "/ws/.rewind/workspace.json"] {
            let (events, dirty, anomalies) = confine_raw(
                &raw(RawKind::Modify, &[path]),
                &root(),
                Uuid::nil(),
                1,
                EventSource::Fake,
                100,
            );
            assert!(events.is_empty() && dirty.is_empty() && anomalies.is_empty());
        }
    }

    #[test]
    fn rename_both_records_both_endpoints() {
        let (events, dirty, anomalies) = confine_raw(
            &raw(RawKind::RenameBoth, &["/ws/old.txt", "/ws/new.txt"]),
            &root(),
            Uuid::nil(),
            7,
            EventSource::Fake,
            100,
        );
        assert!(anomalies.is_empty());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::Rename);
        assert_eq!(events[0].path, "new.txt");
        assert_eq!(events[0].from_path.as_deref(), Some("old.txt"));
        assert_eq!(dirty.len(), 2);
        assert!(dirty.contains(&"old.txt".to_owned()));
        assert!(dirty.contains(&"new.txt".to_owned()));
    }

    #[test]
    fn rename_from_maps_to_delete_semantics() {
        let (events, dirty, _) = confine_raw(
            &raw(RawKind::RenameFrom, &["/ws/old.txt"]),
            &root(),
            Uuid::nil(),
            1,
            EventSource::Fake,
            100,
        );
        assert_eq!(events[0].kind, EventKind::Delete);
        assert_eq!(dirty, vec!["old.txt".to_owned()]);
    }

    #[test]
    fn platform_separator_paths_normalize_to_forward_slashes() {
        // On Windows the raw path uses the platform separator; the relative
        // form must always be '/'-separated to match the scanner.
        let absolute = if cfg!(windows) {
            PathBuf::from("C:\\ws\\a\\b.txt")
        } else {
            PathBuf::from("/ws/a/b.txt")
        };
        let workspace_root = if cfg!(windows) {
            PathBuf::from("C:\\ws")
        } else {
            root()
        };
        let (events, _, _) = confine_raw(
            &raw(RawKind::Modify, &[absolute.to_string_lossy().as_ref()]),
            &workspace_root,
            Uuid::nil(),
            1,
            EventSource::Fake,
            100,
        );
        assert_eq!(events[0].path, "a/b.txt");
    }

    #[test]
    fn config_env_is_clamped_to_contract_bounds() {
        std::env::set_var("REWIND_WATCH_BATCH_MS", "1");
        let config = WatchConfig::from_env();
        assert!(config.batch >= Duration::from_millis(MIN_BATCH_MS));
        std::env::set_var("REWIND_WATCH_BATCH_MS", "999999");
        let config = WatchConfig::from_env();
        assert!(config.batch <= Duration::from_millis(MAX_BATCH_MS));
        std::env::remove_var("REWIND_WATCH_BATCH_MS");
    }

    #[test]
    fn dirty_index_survives_a_restart_by_merging() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = WatchPaths::new(temp.path());
        let mut previous = DirtyIndex::new(Uuid::nil());
        previous.paths.insert("kept.txt".to_owned());
        write_json_atomic(&paths.dirty, &previous).expect("write dirty index");

        // The stop flag is present from the start: the loop polls once and
        // exits, which is enough to prove the final write merged the
        // previous run's index instead of replacing it.
        crate::paths::atomic_write(&paths.stop_flag, b"stop\n").expect("stop flag");
        let mut adapter = super::super::adapter::FakeAdapter::new([]);
        let config = WatchConfig {
            batch: Duration::from_millis(MIN_BATCH_MS),
            ..WatchConfig::default()
        };
        let exit = serve_loop(&paths, temp.path(), Uuid::nil(), &mut adapter, &config);
        assert_eq!(exit.expect("loop"), LoopExit::Stopped);
        let index = read_json::<DirtyIndex>(&paths.dirty).expect("dirty index");
        assert!(
            index.paths.contains("kept.txt"),
            "a restart must not erase the previous run's evidence"
        );
    }
}
