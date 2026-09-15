//! Phase 1.4 passive-boundary identity and asynchronous post-hook
//! concurrency suite.
//!
//! Phase 1.3 made post-hook bookkeeping asynchronous (`nohup ... &`). The
//! retired implementation then correlated a background post-hook with its
//! boundary by asking the catalog for the *newest unconsumed boundary of the
//! session* — which is ambiguous as soon as two background hooks are in
//! flight, and could associate one command's exit code, cwd, effects, and
//! state transition with another command.
//!
//! Phase 1.4 removed that lookup. A post-hook receives the immutable
//! boundary id that its own pre-hook minted, and claims exactly that
//! boundary exactly once. These tests prove the identity property directly:
//!
//! - the post for the *older* boundary may never consume a newer one (the
//!   precise ordering the retired lookup got wrong);
//! - three background hooks may genuinely overlap and still keep identity;
//! - unknown, already-accounted-for, and cross-workspace ids fail open with
//!   no side effects;
//! - a boundary is accounted for exactly once, database-enforced;
//! - observations keep command/exit-code/cwd/effect provenance, the trusted
//!   checkpoint only ever advances to a *later* scan, and reversed
//!   completion order never regresses it.
//!
//! Every identity assertion here is deterministic: it never depends on scan
//! timing, wall-clock delays, or scheduling order. The gated-workspace
//! scenarios take the hook's scan-free `BoundaryOnly` path, so their
//! per-boundary metadata is a direct read of the claimed boundary; the
//! overlap scenario is synchronized with the writer lease rather than with
//! a sleep-based race window.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rewind::db::BoundaryRow;
use rewind::model::{OperationKind, OperationRecord, WorkspaceCondition};
use rewind::workspace::{Workspace, WorkspaceLease};
use tempfile::TempDir;

const COMMAND_A: &str = "COMMAND_A";
const COMMAND_B: &str = "COMMAND_B";
const COMMAND_C: &str = "COMMAND_C";
const EXIT_A: i32 = 17;
const EXIT_B: i32 = 23;
const EXIT_C: i32 = 41;

fn rewind_bin() -> &'static str {
    env!("CARGO_BIN_EXE_rewind")
}

fn fixture() -> (TempDir, TempDir, Workspace) {
    let root = tempfile::tempdir().expect("workspace root");
    fs::write(root.path().join("seed.txt"), b"seed").expect("fixture file");
    let store = tempfile::tempdir().expect("store");
    let workspace = Workspace::init(root.path(), Some(store.path())).expect("initialize");
    (root, store, workspace)
}

fn cli(store: &Path, root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(rewind_bin())
        .args(args)
        .current_dir(root)
        .env("REWIND_HOME", store.to_string_lossy().into_owned())
        .output()
        .expect("run rewind cli")
}

/// Runs the pre-hook and returns the immutable boundary id it prints as the
/// single line on stdout. The shape of that output is asserted here so no
/// test can silently proceed without a real id.
fn pre_hook(store: &Path, root: &Path, session: &str, command: &str) -> String {
    let output = cli(
        store,
        root,
        &["hook", "pre", "--command", command, "--session", session],
    );
    assert!(
        output.status.success(),
        "pre-hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8 pre-hook stdout");
    let id = stdout.trim_end_matches(['\n', '\r']);
    assert!(
        id.lines().count() == 1,
        "the pre-hook must print exactly one line (got {id:?})"
    );
    assert!(
        id.len() >= 32,
        "the pre-hook must print the boundary id on stdout (got {id:?})"
    );
    id.to_owned()
}

fn post_hook(store: &Path, root: &Path, boundary: &str, exit_code: i32) -> std::process::Output {
    cli(
        store,
        root,
        &[
            "hook",
            "post",
            "--boundary",
            boundary,
            "--exit-code",
            &exit_code.to_string(),
        ],
    )
}

