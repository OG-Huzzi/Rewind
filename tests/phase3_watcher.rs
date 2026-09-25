//! Phase 3 verification: the watcher is advisory, its uncertainty is durable
//! and conservative, and nothing in Phase 1/2 moved. See
//! `.ai/PHASE_3_CONTINUOUS_OBSERVATION.md` §15 for the obligations this
//! suite owns.
//!
//! The state-machine tests drive the real serve loop with the deterministic
//! `FakeAdapter` (contract §5) so overflow and failure paths do not depend
//! on platform scheduling; the lifecycle tests run the real adapter through
//! the real CLI, including the detached spawn path.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rewind::model::WorkspaceCondition;
use rewind::watch::adapter::{AdapterOutput, FakeAdapter, RawEvent, RawKind};
use rewind::watch::lifecycle::{self, Lifecycle};
use rewind::watch::model::{
    now_micros, read_degradations, DegradationReason, DirtyIndex, StatusKind, WatcherState,
};
use rewind::watch::run::{serve_loop, LoopExit, WatchConfig};
use rewind::watch::{self, WatchContext};
use rewind::workspace::Workspace;

mod common;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A real initialized workspace with an external store, plus the watcher
/// context derived from it.
struct Fixture {
    root: PathBuf,
    _store: tempfile::TempDir,
    ctx: WatchContext,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("temporary root").keep();
    let store = tempfile::tempdir().expect("temporary store");
    fs::write(root.join("foo.txt"), b"A").expect("write fixture");
    let workspace = Workspace::init(&root, Some(store.path())).expect("initialize");
    let workspace_id = workspace.id;
    drop(workspace);
    let ctx = WatchContext::discover(&root).expect("watch context");
    assert_eq!(ctx.workspace_id, workspace_id);
    Fixture {
        root,
        _store: store,
        ctx,
    }
}

fn config(batch_ms: u64) -> WatchConfig {
    WatchConfig {
        batch: Duration::from_millis(batch_ms),
        ..WatchConfig::default()
    }
}

fn raw(kind: RawKind, paths: &[PathBuf]) -> AdapterOutput {
    AdapterOutput::Event(RawEvent {
        paths: paths.to_vec(),
        kind,
    })
}

/// Runs the serve loop on a thread and returns a handle. The loop exits only
/// through the stop flag, so tests observe durable effects first, then stop.
fn spawn_loop(
    ctx: &WatchContext,
    script: Vec<AdapterOutput>,
    watch_config: &WatchConfig,
) -> std::thread::JoinHandle<LoopExit> {
    let paths = ctx.paths.clone();
    let root = ctx.workspace_root.clone();
    let workspace_id = ctx.workspace_id;
    let watch_config = *watch_config;
    let mut adapter = FakeAdapter::new(script);
    std::thread::spawn(move || {
        serve_loop(&paths, &root, workspace_id, &mut adapter, &watch_config).expect("serve loop")
    })
}

/// Waits until `condition` holds or the deadline passes. All waits are
/// poll-based; no test depends on wall-clock timing.
fn wait_until(deadline: Duration, condition: impl Fn() -> bool) -> bool {
    let started = Instant::now();
    while started.elapsed() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    condition()
}

fn request_stop(ctx: &WatchContext) {
    rewind::paths::atomic_write(&ctx.paths.stop_flag, b"stop\n").expect("stop flag");
}

fn degradations(ctx: &WatchContext) -> Vec<DegradationReason> {
    read_degradations(&ctx.paths.degradations)
        .map(|marker| marker.records.iter().map(|record| record.reason).collect())
        .unwrap_or_default()
}

fn dirty_paths(ctx: &WatchContext) -> Vec<String> {
    let text = fs::read_to_string(&ctx.paths.dirty).unwrap_or_default();
    let index: DirtyIndex = serde_json::from_str(&text).unwrap_or_default();
    index.paths.into_iter().collect()
}

