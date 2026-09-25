//! Phase 3: continuous observation — an optional, advisory, per-workspace
//! background watcher. See `.ai/PHASE_3_CONTINUOUS_OBSERVATION.md`.
//!
//! Layering, fixed by the contract:
//!
//! ```text
//! adapter (platform notifications) → serve loop (this module's run::serve_loop)
//!   → advisory artifacts under <store>/projects/<id>/watch/
//!   → degradation marker consumed by enforcement (workspace.rs)
//!   → unknown interval → reconciliation → authoritative state
//! ```
//!
//! The watcher process never opens the catalog, never acquires the writer
//! lease, and never writes inside the workspace root. Every artifact it
//! produces is advisory; only the enforcement point — running inside the
//! existing single-writer paths — turns a degradation into an unknown
//! interval, and only reconciliation closes it.

pub mod adapter;
#[cfg(windows)]
pub mod detach_windows;
pub mod lifecycle;
pub mod model;
pub mod run;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::error::{Result, RewindError};
use crate::model::WorkspaceCondition;
use crate::paths::{canonical_root, discover_marker, read_pointer};
use crate::workspace::Workspace;

use adapter::NotifyAdapter;
use model::{
    now_micros, read_degradations, read_json, DegradationReason, Degradations, DirtyIndex,
    StatusKind, WatcherState, MAX_BATCH_MS, MIN_BATCH_MS,
};
use run::{serve_loop, WatchConfig, WatchPaths};

/// The watcher's view of one workspace: identities and file paths only. This
/// is deliberately *not* a `Workspace`: no catalog, no store handles, no
/// lease — the watcher's context is pointer metadata plus paths.
#[derive(Clone, Debug)]
pub struct WatchContext {
    pub workspace_id: Uuid,
    pub workspace_root: PathBuf,
    /// `<store>/projects/<id>/` — the workspace's external store project
    /// directory, outside the workspace root.
    pub project_root: PathBuf,
    pub paths: WatchPaths,
}

impl WatchContext {
    /// Discovers and validates the workspace from a path inside it, using
    /// the same pointer-identity discipline as `Workspace::open_from_pointer`
    /// (`src/workspace.rs:210-243`): canonical root and store must match the
    /// recorded pointer exactly.
    pub fn discover(start: &Path) -> Result<Self> {
        let marker = discover_marker(start)?;
        if let Ok(metadata) = std::fs::symlink_metadata(&marker) {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(RewindError::PathEscape(format!(
                    "workspace marker is not a regular file: {}",
                    marker.display()
                )));
            }
        }
        let pointer = read_pointer(&marker)?;
        let rewind_dir = marker
            .parent()
            .ok_or_else(|| RewindError::WorkspaceIdentity("marker has no parent".to_owned()))?;
        let root = rewind_dir
            .parent()
            .ok_or_else(|| RewindError::WorkspaceIdentity("marker has no root".to_owned()))?;
        let root = canonical_root(root)?;
        if root.to_string_lossy() != pointer.root {
            return Err(RewindError::WorkspaceIdentity(format!(
                "canonical workspace root changed from {} to {}",
                pointer.root,
                root.display()
            )));
        }
        let store_path = PathBuf::from(&pointer.store_root);
        let store_root = std::fs::canonicalize(&store_path)?;
        if store_root.to_string_lossy() != pointer.store_root {
            return Err(RewindError::WorkspaceIdentity(format!(
                "canonical external store changed from {} to {}",
                pointer.store_root,
                store_root.display()
            )));
        }
        let project_root = store_root
            .join("projects")
            .join(pointer.workspace_id.to_string());
        let paths = WatchPaths::new(&project_root);
        Ok(Self {
            workspace_id: pointer.workspace_id,
            workspace_root: root,
            project_root,
            paths,
        })
    }
}

fn config_with_batch_override(batch_ms: Option<u64>) -> WatchConfig {
    let mut config = WatchConfig::from_env();
    if let Some(ms) = batch_ms {
        config.batch = Duration::from_millis(ms.clamp(MIN_BATCH_MS, MAX_BATCH_MS));
    }
    config
}

/// Is a watcher process credibly alive for this workspace? Derived from
/// heartbeat freshness (contract §6) — no pid queries.
fn watcher_is_running(state: &WatcherState) -> bool {
    matches!(
        state.status,
        StatusKind::Starting | StatusKind::Running | StatusKind::Stopping
    ) && lifecycle::heartbeat_is_fresh(state)
}