/// Starts a real background post-hook process (the shape the shell
/// integration uses) so that several hooks can be in flight at once.
fn spawn_post(store: &Path, root: &Path, boundary: &str, exit_code: i32) -> Child {
    Command::new(rewind_bin())
        .args([
            "hook",
            "post",
            "--boundary",
            boundary,
            "--exit-code",
            &exit_code.to_string(),
        ])
        .current_dir(root)
        .env("REWIND_HOME", store.to_string_lossy().into_owned())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn post hook")
}
fn wait_for_child(child: &mut Child, limit: Duration) -> i32 {
    let started = Instant::now();
    loop {
        match child.try_wait().expect("poll post hook") {
            Some(status) => return status.code().unwrap_or(-1),
            None if started.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("post-hook did not finish within {limit:?}");
            }
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    }
}

/// Gating the workspace makes the post-hook's representation deterministic
/// and scan-free: a gated workspace records a `BoundaryOnly` operation that
/// carries the claimed boundary's own command, cwd, and exit code. Identity
/// can therefore be asserted without any dependence on scan timing.
fn gate(workspace: &Workspace) {
    let baseline = workspace.row().expect("row").baseline_state;
    workspace
        .storage
        .catalog
        .open_unknown(workspace.id, baseline.as_deref(), "correlation test gate")
        .expect("open unknown interval");
    workspace
        .storage
        .catalog
        .set_workspace(
            workspace.id,
            &WorkspaceCondition::ReconciliationRequired,
            baseline.as_deref(),
        )
        .expect("gate workspace");
}

fn boundaries(workspace: &Workspace) -> Vec<BoundaryRow> {
    workspace
        .storage
        .catalog
        .boundaries(workspace.id)
        .expect("boundaries")
}

fn operations(workspace: &Workspace) -> Vec<OperationRecord> {
    workspace
        .storage
        .catalog
        .list_operations(workspace.id)
        .expect("operations")
}

fn condition(workspace: &Workspace) -> WorkspaceCondition {
    workspace.condition().expect("workspace condition")
}

fn observation_count(workspace: &Workspace) -> usize {
    operations(workspace)
        .iter()
        .filter(|record| record.kind == OperationKind::PassiveObservation)
        .count()
}

fn db_path(workspace: &Workspace) -> PathBuf {
    workspace.storage.project_root.join("metadata.sqlite")
}

fn boundary_for<'a>(boundaries: &'a [BoundaryRow], token: &str) -> &'a BoundaryRow {
    let mut matches = boundaries.iter().filter(|row| row.command.contains(token));
    let found = matches
        .next()
        .unwrap_or_else(|| panic!("no boundary whose command contains {token}"));
    assert!(
        matches.next().is_none(),
        "more than one boundary carries {token}"
    );
    found
}

fn operation_for<'a>(operations: &'a [OperationRecord], command: &str) -> &'a OperationRecord {
    let mut matches = operations
        .iter()
        .filter(|record| record.command.as_deref() == Some(command));
    let found = matches
        .next()
        .unwrap_or_else(|| panic!("no operation for {command}"));
    assert!(
        matches.next().is_none(),
        "more than one operation carries {command}"
    );
    found
}

/// The shared invariant set for a workspace whose boundaries were all
/// claimed by exactly one post-hook each: no fabrication and no operation
/// whose command/exit-code pair could belong to another boundary.
fn assert_accounted_once(workspace: &Workspace, expected: &[(&str, i32)]) {
    let rows = boundaries(workspace);
    assert_eq!(
        rows.len(),
        expected.len(),
        "no boundary may be fabricated: {:?}",
        rows.iter()
            .map(|row| (&row.command, row.consumed))
            .collect::<Vec<_>>()
    );
    for (token, exit_code) in expected {
        let row = boundary_for(&rows, token);
        assert!(row.consumed, "{token} must be accounted for exactly once");
        assert_eq!(
            row.exit_code,
            Some(*exit_code),
            "{token} must keep its own exit status"
        );
        assert!(row.ended_at.is_some(), "{token} must record its end time");
    }
    let records = operations(workspace);
    assert!(
        records
            .iter()
            .all(|record| record.kind != OperationKind::Strong),
        "passive bookkeeping must never fabricate a strong operation"
    );
    for record in &records {
        let command = record.command.as_deref().unwrap_or_default();
        if let Some((_, expected_exit)) =
            expected.iter().find(|(token, _)| command.contains(*token))
        {
            assert_eq!(
                record.exit_code,
                Some(*expected_exit),
                "operation {command:?} must carry its own boundary's exit status"
            );
        }
    }
}