/// Recursively collects sorted (relative path, length) for every file under
/// `root`, skipping nothing — used to prove the watcher wrote nothing inside
/// the workspace.
fn tree_fingerprint(root: &Path) -> Vec<(String, u64)> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, u64)>) {
        for entry in fs::read_dir(dir).expect("read dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                let relative = path
                    .strip_prefix(base)
                    .expect("inside root")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((relative, fs::metadata(&path).expect("meta").len()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn run_cli(args: &[&str], cwd: &Path) -> (i32, String, String) {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_rewind"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn rewind");
    (
        output.status.code().unwrap_or(1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

// ---------------------------------------------------------------------------
// Coalescing and evidence preservation
// ---------------------------------------------------------------------------

#[test]
fn repeated_modifies_coalesce_into_one_dirty_entry_without_losing_evidence() {
    let fixture = fixture();
    let path = fixture.ctx.workspace_root.join("foo.txt");
    let script = (0..5)
        .map(|_| raw(RawKind::Modify, std::slice::from_ref(&path)))
        .collect();
    let handle = spawn_loop(&fixture.ctx, script, &config(50));

    assert!(
        wait_until(Duration::from_secs(10), || dirty_paths(&fixture.ctx)
            == ["foo.txt"]),
        "dirty index must contain foo.txt exactly"
    );
    request_stop(&fixture.ctx);
    assert_eq!(handle.join().expect("join"), LoopExit::Stopped);

    // The index collapsed the duplicates…
    assert_eq!(dirty_paths(&fixture.ctx), ["foo.txt"]);
    // …and the raw evidence survives untouched: five MODIFY events on the
    // log, none erased by coalescing (contract §9).
    let log = fs::read_to_string(&fixture.ctx.paths.events).expect("event log");
    let modifies = log
        .lines()
        .filter(|line| line.contains("\"kind\":\"MODIFY\"") && line.contains("foo.txt"))
        .count();
    assert_eq!(modifies, 5, "raw events must be preserved in events.log");
}

#[test]
fn create_then_delete_in_one_run_leaves_the_path_dirty() {
    let fixture = fixture();
    let path = fixture.ctx.workspace_root.join("temp.txt");
    let script = vec![
        raw(RawKind::Create, std::slice::from_ref(&path)),
        raw(RawKind::Delete, std::slice::from_ref(&path)),
    ];
    let handle = spawn_loop(&fixture.ctx, script, &config(50));
    assert!(
        wait_until(Duration::from_secs(10), || dirty_paths(&fixture.ctx)
            == ["temp.txt"]),
        "the path is dirty: it may have changed"
    );
    request_stop(&fixture.ctx);
    assert_eq!(handle.join().expect("join"), LoopExit::Stopped);

    // No create/delete reconstruction: the index states uncertainty, and the
    // log keeps both endpoint events for inspection.
    let log = fs::read_to_string(&fixture.ctx.paths.events).expect("event log");
    assert!(log.contains("\"kind\":\"CREATE\""));
    assert!(log.contains("\"kind\":\"DELETE\""));
}

#[test]
fn the_dirty_index_is_serialized_sorted_and_deterministic() {
    let fixture = fixture();
    let paths: Vec<PathBuf> = ["z.txt", "a.txt", "m.txt"]
        .iter()
        .map(|name| fixture.ctx.workspace_root.join(name))
        .collect();
    let script = paths
        .iter()
        .map(|path| raw(RawKind::Modify, std::slice::from_ref(path)))
        .collect();
    let handle = spawn_loop(&fixture.ctx, script, &config(50));
    assert!(wait_until(Duration::from_secs(10), || dirty_paths(
        &fixture.ctx
    )
    .len()
        == 3));
    request_stop(&fixture.ctx);
    assert_eq!(handle.join().expect("join"), LoopExit::Stopped);

    assert_eq!(
        dirty_paths(&fixture.ctx),
        vec!["a.txt", "m.txt", "z.txt"],
        "BTreeSet serialization is sorted regardless of arrival order"
    );
}

// ---------------------------------------------------------------------------
// Overflow: the degradation path and the authoritative gate
// ---------------------------------------------------------------------------

#[test]
fn overflow_records_a_degradation_keeps_evidence_and_never_gates_by_itself() {
    let fixture = fixture();
    let catalog_path = fixture.ctx.project_root.join("metadata.sqlite");
    let catalog_before = fs::read(&catalog_path).expect("catalog bytes");
    let tree_before = tree_fingerprint(&fixture.root);

    let foo = fixture.ctx.workspace_root.join("foo.txt");
    let bar = fixture.ctx.workspace_root.join("bar.txt");
    let script = vec![
        raw(RawKind::Modify, std::slice::from_ref(&foo)),
        AdapterOutput::Overflow,
        raw(RawKind::Modify, std::slice::from_ref(&bar)),
    ];
    let handle = spawn_loop(&fixture.ctx, script, &config(50));
    assert!(
        wait_until(Duration::from_secs(10), || degradations(&fixture.ctx)
            .contains(&DegradationReason::Overflow)),
        "overflow must append a durable degradation record immediately"
    );
    request_stop(&fixture.ctx);
    assert_eq!(handle.join().expect("join"), LoopExit::Stopped);

    // The record exists, the dirty set survived the overflow, and the
    // watcher degraded itself.
    assert_eq!(
        degradations(&fixture.ctx),
        vec![DegradationReason::Overflow]
    );
    assert_eq!(dirty_paths(&fixture.ctx), vec!["bar.txt", "foo.txt"]);
    let state: WatcherState =
        serde_json::from_str(&fs::read_to_string(&fixture.ctx.paths.state).expect("state"))
            .expect("state json");
    assert!(state.degraded);
    assert!(state
        .degraded_detail
        .as_deref()
        .unwrap_or_default()
        .contains("overflow"));

    // Isolation: the watcher opened no catalog and wrote nothing inside the
    // workspace (contract §3 C2).
    assert_eq!(
        fs::read(&catalog_path).expect("catalog bytes"),
        catalog_before,
        "the watcher must not touch the catalog"
    );
    assert_eq!(
        tree_fingerprint(&fixture.root),
        tree_before,
        "the watcher must not write inside the workspace root"
    );

    // The watcher never gates the workspace by itself…
    let workspace = Workspace::open_from_current(&fixture.root).expect("open");
    assert_eq!(
        workspace.condition().expect("condition"),
        WorkspaceCondition::Healthy
    );

    // …but the next enforcement point converts the pending marker into an
    // unknown interval and RECONCILIATION_REQUIRED (contract §12).
    workspace.enforce_pending_safety_gate().expect("enforce");
    assert_eq!(
        workspace.condition().expect("condition"),
        WorkspaceCondition::ReconciliationRequired
    );
    assert!(workspace
        .storage
        .catalog
        .has_open_unknown(workspace.id)
        .expect("open unknown"));
    assert!(
        !fixture.ctx.paths.degradations.exists(),
        "the marker is consumed only after the interval and gate are durable"
    );

    // Only reconciliation closes the uncertainty.
    let state_id = workspace
        .reconcile_locked("verification reconciliation")
        .expect("reconcile");
    assert_eq!(
        workspace.condition().expect("condition"),
        WorkspaceCondition::Healthy
    );
    assert!(!workspace
        .storage
        .catalog
        .has_open_unknown(workspace.id)
        .expect("unknown closed"));
    assert_eq!(workspace.baseline_id().expect("baseline"), state_id);
    // A second enforcement is a no-op — no duplicate interval.
    workspace
        .enforce_pending_safety_gate()
        .expect("enforce again");
    assert_eq!(
        workspace.condition().expect("condition"),
        WorkspaceCondition::Healthy
    );
}

#[test]
fn an_unrecoverable_adapter_failure_lands_failed_with_a_degradation_record() {
    let fixture = fixture();
    let mut adapter = FakeAdapter::new([AdapterOutput::Failed("injected failure".to_owned())])
        .with_failing_restarts();
    let paths = fixture.ctx.paths.clone();
    let root = fixture.root.clone();
    let workspace_id = fixture.ctx.workspace_id;
    let watch_config = config(50);
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let exit = serve_loop(&paths, &root, workspace_id, &mut adapter, &watch_config)
            .expect("serve loop");
        let _ = done_tx.send(exit);
    });
    let exit = done_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("loop exits without a stop flag on unrecoverable failure");
    assert_eq!(exit, LoopExit::Failed);
    assert_eq!(
        degradations(&fixture.ctx),
        vec![
            DegradationReason::AdapterFailed,
            DegradationReason::AdapterFailed
        ],
        "one record for the failure, one for the abandoned restart"
    );
    let state: WatcherState =
        serde_json::from_str(&fs::read_to_string(&fixture.ctx.paths.state).expect("state"))
            .expect("state json");
    assert_eq!(state.status, StatusKind::Failed);
    let derived = lifecycle::derive(
        Some(&state),
        read_degradations(&fixture.ctx.paths.degradations).as_ref(),
    );
    // A pending marker dominates: the workspace gate outranks the watcher's
    // own state (contract §7).
    assert_eq!(derived, Lifecycle::ReconciliationRequired);
}

// ---------------------------------------------------------------------------
// Resource bounds
// ---------------------------------------------------------------------------

#[test]
fn the_dirty_cap_degrades_instead_of_growing_unbounded() {
    let fixture = fixture();
    let paths: Vec<PathBuf> = ["a.txt", "b.txt", "c.txt", "d.txt"]
        .iter()
        .map(|name| fixture.ctx.workspace_root.join(name))
        .collect();
    let script = paths
        .iter()
        .map(|path| raw(RawKind::Modify, std::slice::from_ref(path)))
        .collect();
    let watch_config = WatchConfig {
        batch: Duration::from_millis(50),
        dirty_cap: 2,
        ..WatchConfig::default()
    };
    let handle = spawn_loop(&fixture.ctx, script, &watch_config);
    assert!(
        wait_until(Duration::from_secs(10), || degradations(&fixture.ctx)
            .contains(&DegradationReason::DirtyCap)),
        "hitting the cap must degrade, not grow"
    );
    request_stop(&fixture.ctx);
    assert_eq!(handle.join().expect("join"), LoopExit::Stopped);
    let index: DirtyIndex =
        serde_json::from_str(&fs::read_to_string(&fixture.ctx.paths.dirty).expect("dirty"))
            .expect("dirty json");
    assert!(
        index.paths.len() <= 2,
        "no paths are added past the cap: {:?}",
        index.paths
    );
}

// ---------------------------------------------------------------------------
// Scope and confinement
// ---------------------------------------------------------------------------

#[test]
fn events_outside_the_root_and_rewind_metadata_are_dropped() {
    let fixture = fixture();
    let escape = std::env::temp_dir().join("rewind-outside-probe.txt");
    fs::write(&escape, b"outside").expect("outside file");
    let rewind_meta = fixture
        .ctx
        .workspace_root
        .join(".rewind")
        .join("workspace.json");
    let inside = fixture.ctx.workspace_root.join("foo.txt");
    let script = vec![
        raw(RawKind::Modify, std::slice::from_ref(&escape)),
        raw(RawKind::Modify, std::slice::from_ref(&rewind_meta)),
        raw(RawKind::Modify, std::slice::from_ref(&inside)),
    ];
    let handle = spawn_loop(&fixture.ctx, script, &config(50));
    assert!(wait_until(Duration::from_secs(10), || dirty_paths(
        &fixture.ctx
    ) == ["foo.txt"]));
    request_stop(&fixture.ctx);
    assert_eq!(handle.join().expect("join"), LoopExit::Stopped);

    let index: DirtyIndex =
        serde_json::from_str(&fs::read_to_string(&fixture.ctx.paths.dirty).expect("dirty"))
            .expect("dirty json");
    assert_eq!(index.anomalies, 1, "the escape is dropped and counted");
    let log = fs::read_to_string(&fixture.ctx.paths.events).expect("event log");
    assert!(
        !log.contains(".rewind"),
        "watcher scope equals scanner scope: no .rewind events"
    );
    let _ = fs::remove_file(&escape);
}

// ---------------------------------------------------------------------------
// Crash/restart and offline semantics (contract §10, §14, §15)
// ---------------------------------------------------------------------------

fn planted_state(heartbeat_age_micros: i64, status: StatusKind) -> WatcherState {
    let mut state = WatcherState::new(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), status, 250);
    state.heartbeat_at = now_micros() - heartbeat_age_micros;
    state
}

#[test]
fn a_first_ever_start_claims_no_prior_coverage_and_records_no_gap() {
    let fixture = fixture();
    assert!(!fixture.ctx.paths.state.exists(), "no prior run");
    watch::record_start_gap(&fixture.ctx).expect("gap check");
    assert!(
        !fixture.ctx.paths.degradations.exists(),
        "nothing was claimed observed before, so nothing is broken"
    );
}

#[test]
fn a_crashed_watcher_leaves_an_unobserved_gap_that_gates_the_workspace() {
    let fixture = fixture();
    // 10:00 watcher healthy, 10:17 watcher died (heartbeat is 13 min old).
    let dead = planted_state(13 * 60 * 1_000_000, StatusKind::Running);
    let heartbeat = dead.heartbeat_at;
    rewind::watch::model::write_json_atomic(&fixture.ctx.paths.state, &dead)
        .expect("plant crashed watcher state");

    watch::record_start_gap(&fixture.ctx).expect("gap check");
    let marker = read_degradations(&fixture.ctx.paths.degradations).expect("gap marker");
    assert_eq!(marker.records.len(), 1);
    assert_eq!(marker.records[0].reason, DegradationReason::WatcherGap);
    assert_eq!(marker.records[0].gap_from, Some(heartbeat));
    assert!(marker.records[0].gap_to.unwrap_or(0) >= heartbeat);

    // The next writer opens the interval; reconciliation closes it.
    let workspace = Workspace::open_from_current(&fixture.root).expect("open");
    workspace.enforce_pending_safety_gate().expect("enforce");
    assert_eq!(
        workspace.condition().expect("condition"),
        WorkspaceCondition::ReconciliationRequired
    );
    let reason = workspace
        .storage
        .catalog
        .unknown_intervals(workspace.id)
        .expect("intervals")
        .into_iter()
        .find(|interval| interval.is_open)
        .expect("open interval")
        .reason;
    assert!(
        reason.contains("WATCHER_GAP"),
        "the interval must name the watcher gap: {reason}"
    );
    workspace
        .reconcile_locked("close the crash gap")
        .expect("reconcile");
    assert_eq!(
        workspace.condition().expect("condition"),
        WorkspaceCondition::Healthy
    );
}

#[test]
fn a_graceful_stop_still_leaves_the_interval_after_it_unobserved() {
    let fixture = fixture();
    // The watcher stopped cleanly at T: everything after T was not observed.
    let mut stopped = planted_state(0, StatusKind::Stopped);
    stopped.stopped_at = Some(now_micros() - 5 * 60 * 1_000_000);
    rewind::watch::model::write_json_atomic(&fixture.ctx.paths.state, &stopped)
        .expect("plant stopped state");

    watch::record_start_gap(&fixture.ctx).expect("gap check");
    assert_eq!(
        degradations(&fixture.ctx),
        vec![DegradationReason::WatcherGap],
        "offline changes are never pretended observed (contract §15)"
    );
}

// ---------------------------------------------------------------------------
// Enforcement inside the real writer paths, end to end through the CLI
// ---------------------------------------------------------------------------

#[test]
fn a_pending_marker_gates_the_writer_and_reconcile_clears_it() {
    let fixture = fixture();
    let mut state = planted_state(0, StatusKind::Stopped);
    state.stopped_at = Some(now_micros());
    rewind::watch::model::write_json_atomic(&fixture.ctx.paths.state, &state).expect("state");
    watch::record_start_gap(&fixture.ctx).expect("gap check");

    // `rewind doctor` is a diagnostic enforcement point: it must surface the
    // gate with exit 3 and the reconcile action (house convention).
    let (code, stdout, _stderr) = run_cli(&["doctor"], &fixture.root);
    assert_eq!(code, 3, "doctor output: {stdout}");
    assert!(stdout.contains("RECONCILIATION_REQUIRED"), "{stdout}");

    // `rewind reconcile` consumes the marker and restores HEALTHY.
    let (code, stdout, _stderr) = run_cli(&["reconcile"], &fixture.root);
    assert_eq!(code, 0, "reconcile output: {stdout}");
    assert!(
        !fixture.ctx.paths.degradations.exists(),
        "the marker must not survive a successful reconciliation"
    );
    let (code, stdout, _stderr) = run_cli(&["status"], &fixture.root);
    assert_eq!(code, 0, "status output: {stdout}");
    assert!(stdout.contains("HEALTHY"), "{stdout}");
}

// ---------------------------------------------------------------------------
// Real-adapter lifecycle through the real CLI, including detached start
// ---------------------------------------------------------------------------

#[test]
fn the_real_watcher_lifecycle_observes_changes_and_stops_cleanly() {
    let fixture = fixture();

    // First-ever start: status is STOPPED and no marker exists.
    let (code, stdout, _stderr) = run_cli(&["watch", "status", "--json"], &fixture.root);
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("STOPPED"), "{stdout}");

    // Start detached (the production path: current binary + spawn).
    let (code, stdout, stderr) = run_cli(&["watch", "start", "--batch-ms", "50"], &fixture.root);
    assert_eq!(code, 0, "start: {stdout}{stderr}");

    // The derived lifecycle becomes RUNNING (fresh heartbeat).
    assert!(
        wait_until(Duration::from_secs(15), || {
            let (code, stdout, _) = run_cli(&["watch", "status", "--json"], &fixture.root);
            code == 0 && stdout.contains("\"RUNNING\"")
        }),
        "watcher must reach RUNNING"
    );

    // A real filesystem change lands in the dirty index and the event log.
    fs::write(fixture.root.join("watched.txt"), b"hello").expect("write watched file");
    assert!(
        wait_until(Duration::from_secs(15), || dirty_paths(&fixture.ctx)
            .contains(&"watched.txt".to_owned())),
        "the real adapter must deliver the change into the advisory index"
    );
    let log = fs::read_to_string(&fixture.ctx.paths.events).expect("event log");
    assert!(
        log.contains("watched.txt"),
        "the raw event is recorded: {log}"
    );

    // A concurrent writer works while the watcher runs: no lease conflict,
    // no interference (single-writer rule intact, contract §3 C2).
    let (code, stdout, stderr) = run_cli(&["reconcile"], &fixture.root);
    assert_eq!(code, 0, "reconcile while watching: {stdout}{stderr}");
    assert!(stdout.contains("reconciled"), "{stdout}");

    // Bounded, signal-free stop.
    let (code, stdout, stderr) = run_cli(&["watch", "stop"], &fixture.root);
    assert_eq!(code, 0, "stop: {stdout}{stderr}");
    let (code, stdout, _) = run_cli(&["watch", "status", "--json"], &fixture.root);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"STOPPED\""), "{stdout}");
}