/// Writes the unobserved-gap degradation record for the interval between the
/// previous run's last durable observation and now (contract §10). A first
/// ever start — no prior state — records nothing: no coverage was claimed
/// before, so nothing is broken.
///
/// Public so the crash/restart and offline semantics (§10, §14, §15) are
/// verifiable directly; `start` calls this before any new coverage is
/// claimed.
pub fn record_start_gap(ctx: &WatchContext) -> Result<()> {
    let Some(state) = run::read_state(&ctx.paths) else {
        return Ok(());
    };
    let gap_to = now_micros();
    let gap_from = state.stopped_at.unwrap_or(state.heartbeat_at);
    let detail = format!(
        "previous watcher run (status {}) last observed {}; the interval until \
         this start is unobserved",
        state.status.as_str(),
        state.heartbeat_at
    );
    run::append_degradation(
        &ctx.paths,
        ctx.workspace_id,
        model::DegradationRecord::gap(DegradationReason::WatcherGap, gap_from, gap_to, detail),
    )
}

/// `rewind watch start`: refuses a second watcher, records the restart gap,
/// then either runs the loop in-process (`--foreground`) or spawns a
/// detached `rewind watch serve` child.
pub fn start(start: &Path, foreground: bool, batch_ms: Option<u64>) -> Result<i32> {
    let ctx = WatchContext::discover(start)?;
    let config = config_with_batch_override(batch_ms);

    if let Some(state) = run::read_state(&ctx.paths) {
        if watcher_is_running(&state) {
            eprintln!(
                "rewind: watcher already running for this workspace (pid {}, \
                 heartbeat {}); stop it first or run `rewind watch status`",
                state.pid, state.heartbeat_at
            );
            return Ok(3);
        }
    }

    record_start_gap(&ctx)?;
    let _ = std::fs::remove_file(&ctx.paths.stop_flag);
    let previous_run_id = run::read_state(&ctx.paths).map(|state| state.run_id);

    if foreground {
        let mut adapter = NotifyAdapter::new(&ctx.workspace_root)?;
        return match serve_loop(
            &ctx.paths,
            &ctx.workspace_root,
            ctx.workspace_id,
            &mut adapter,
            &config,
        )? {
            run::LoopExit::Stopped => Ok(0),
            run::LoopExit::Failed => Ok(1),
        };
    }

    let exe = std::env::current_exe()?;
    let arguments = [
        "watch".to_owned(),
        "serve".to_owned(),
        "--root".to_owned(),
        ctx.workspace_root.to_string_lossy().into_owned(),
        "--batch-ms".to_owned(),
        config.batch.as_millis().to_string(),
    ];
    let pid = spawn_detached(&exe, &arguments)?;
    let pid = pid as usize;

    // Wait until the child reports its first heartbeat so `start` can
    // distinguish a running watcher from one that died immediately: the
    // state file must carry a NEW run identity and a live status.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(state) = run::read_state(&ctx.paths) {
            if previous_run_id != Some(state.run_id) {
                match state.status {
                    StatusKind::Starting | StatusKind::Running => {
                        println!(
                            "watcher started pid {pid} run {} (batch {} ms)",
                            state.run_id, state.batch_ms
                        );
                        return Ok(0);
                    }
                    StatusKind::Failed => {
                        eprintln!(
                            "rewind: watcher failed during startup: {}",
                            state
                                .degraded_detail
                                .unwrap_or_else(|| "unknown cause".to_owned())
                        );
                        return Ok(3);
                    }
                    _ => {}
                }
            }
        }
        if Instant::now() >= deadline {
            eprintln!(
                "rewind: watcher (pid {pid}) did not report startup within 5s; \
                 inspect {}",
                ctx.paths.state.display()
            );
            return Ok(3);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Spawns a fully detached, non-inheriting watcher process.
///
/// On Windows this is a raw `CreateProcessW` with `bInheritHandles = FALSE`
/// and NUL std handles: the standard library always spawns with
/// `bInheritHandles = TRUE` and offers no way to restrict handle
/// inheritance (rust-lang/rust#73281), so a daemon spawned through
/// `std::process::Command` would inherit the caller's stdout/stderr pipe
/// handles and keep them open forever — any pipe-captured invocation
/// (`$(rewind watch start)`, `Command::output()`) would then block waiting
/// for an EOF the daemon never sends. The only unsafe code in this module
/// is this spawn, mirroring the `reparse_tag` precedent in `src/scan.rs`:
/// minimal surface, every handle closed on every return path.
///
/// On POSIX, the standard-library spawn with null stdio replaces file
/// descriptors 0/1/2 via `dup2`, which closes the caller's stdio pipes in
/// the child; `process_group(0)` detaches the terminal's process group.
/// Residual limitation (documented in the contract): other inherited
/// descriptors above 2 are not closed there.
#[cfg(windows)]
fn spawn_detached(exe: &Path, arguments: &[String]) -> Result<u32> {
    detach_windows::spawn_detached_no_inherit(exe, arguments)
}

#[cfg(not(windows))]
fn spawn_detached(exe: &Path, arguments: &[String]) -> Result<u32> {
    let mut command = std::process::Command::new(exe);
    command.args(arguments);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command.process_group(0);
    let child = command.spawn()?;
    Ok(child.id())
}

/// `rewind watch serve`: the detached loop itself. Internal; does not
/// perform the restart gap check (the launcher owns that) — running it
/// directly skips that conservative step by design.
pub fn serve(root: &Path, batch_ms: Option<u64>) -> Result<i32> {
    let ctx = WatchContext::discover(root)?;
    let config = config_with_batch_override(batch_ms);
    let mut adapter = NotifyAdapter::new(&ctx.workspace_root)?;
    match serve_loop(
        &ctx.paths,
        &ctx.workspace_root,
        ctx.workspace_id,
        &mut adapter,
        &config,
    )? {
        run::LoopExit::Stopped => Ok(0),
        run::LoopExit::Failed => Ok(1),
    }
}

/// `rewind watch status`: read-only lifecycle report. Exit 3 when operator
/// action is needed (pending reconciliation, degraded, or failed watcher).
pub fn status(start: &Path, json: bool) -> Result<i32> {
    let ctx = WatchContext::discover(start)?;
    let state = run::read_state(&ctx.paths);
    let degradations = read_degradations(&ctx.paths.degradations);
    let dirty = read_json::<DirtyIndex>(&ctx.paths.dirty);
    let lifecycle = lifecycle::derive(state.as_ref(), degradations.as_ref());

    let pending = degradations.filter(|marker| !marker.records.is_empty());
    let (dirty_paths, dirty_anomalies) = dirty
        .map(|index| (index.paths.len(), index.anomalies))
        .unwrap_or((0, 0));
    let recommendation = lifecycle::recommendation(lifecycle);
    let view = lifecycle::StatusView {
        workspace_id: ctx.workspace_id.to_string(),
        lifecycle,
        recommendation,
        state: state.as_ref(),
        pending_degradations: pending.as_ref(),
        dirty_paths,
        dirty_anomalies,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(lifecycle::exit_code(lifecycle));
    }

    println!("workspace {}", ctx.workspace_id);
    println!("watcher {}", lifecycle.as_str());
    if let Some(state) = &state {
        println!("pid {} status {}", state.pid, state.status.as_str());
        println!(
            "coverage starts at {} ({})",
            state.coverage_started_at, state.run_id
        );
        println!("heartbeat {}", state.heartbeat_at);
        if let Some(detail) = &state.degraded_detail {
            println!("degraded: {detail}");
        }
    }
    match &pending {
        Some(marker) => println!("pending degradations {}", marker.records.len()),
        None => println!("pending degradations 0"),
    }
    if let Some(marker) = &pending {
        for record in &marker.records {
            println!("  {} {}", record.reason.as_str(), record.detail);
        }
    }
    println!("dirty paths {dirty_paths} (anomalies {dirty_anomalies})");
    println!("recommendation: {recommendation}");
    Ok(lifecycle::exit_code(lifecycle))
}

/// `rewind watch stop`: requests a stop through the stop flag and waits a
/// bounded time. Never kills by pid; an unresponsive watcher is reported
/// honestly (exit 3).
pub fn stop(start: &Path) -> Result<i32> {
    let ctx = WatchContext::discover(start)?;
    let Some(state) = run::read_state(&ctx.paths) else {
        println!("watcher is not running");
        return Ok(0);
    };
    match state.status {
        StatusKind::Stopped | StatusKind::Failed => {
            println!(
                "watcher is not running (last status {})",
                state.status.as_str()
            );
            return Ok(0);
        }
        _ => {}
    }
    if !lifecycle::heartbeat_is_fresh(&state) {
        eprintln!(
            "rewind: watcher appears dead (last heartbeat {}); nothing to stop. \
             The unobserved interval will be gated when you restart with \
             `rewind watch start`",
            state.heartbeat_at
        );
        return Ok(3);
    }

    crate::paths::atomic_write(&ctx.paths.stop_flag, b"stop\n")?;
    let batch = Duration::from_millis(state.batch_ms.clamp(MIN_BATCH_MS, MAX_BATCH_MS));
    let deadline =
        Instant::now() + Duration::from_millis(2000) + batch * 2 + Duration::from_millis(1000);
    loop {
        match run::read_state(&ctx.paths) {
            Some(current) if current.status == StatusKind::Stopped => {
                println!("watcher stopped");
                return Ok(0);
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            eprintln!(
                "rewind: watcher (pid {}) did not stop within the bounded wait; \
                 inspect {}",
                state.pid,
                ctx.paths.dir.display()
            );
            return Ok(3);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The enforcement point (contract §12): converts pending watcher
/// degradations into an unknown interval plus `RECONCILIATION_REQUIRED`,
/// running exclusively inside existing lease-holding/diagnostic paths. The
/// marker is removed only after the interval and gate are durable, so a
/// crash before that point repeats idempotently.
pub fn enforce_watch_degradations(workspace: &Workspace) -> Result<()> {
    let paths = WatchPaths::new(&workspace.storage.project_root);
    let Some(degradations) = read_degradations(&paths.degradations) else {
        return Ok(());
    };
    if degradations.records.is_empty() {
        return Ok(());
    }
    let baseline = workspace.row()?.baseline_state;
    if !workspace.storage.catalog.has_open_unknown(workspace.id)? {
        workspace.storage.catalog.open_unknown(
            workspace.id,
            baseline.as_deref(),
            &degradation_summary(&degradations),
        )?;
    }
    workspace.storage.catalog.set_workspace(
        workspace.id,
        &WorkspaceCondition::ReconciliationRequired,
        baseline.as_deref(),
    )?;
    match std::fs::remove_file(&paths.degradations) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// One bounded catalog-reason string summarizing every pending record.
fn degradation_summary(degradations: &Degradations) -> String {
    const SUMMARY_LIMIT: usize = 512;
    let mut summary = String::from("watcher degradation:");
    for record in &degradations.records {
        let entry = format!(
            " {} {} at {}",
            record.reason.as_str(),
            record.detail,
            record.recorded_at
        );
        if summary.len() + entry.len() > SUMMARY_LIMIT {
            summary.push_str(" …(truncated)");
            break;
        }
        summary.push_str(&entry);
    }
    summary
}

/// Clears the advisory dirty index after a successful authoritative
/// reconciliation: the scan has observed everything, so "what may have
/// changed" resets. Best-effort by design (the artifact is advisory).
pub fn clear_dirty_index(project_root: &Path) {
    let paths = WatchPaths::new(project_root);
    if let Err(error) = std::fs::remove_file(&paths.dirty) {
        if error.kind() != std::io::ErrorKind::NotFound {
            eprintln!("rewind: could not clear the watcher dirty index: {error}");
        }
    }
}

/// True when a degradation marker with pending records exists.
pub fn has_pending_degradations(project_root: &Path) -> bool {
    let paths = WatchPaths::new(project_root);
    read_degradations(&paths.degradations).is_some_and(|marker| !marker.records.is_empty())
}

pub use model::{DegradationRecord, EventKind, EventSource, FsEvent, WATCH_SCHEMA_VERSION};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degradation_summary_is_bounded_and_names_reasons() {
        let marker = Degradations {
            schema_version: WATCH_SCHEMA_VERSION,
            workspace_id: Uuid::new_v4(),
            records: vec![
                model::DegradationRecord::new(DegradationReason::Overflow, "queue overflow"),
                model::DegradationRecord::gap(
                    DegradationReason::WatcherGap,
                    1_000,
                    2_000,
                    "gap detail",
                ),
            ],
        };
        let summary = degradation_summary(&marker);
        assert!(summary.starts_with("watcher degradation:"));
        assert!(summary.contains("OVERFLOW"));
        assert!(summary.contains("WATCHER_GAP"));
        assert!(summary.len() < 600);
    }

    #[test]
    fn watcher_is_running_requires_fresh_heartbeat() {
        let mut state = WatcherState::new(Uuid::new_v4(), Uuid::new_v4(), StatusKind::Running, 250);
        assert!(watcher_is_running(&state));
        state.heartbeat_at = now_micros() - 3_600_000_000;
        assert!(!watcher_is_running(&state));
        let mut stopped =
            WatcherState::new(Uuid::new_v4(), Uuid::new_v4(), StatusKind::Stopped, 250);
        assert!(!watcher_is_running(&stopped));
        stopped.status = StatusKind::Running;
        stopped.heartbeat_at = now_micros();
        assert!(watcher_is_running(&stopped));
    }
}