/// THE regression test for the retired defect. The post-hook for the OLDER
/// boundary runs first while a newer boundary is still pending — exactly the
/// ordering in which the retired "newest unconsumed boundary of the session"
/// lookup consumed the wrong boundary (post(A) took B, then post(B) took A,
/// swapping command, exit code, effects, and state transition).
#[test]
fn older_boundary_post_never_consumes_a_newer_boundary() {
    let (root, store, workspace) = fixture();
    gate(&workspace);
    let session = format!("corr-{}", std::process::id());
    let boundary_a = pre_hook(store.path(), root.path(), &session, COMMAND_A);
    let boundary_b = pre_hook(store.path(), root.path(), &session, COMMAND_B);
    assert_ne!(
        boundary_a, boundary_b,
        "each pre-hook must mint a distinct boundary id"
    );

    let first = post_hook(store.path(), root.path(), &boundary_a, EXIT_A);
    assert_eq!(
        first.status.code(),
        Some(0),
        "the hook must fail open: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = post_hook(store.path(), root.path(), &boundary_b, EXIT_B);
    assert_eq!(
        second.status.code(),
        Some(0),
        "the hook must fail open: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    assert_accounted_once(&workspace, &[(COMMAND_A, EXIT_A), (COMMAND_B, EXIT_B)]);

    let rows = boundaries(&workspace);
    assert_eq!(boundary_for(&rows, COMMAND_A).id, boundary_a);
    assert_eq!(boundary_for(&rows, COMMAND_B).id, boundary_b);

    // The gated path records one BoundaryOnly operation per claimed
    // boundary; each carries the metadata of the boundary it claimed.
    let records = operations(&workspace);
    let for_a = operation_for(&records, COMMAND_A);
    let for_b = operation_for(&records, COMMAND_B);
    assert_eq!(for_a.kind, OperationKind::BoundaryOnly);
    assert_eq!(for_b.kind, OperationKind::BoundaryOnly);
    assert_eq!(for_a.exit_code, Some(EXIT_A));
    assert_eq!(for_b.exit_code, Some(EXIT_B));
    assert_eq!(
        for_a.cwd.as_deref(),
        Some(boundary_for(&rows, COMMAND_A).cwd.as_str())
    );
    assert_eq!(
        for_b.cwd.as_deref(),
        Some(boundary_for(&rows, COMMAND_B).cwd.as_str())
    );
    assert_eq!(
        observation_count(&workspace),
        0,
        "a gated workspace yields boundary-only records, never observations"
    );
    assert_eq!(
        condition(&workspace),
        WorkspaceCondition::ReconciliationRequired,
        "the test gate must survive the hooks"
    );
}

/// Three post-hooks for three boundaries run as genuine concurrent
/// processes. The writer lease is used as a *barrier*: it is held by the
/// test while all three hooks start, so none of them can reach its
/// bookkeeping until every hook is already running (real overlap), and the
/// assertions are order-independent, so the outcome is deterministic
/// regardless of which hook wins the lease afterwards.
#[test]
fn overlapping_background_posts_keep_identity() {
    let (root, store, workspace) = fixture();
    gate(&workspace);
    let session = format!("overlap-{}", std::process::id());
    let plan = [
        (COMMAND_A, EXIT_A),
        (COMMAND_B, EXIT_B),
        (COMMAND_C, EXIT_C),
    ];
    let ids: Vec<String> = plan
        .iter()
        .map(|(command, _)| pre_hook(store.path(), root.path(), &session, command))
        .collect();
    assert_eq!(ids.len(), 3);

    let lease = WorkspaceLease::acquire(&workspace, true).expect("hold writer lease as a barrier");
    let mut children: Vec<Child> = ids
        .iter()
        .zip(plan.iter())
        .map(|(id, (_, exit_code))| spawn_post(store.path(), root.path(), id, *exit_code))
        .collect();
    // Every hook is now running and blocked on the lease: overlap is real,
    // not a race window.
    std::thread::sleep(Duration::from_millis(500));
    drop(lease);

    for child in &mut children {
        let code = wait_for_child(child, Duration::from_secs(60));
        assert_eq!(code, 0, "the hook must fail open under overlap");
    }

    assert_accounted_once(&workspace, &plan[..]);
    let rows = boundaries(&workspace);
    for (id, (command, exit_code)) in ids.iter().zip(plan.iter()) {
        let row = boundary_for(&rows, command);
        assert_eq!(&row.id, id, "{command} must be its own boundary");
        assert_eq!(row.exit_code, Some(*exit_code));
    }
    let records = operations(&workspace);
    for (command, exit_code) in plan.iter() {
        let record = operation_for(&records, command);
        assert_eq!(record.kind, OperationKind::BoundaryOnly);
        assert_eq!(record.exit_code, Some(*exit_code));
    }
    assert_eq!(observation_count(&workspace), 0);
    assert!(
        !workspace
            .storage
            .catalog
            .has_pending_bypass(workspace.id)
            .expect("bypass"),
        "serialized hooks must not need the conservative bypass fallback"
    );
}

/// Case 1: a post-hook referencing a boundary that does not exist must fail
/// open with no side effects at all — no fabricated observation, no durable
/// gate for an interval that never existed, and above all no unrelated
/// boundary consumed.
#[test]
fn nonexistent_boundary_post_fails_open_without_side_effects() {
    let (root, store, workspace) = fixture();
    let session = format!("unknown-{}", std::process::id());
    let _a = pre_hook(store.path(), root.path(), &session, COMMAND_A);
    let _b = pre_hook(store.path(), root.path(), &session, COMMAND_B);

    let output = post_hook(
        store.path(),
        root.path(),
        "5b1f5f9e-0000-4000-8000-000000000000",
        EXIT_A,
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "an unknown boundary id must fail open"
    );
    assert!(
        !output.stderr.is_empty(),
        "an unknown boundary id must leave a diagnostic"
    );

    let rows = boundaries(&workspace);
    assert_eq!(rows.len(), 2, "no boundary may be fabricated");
    assert!(
        rows.iter().all(|row| !row.consumed),
        "no unrelated boundary may be consumed"
    );
    assert!(
        operations(&workspace).is_empty(),
        "an unknown boundary id must not fabricate an operation"
    );
    assert_eq!(
        condition(&workspace),
        WorkspaceCondition::Healthy,
        "an unknown boundary id must not gate the workspace"
    );
    assert!(
        !workspace
            .storage
            .catalog
            .has_pending_bypass(workspace.id)
            .expect("bypass"),
        "an unknown boundary id must not leave a bypass marker"
    );
    assert!(
        !workspace
            .storage
            .catalog
            .has_open_unknown(workspace.id)
            .expect("gap"),
        "an unknown boundary id must not open an unknown interval"
    );
}

/// Case 2: a duplicate post for an already-accounted-for boundary changes
/// nothing — no second observation, no overwritten exit status, no durable
/// gate, and no consumption of another pending boundary.
#[test]
fn duplicate_post_cannot_account_for_a_boundary_twice() {
    let (root, store, workspace) = fixture();
    let session = format!("dup-{}", std::process::id());
    let boundary_a = pre_hook(store.path(), root.path(), &session, COMMAND_A);

    let first = post_hook(store.path(), root.path(), &boundary_a, EXIT_A);
    assert_eq!(first.status.code(), Some(0));
    assert_eq!(observation_count(&workspace), 1, "the first post observes");

    // A second, still pending boundary that must not be touched by the
    // duplicate post.
    let boundary_b = pre_hook(store.path(), root.path(), &session, COMMAND_B);

    let duplicate = post_hook(store.path(), root.path(), &boundary_a, 99);
    assert_eq!(
        duplicate.status.code(),
        Some(0),
        "a duplicate post must fail open"
    );
    assert!(
        !duplicate.stderr.is_empty(),
        "a duplicate post must leave a diagnostic"
    );

    let rows = boundaries(&workspace);
    assert_eq!(rows.len(), 2, "no boundary may be fabricated");
    let row_a = boundary_for(&rows, COMMAND_A);
    let row_b = boundary_for(&rows, COMMAND_B);
    assert!(row_a.consumed);
    assert_eq!(
        row_a.exit_code,
        Some(EXIT_A),
        "a duplicate post must not overwrite the recorded exit status"
    );
    assert!(!row_b.consumed, "a duplicate post must not consume B");
    assert_eq!(
        observation_count(&workspace),
        1,
        "a duplicate post must not create a second observation"
    );
    assert_eq!(
        operation_for(&operations(&workspace), COMMAND_A).exit_code,
        Some(EXIT_A),
        "the recorded operation must still carry the original exit status"
    );
    assert!(!workspace
        .storage
        .catalog
        .has_pending_bypass(workspace.id)
        .expect("bypass"));
    assert_eq!(condition(&workspace), WorkspaceCondition::Healthy);
    let _ = boundary_b;
}

/// A boundary id minted by one workspace can never be consumed in another,
/// and the workspace that owns it can still complete it normally.
#[test]
fn cross_workspace_boundary_id_is_refused() {
    let (root_one, store_one, workspace_one) = fixture();
    let (root_two, store_two, workspace_two) = fixture();
    let foreign = pre_hook(store_one.path(), root_one.path(), "session-one", COMMAND_A);
    let _own = pre_hook(store_two.path(), root_two.path(), "session-two", COMMAND_B);

    let refused = post_hook(store_two.path(), root_two.path(), &foreign, EXIT_A);
    assert_eq!(
        refused.status.code(),
        Some(0),
        "a foreign id must fail open"
    );
    assert!(!refused.stderr.is_empty(), "a foreign id must be diagnosed");

    assert!(
        boundaries(&workspace_two).iter().all(|row| !row.consumed),
        "a foreign boundary id must not consume this workspace's boundary"
    );
    assert!(
        operations(&workspace_two).is_empty(),
        "a foreign boundary id must not fabricate an operation here"
    );
    assert!(
        boundaries(&workspace_one).iter().all(|row| !row.consumed),
        "the owning workspace's boundary must still be pending"
    );

    let accepted = post_hook(store_one.path(), root_one.path(), &foreign, EXIT_A);
    assert_eq!(accepted.status.code(), Some(0));
    assert_accounted_once(&workspace_one, &[(COMMAND_A, EXIT_A)]);
}

/// Two sessions in the same workspace keep their own observation provenance.
/// The session id is recorded for diagnostics only; identity comes from the
/// boundary id, so boundaries of different sessions can never be swapped and
/// an observation can never attribute a later command's effect to an earlier
/// command.
#[test]
fn multiple_sessions_keep_their_own_observation_provenance() {
    let (root, store, workspace) = fixture();
    let boundary_a = pre_hook(store.path(), root.path(), "session-one", COMMAND_A);
    let boundary_b = pre_hook(store.path(), root.path(), "session-two", COMMAND_B);

    fs::write(root.path().join("a-change.txt"), b"a").expect("a change");
    let first = post_hook(store.path(), root.path(), &boundary_a, EXIT_A);
    assert_eq!(first.status.code(), Some(0));

    fs::write(root.path().join("b-change.txt"), b"b").expect("b change");
    let second = post_hook(store.path(), root.path(), &boundary_b, EXIT_B);
    assert_eq!(second.status.code(), Some(0));

    let rows = boundaries(&workspace);
    let row_a = boundary_for(&rows, COMMAND_A);
    let row_b = boundary_for(&rows, COMMAND_B);
    assert_eq!(row_a.session_id, "session-one");
    assert_eq!(row_b.session_id, "session-two");
    assert_eq!(row_a.id, boundary_a);
    assert_eq!(row_b.id, boundary_b);

    let records = operations(&workspace);
    let for_a = operation_for(&records, COMMAND_A);
    let for_b = operation_for(&records, COMMAND_B);
    assert_eq!(for_a.kind, OperationKind::PassiveObservation);
    assert_eq!(for_b.kind, OperationKind::PassiveObservation);
    assert_eq!(for_a.exit_code, Some(EXIT_A));
    assert_eq!(for_b.exit_code, Some(EXIT_B));
    assert_eq!(for_a.cwd.as_deref(), Some(row_a.cwd.as_str()));
    assert_eq!(for_b.cwd.as_deref(), Some(row_b.cwd.as_str()));
    assert!(
        for_a
            .effects
            .iter()
            .any(|effect| effect.path == "a-change.txt"),
        "COMMAND_A's observation must contain its own effect: {:?}",
        for_a
            .effects
            .iter()
            .map(|effect| &effect.path)
            .collect::<Vec<_>>()
    );
    assert!(
        for_b
            .effects
            .iter()
            .any(|effect| effect.path == "b-change.txt"),
        "COMMAND_B's observation must contain its own effect"
    );
    assert!(
        !for_a
            .effects
            .iter()
            .any(|effect| effect.path == "b-change.txt"),
        "an earlier command must not be credited with a later command's effect"
    );
    assert_eq!(condition(&workspace), WorkspaceCondition::Healthy);
}

/// The catalog API itself enforces single accounting, workspace scoping, and
/// immutability of an accounted-for boundary — including at the SQL level.
#[test]
fn boundary_api_accounts_for_exactly_one_boundary_once() {
    let (_root, _store, workspace) = fixture();
    let catalog = &workspace.storage.catalog;
    let first = catalog
        .add_boundary(workspace.id, "api-session", COMMAND_A, "/api/cwd-a")
        .expect("add boundary");
    let second = catalog
        .add_boundary(workspace.id, "api-session", COMMAND_B, "/api/cwd-b")
        .expect("add boundary");
    assert_ne!(first, second, "boundary ids must be unique");

    let row = catalog
        .boundary(workspace.id, &first)
        .expect("fetch boundary")
        .expect("boundary exists");
    assert_eq!(row.command, COMMAND_A);
    assert_eq!(row.cwd, "/api/cwd-a");
    assert!(!row.consumed);
    assert_eq!(row.exit_code, None);
    assert!(catalog
        .boundary(workspace.id, "not-a-boundary-id")
        .expect("fetch missing boundary")
        .is_none());
    assert!(
        catalog
            .boundary(uuid::Uuid::new_v4(), &first)
            .expect("fetch from foreign workspace")
            .is_none(),
        "a boundary is not visible from another workspace"
    );

    assert!(
        catalog
            .finish_boundary(workspace.id, &first, EXIT_A)
            .expect("first accounting"),
        "the first accounting must succeed"
    );
    assert!(
        !catalog
            .finish_boundary(workspace.id, &first, EXIT_B)
            .expect("second accounting"),
        "a boundary cannot be accounted for twice"
    );
    assert!(
        !catalog
            .finish_boundary(workspace.id, "not-a-boundary-id", 0)
            .expect("accounting an unknown id"),
        "an unknown id cannot be accounted for"
    );
    assert!(
        !catalog
            .finish_boundary(uuid::Uuid::new_v4(), &second, EXIT_B)
            .expect("accounting from another workspace"),
        "another workspace cannot account for this boundary"
    );

    let row = catalog
        .boundary(workspace.id, &first)
        .expect("refetch boundary")
        .expect("boundary exists");
    assert!(row.consumed);
    assert_eq!(row.exit_code, Some(EXIT_A));
    let untouched = catalog
        .boundary(workspace.id, &second)
        .expect("refetch second boundary")
        .expect("second boundary exists");
    assert!(!untouched.consumed);
    assert_eq!(untouched.exit_code, None);

    // The database refuses to rewrite or resurrect an accounted-for boundary
    // even for a caller that bypasses the typed API.
    let raw = rusqlite::Connection::open(db_path(&workspace)).expect("open catalog directly");
    assert!(
        raw.execute(
            "UPDATE passive_boundaries SET consumed = 0 WHERE id = ?1",
            rusqlite::params![first]
        )
        .is_err(),
        "the catalog must refuse to resurrect an accounted-for boundary"
    );
    assert!(
        raw.execute(
            "UPDATE passive_boundaries SET exit_code = 99 WHERE id = ?1",
            rusqlite::params![first]
        )
        .is_err(),
        "the catalog must refuse to rewrite an accounted-for boundary"
    );
}

/// A platform-native writer command for `rewind run` (same shape the other
/// suites use).
fn writer_args() -> Vec<&'static str> {
    if cfg!(windows) {
        vec!["run", "--", "cmd", "/C", "echo n> next.txt"]
    } else {
        vec!["run", "--", "sh", "-c", "echo n > next.txt"]
    }
}

/// Case 4 / "post during writer activity": a writer holds the lease for
/// longer than the hook's retry window, so the post-hook accounts for its own
/// boundary and then falls back to the durable bypass marker. A stays
/// conservatively represented (gate, then reconciliation), B is untouched,
/// and the later writer path stays correct.
#[test]
fn post_during_writer_activity_gates_durably_and_leaves_others_pending() {
    let (root, store, workspace) = fixture();
    let session = format!("writer-{}", std::process::id());
    let boundary_a = pre_hook(store.path(), root.path(), &session, COMMAND_A);
    let boundary_b = pre_hook(store.path(), root.path(), &session, COMMAND_B);

    let lease = WorkspaceLease::acquire(&workspace, true).expect("hold writer lease");
    let post = post_hook(store.path(), root.path(), &boundary_a, EXIT_A);
    assert_eq!(
        post.status.code(),
        Some(0),
        "the hook must fail open while a writer holds the lease: {}",
        String::from_utf8_lossy(&post.stderr)
    );
    drop(lease);

    let rows = boundaries(&workspace);
    assert_eq!(rows.len(), 2, "no boundary may be fabricated");
    let row_a = boundary_for(&rows, COMMAND_A);
    let row_b = boundary_for(&rows, COMMAND_B);
    assert!(
        row_a.consumed,
        "the hook accounts for its own boundary before waiting for the lease"
    );
    assert_eq!(row_a.exit_code, Some(EXIT_A));
    assert!(
        !row_b.consumed,
        "the unrelated boundary stays pending for its own post-hook"
    );
    assert_eq!(
        observation_count(&workspace),
        0,
        "no observation may be fabricated across an active writer"
    );
    assert!(
        workspace
            .storage
            .catalog
            .has_pending_bypass(workspace.id)
            .expect("bypass"),
        "the deferred post must leave a durable bypass marker"
    );

    let run = cli(store.path(), root.path(), &writer_args());
    assert!(
        run.status.success(),
        "the writer must gate, reconcile, and capture: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        condition(&workspace),
        WorkspaceCondition::Healthy,
        "reconciliation must restore HEALTHY"
    );
    assert!(
        operations(&workspace)
            .iter()
            .any(|record| record.kind == OperationKind::Strong),
        "the writer must capture a strong operation"
    );
    assert_eq!(
        observation_count(&workspace),
        0,
        "the deferred interval must never become an observation"
    );

    // The pending boundary is still completable, and completing it cannot
    // disturb the interval that was already accounted for.
    let completed = post_hook(store.path(), root.path(), &boundary_b, EXIT_B);
    assert_eq!(completed.status.code(), Some(0));
    let rows = boundaries(&workspace);
    assert!(boundary_for(&rows, COMMAND_B).consumed);
    assert_eq!(boundary_for(&rows, COMMAND_B).exit_code, Some(EXIT_B));
    assert_eq!(
        boundary_for(&rows, COMMAND_A).exit_code,
        Some(EXIT_A),
        "B's post-hook must not touch A's record"
    );
}

/// §13 in-order proof: the trusted checkpoint chain is the completion order,
/// each observation is measured from the checkpoint in force when its scan
/// ran, and a following writer finds the checkpoint fresh (its anchor is the
/// newest observation, so no drift reconciliation is needed).
#[test]
fn observation_chain_advances_the_checkpoint_in_completion_order() {
    let (root, store, workspace) = fixture();
    let session = format!("chain-{}", std::process::id());
    let start = workspace.baseline_id().expect("baseline");

    let boundary_a = pre_hook(store.path(), root.path(), &session, COMMAND_A);
    fs::write(root.path().join("first.txt"), b"one").expect("first change");
    let first = post_hook(store.path(), root.path(), &boundary_a, EXIT_A);
    assert_eq!(first.status.code(), Some(0));
    let after_a = workspace.baseline_id().expect("baseline after A");
    assert_ne!(
        after_a, start,
        "a complete observation advances the trusted checkpoint"
    );

    let boundary_b = pre_hook(store.path(), root.path(), &session, COMMAND_B);
    fs::write(root.path().join("second.txt"), b"two").expect("second change");
    let second = post_hook(store.path(), root.path(), &boundary_b, EXIT_B);
    assert_eq!(second.status.code(), Some(0));
    let after_b = workspace.baseline_id().expect("baseline after B");
    assert_ne!(after_b, after_a);

    let records = operations(&workspace);
    let for_a = operation_for(&records, COMMAND_A);
    let for_b = operation_for(&records, COMMAND_B);
    assert_eq!(for_a.pre_state_id.as_deref(), Some(start.as_str()));
    assert_eq!(for_a.post_state_id.as_deref(), Some(after_a.as_str()));
    assert_eq!(for_b.pre_state_id.as_deref(), Some(after_a.as_str()));
    assert_eq!(for_b.post_state_id.as_deref(), Some(after_b.as_str()));
    assert_eq!(condition(&workspace), WorkspaceCondition::Healthy);

    let run = cli(store.path(), root.path(), &writer_args());
    assert!(
        run.status.success(),
        "the writer must run after two observations: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let records = operations(&workspace);
    let strong = records
        .iter()
        .find(|record| record.kind == OperationKind::Strong)
        .expect("strong operation");
    assert_eq!(
        strong.pre_state_id.as_deref(),
        Some(after_b.as_str()),
        "the writer must anchor on the newest observation, not an older one"
    );
}

/// §13 out-of-order proof: B's background post-hook completes before A's.
/// The checkpoint chain follows completion order, each observation is
/// measured from the checkpoint in force when its own scan ran, and the
/// trusted state can never be rewound to an older observation: a
/// later-finishing hook scans later under the exclusive lease.
#[test]
fn reversed_completion_never_regresses_the_trusted_baseline() {
    let (root, store, workspace) = fixture();
    let session = format!("reversed-{}", std::process::id());
    let start = workspace.baseline_id().expect("baseline");

    let boundary_b = pre_hook(store.path(), root.path(), &session, COMMAND_B);
    let boundary_a = pre_hook(store.path(), root.path(), &session, COMMAND_A);

    fs::write(root.path().join("first.txt"), b"one").expect("first change");
    let completed_b = post_hook(store.path(), root.path(), &boundary_b, EXIT_B);
    assert_eq!(completed_b.status.code(), Some(0));
    let after_b = workspace.baseline_id().expect("baseline after B");

    fs::write(root.path().join("second.txt"), b"two").expect("second change");
    let completed_a = post_hook(store.path(), root.path(), &boundary_a, EXIT_A);
    assert_eq!(completed_a.status.code(), Some(0));
    let after_a = workspace.baseline_id().expect("baseline after A");

    assert_ne!(
        after_a, after_b,
        "the later completion must advance the checkpoint"
    );
    let records = operations(&workspace);
    let for_b = operation_for(&records, COMMAND_B);
    let for_a = operation_for(&records, COMMAND_A);
    assert_eq!(for_b.exit_code, Some(EXIT_B));
    assert_eq!(for_a.exit_code, Some(EXIT_A));
    assert_eq!(for_b.pre_state_id.as_deref(), Some(start.as_str()));
    assert_eq!(for_b.post_state_id.as_deref(), Some(after_b.as_str()));
    assert_eq!(for_a.pre_state_id.as_deref(), Some(after_b.as_str()));
    assert_eq!(for_a.post_state_id.as_deref(), Some(after_a.as_str()));

    assert_eq!(workspace.baseline_id().expect("baseline"), after_a);
    let manifest = workspace.state_manifest(&after_a).expect("manifest");
    assert!(
        manifest.entries.contains_key("first.txt"),
        "the trusted checkpoint must keep every observed change"
    );
    assert!(manifest.entries.contains_key("second.txt"));
    assert_eq!(condition(&workspace), WorkspaceCondition::Healthy);
    assert!(!workspace
        .storage
        .catalog
        .has_open_unknown(workspace.id)
        .expect("gap"));
    assert!(!workspace
        .storage
        .catalog
        .has_pending_bypass(workspace.id)
        .expect("bypass"));
}