#[test]
fn a_second_start_is_refused_while_a_watcher_is_running() {
    let fixture = fixture();
    let (code, _stdout, _stderr) = run_cli(&["watch", "start", "--batch-ms", "50"], &fixture.root);
    assert_eq!(code, 0, "the first start must succeed");
    let (code, _stdout, stderr) = run_cli(&["watch", "start"], &fixture.root);
    assert_eq!(code, 3, "a second watcher must be refused");
    assert!(stderr.contains("already running"), "{stderr}");
    let (code, _stdout, _stderr) = run_cli(&["watch", "stop"], &fixture.root);
    assert_eq!(code, 0, "cleanup stop");
}

#[test]
fn restarting_a_stopped_watcher_records_the_gap_through_the_cli() {
    let fixture = fixture();
    // Run once, stop cleanly (this is the "watcher was running, then stopped"
    // half of the crash/restart model).
    let (code, stdout, stderr) = run_cli(&["watch", "start", "--batch-ms", "50"], &fixture.root);
    assert_eq!(code, 0, "{stdout}{stderr}");
    let (code, _stdout, _stderr) = run_cli(&["watch", "stop"], &fixture.root);
    assert_eq!(code, 0);

    // Files changed while the watcher was off.
    fs::write(fixture.root.join("offline.txt"), b"changed offline").expect("write");

    // The restart must not pretend it observed them: the gap record exists
    // before any new coverage is claimed.
    let (code, _stdout, stderr) = run_cli(&["watch", "start", "--batch-ms", "50"], &fixture.root);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        degradations(&fixture.ctx),
        vec![DegradationReason::WatcherGap],
        "the unobserved interval is durable immediately"
    );
    // The workspace is gated until reconciliation.
    let (code, stdout, _) = run_cli(&["doctor"], &fixture.root);
    assert_eq!(code, 3, "{stdout}");
    assert!(stdout.contains("RECONCILIATION_REQUIRED"), "{stdout}");
    let (code, stdout, _) = run_cli(&["reconcile"], &fixture.root);
    assert_eq!(code, 0, "{stdout}");
    assert!(!fixture.ctx.paths.degradations.exists());

    // Cleanup: the restarted watcher is still running.
    let (code, _stdout, _stderr) = run_cli(&["watch", "stop"], &fixture.root);
    assert_eq!(code, 0);
}

#[test]
fn watch_status_reports_a_dead_watcher_as_failed_not_running() {
    let fixture = fixture();
    let dead = planted_state(13 * 60 * 1_000_000, StatusKind::Running);
    rewind::watch::model::write_json_atomic(&fixture.ctx.paths.state, &dead)
        .expect("plant dead watcher state");
    let (code, stdout, _) = run_cli(&["watch", "status", "--json"], &fixture.root);
    assert_eq!(code, 3, "a dead watcher needs operator action");
    assert!(stdout.contains("\"FAILED\""), "{stdout}");
    assert!(stdout.contains("restart"), "{stdout}");
}
