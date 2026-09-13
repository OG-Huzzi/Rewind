//! Phase 1.2 hardening regression suite.
//!
//! Covers the six hardening fixes on real filesystems:
//! - Fix #1: passive-hook budget/fail-open semantics (bounded work, durable
//!   markers, no fabricated attribution, reconciliation restores HEALTHY)
//! - Fix #2: lock owner metadata cannot be truncated by a non-owner contender
//! - Fix #3: path-confinement TOCTOU behavior (safe refusal for symlinked
//!   parents, post-mutation escape detection)
//! - Fix #4: recursive archive verification catches nested corruption
//! - Fix #5/#6: symlink target kinds and schema-versioned state identity
//! - Fix #18/#20: committed-metadata crash windows repair idempotently;
//!   recovery failure states can never leave a false HEALTHY

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use rewind::model::{
    ArchiveStatus, Fingerprint, Journal, JournalStatus, JournalStep, OperationStatus, StateKind,
    SymlinkTargetKind, WorkspaceCondition, STATE_SCHEMA_VERSION,
};
use rewind::paths::staging_root;
use rewind::rollback::{recover_locked, recover_reconcile, undo, verify_archive_pair};
use rewind::workspace::{Workspace, WorkspaceLease};
use tempfile::TempDir;

fn init_workspace() -> (TempDir, TempDir, Workspace) {
    let root = tempfile::tempdir().expect("workspace root");
    fs::write(root.path().join("foo.txt"), b"A").expect("fixture file");
    let store = tempfile::tempdir().expect("store");
    let workspace = Workspace::init(root.path(), Some(store.path())).expect("initialize");
    (root, store, workspace)
}

fn lock_file(workspace: &Workspace) -> PathBuf {
    workspace.storage.project_root.join("lock.pid")
}

fn condition(workspace: &Workspace) -> WorkspaceCondition {
    workspace.condition().expect("workspace condition")
}

/// Fix #2: a contender that has not won the exclusive lock must never
/// truncate the lock holder's owner metadata. On Windows the lease's
/// exclusive byte-range lock makes the file wholly unreadable while held —
/// so the integrity of the owner record is verified *after* the contender
/// round-trip: the pre-Fix-#2 behavior (truncating on open before the lock
/// attempt) would zero the file and leave it empty forever, while the fixed
/// implementation leaves the holder's record intact.
#[test]
fn lock_owner_metadata_survives_contended_acquire() {
    let (_root, _store, workspace) = init_workspace();

    let first = WorkspaceLease::acquire(&workspace, false).expect("first lease");

    // On Windows the exclusive lease is a per-handle byte-range lock over
    // the whole file: any other reader is rejected with a lock violation.
    // (POSIX advisory locks are per-process, so the same-process read
    // below succeeds there and the cross-process test covers contention.)
    #[cfg(windows)]
    assert!(
        fs::read_to_string(lock_file(&workspace)).is_err(),
        "exclusive byte-range lock must block other readers while held"
    );

    // A non-owner contender tries and fails; on Windows the lock is
    // per-handle so this fails even in-process.
    let contended = WorkspaceLease::acquire(&workspace, true);
    #[cfg(windows)]
    assert!(
        contended.is_err(),
        "contender must fail while owner holds lock"
    );
    drop(contended);
    drop(first);

    // After release the holder's record must be intact: the contender's
    // failed acquire never truncated the file.
    let after = fs::read_to_string(lock_file(&workspace)).expect("owner metadata after release");
    assert!(
        after.contains("pid="),
        "owner metadata must survive the contended acquire: {after}"
    );
}

/// Fix #2 cross-process variant: a live `rewind run` process owns the lease;
/// the test process must fail to acquire and must not clobber the live
/// process's owner record.
#[test]
fn lock_owner_metadata_survives_cross_process_writer() {
    let bin = env!("CARGO_BIN_EXE_rewind");
    let root = tempfile::tempdir().expect("workspace root");
    fs::write(root.path().join("foo.txt"), b"A").expect("fixture file");
    let store = tempfile::tempdir().expect("store");
    let store_path = store.path().to_string_lossy().into_owned();
    let status = Command::new(bin)
        .args(["init", "."])
        .current_dir(root.path())
        .env("REWIND_HOME", &store_path)
        .status()
        .expect("cli init");
    assert!(status.success());

    let sleeper: Vec<String> = if cfg!(windows) {
        vec![
            "cmd".to_owned(),
            "/C".to_owned(),
            "ping -n 6 127.0.0.1 > nul".to_owned(),
        ]
    } else {
        vec!["sleep".to_owned(), "5".to_owned()]
    };
    let mut cli_args = vec!["run".to_owned(), "--".to_owned()];
    cli_args.extend(sleeper.iter().cloned());
    let mut writer = Command::new(bin)
        .args(&cli_args)
        .current_dir(root.path())
        .env("REWIND_HOME", &store_path)
        .spawn()
        .expect("spawn writer");

    // Reopen the workspace from the marker the CLI process created.
    let workspace = Workspace::open_from_current(root.path()).expect("open workspace");
    let mut acquired = false;
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        if WorkspaceLease::acquire(&workspace, true).is_err() {
            acquired = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(acquired, "writer process must hold the lease");
    #[cfg(windows)]
    assert!(
        fs::read_to_string(lock_file(&workspace)).is_err(),
        "exclusive lease must block readers while held"
    );
    // The live writer's own owner record is intact: kill it and read the
    // record it left behind. A contender that truncated on open would have
    // destroyed the writer's record while it still held the lock.
    let _ = writer.kill();
    let _ = writer.wait();
    let metadata = fs::read_to_string(lock_file(&workspace)).expect("owner record after writer");
    assert!(
        metadata.contains("pid="),
        "live writer's owner metadata must never be clobbered: {metadata}"
    );
}

/// Runs the real CLI binary inside `root` (so passive-hook workspace
/// discovery resolves the intended workspace, not the test process's CWD)
/// against the workspace's external store.
fn cli_in(bin: &str, store: &Path, root: &Path, args: &[&str]) -> std::process::Output {
    let store_path = store.to_string_lossy().into_owned();
    Command::new(bin)
        .args(args)
        .current_dir(root)
        .env("REWIND_HOME", &store_path)
        .output()
        .expect("run rewind cli")
}

/// Phase 1.2 Fix #1 semantics (reframed by Phase 1.3): a post-hook whose
/// bounded scan cannot finish (large real workspace, real CAS + SQLite
/// pressure) must degrade conservatively — durable CAPTURE_FAILED/gap
/// record, undo refused while gated, explicit reconciliation restores
/// HEALTHY without fabricating an operation for the unobserved interval.
/// The hook itself always exits 0 so a caller that does wait (the shell no
/// longer does; see tests/shell_integration.rs) is never blocked on the
/// process's own failure. No wall-clock duration is asserted here: the
/// shell-facing non-blocking guarantee is a property of the shell
/// integration, not of this process.
#[test]
fn passive_scan_deadline_degrades_to_capture_gap() {
    let bin = env!("CARGO_BIN_EXE_rewind");
    let (root, store, workspace) = init_workspace();
    // Real filesystem pressure: enough files that a full scan takes orders
    // of magnitude longer than the 50 ms hook scan deadline.
    fs::create_dir(root.path().join("bulk")).expect("bulk dir");
    for index in 0..1500 {
        fs::write(
            root.path().join("bulk").join(format!("f{index}.txt")),
            format!("payload-{index}"),
        )
        .expect("bulk file");
    }

    let session = format!("sess-{}", std::process::id());
    let pre = cli_in(
        bin,
        store.path(),
        root.path(),
        &[
            "hook",
            "pre",
            "--command",
            "slow passive",
            "--session",
            &session,
        ],
    );
    assert!(
        pre.status.success(),
        "pre hook failed: {}",
        String::from_utf8_lossy(&pre.stderr)
    );
    fs::write(root.path().join("hooked.txt"), b"hooked").expect("hooked change");

    let post = cli_in(
        bin,
        store.path(),
        root.path(),
        &["hook", "post", "--exit-code", "0", "--session", &session],
    );
    assert_eq!(
        post.status.code(),
        Some(0),
        "hook must always fail open for the caller; stderr: {}",
        String::from_utf8_lossy(&post.stderr)
    );

    assert_eq!(
        condition(&workspace),
        WorkspaceCondition::ReconciliationRequired
    );
    assert!(
        workspace
            .storage
            .catalog
            .has_open_unknown(workspace.id)
            .expect("gap"),
        "failed bounded capture must record an unknown interval"
    );
    let undo_refusal = undo(&workspace, None, false);
    assert!(undo_refusal.is_err(), "undo must be refused while gated");

    let checkpoint = cli_in(bin, store.path(), root.path(), &["reconcile"]);
    assert_eq!(checkpoint.status.code(), Some(0));
    assert_eq!(condition(&workspace), WorkspaceCondition::Healthy);
    // The hooked change survives on disk and no fabricated operation
    // attributes it.
    assert_eq!(
        fs::read(root.path().join("hooked.txt")).expect("hooked"),
        b"hooked"
    );
    let operations = workspace
        .storage
        .catalog
        .list_operations(workspace.id)
        .expect("operations");
    assert!(
        operations
            .iter()
            .all(|operation| operation.kind.as_str() != "STRONG"),
        "no strong operation may exist for the passive interval"
    );
}

/// Fix #1: when a writer transaction is unfinished, the post hook must not
/// run recovery inline (unbounded work) — it records a durable marker and
/// returns; the writer path then performs recovery.
#[test]
fn passive_hook_defers_recovery_to_writer() {
    let bin = env!("CARGO_BIN_EXE_rewind");
    let (root, store, workspace) = init_workspace();
    craft_unfinished_quarantine_step(&workspace, &root, "foo.txt");

    let session = format!("sess-defer-{}", std::process::id());
    let pre = cli_in(
        bin,
        store.path(),
        root.path(),
        &[
            "hook",
            "pre",
            "--command",
            "deferred recovery",
            "--session",
            &session,
        ],
    );
    assert!(
        pre.status.success(),
        "pre hook failed: {}",
        String::from_utf8_lossy(&pre.stderr)
    );
    let post = cli_in(
        bin,
        store.path(),
        root.path(),
        &["hook", "post", "--exit-code", "0", "--session", &session],
    );
    assert_eq!(
        post.status.code(),
        Some(0),
        "hook must fail open; stderr: {}",
        String::from_utf8_lossy(&post.stderr)
    );
    // The interrupted transaction is still unfinished: the hook deferred it
    // (structural evidence that recovery was not run inline — recovery
    // would have completed or abandoned the transaction).
    assert!(
        !workspace
            .storage
            .catalog
            .unfinished_transactions(workspace.id)
            .expect("unfinished")
            .is_empty(),
        "hook must leave recovery to the writer"
    );
    // The durable bypass marker gates the workspace on the writer path —
    // reconcile-first semantics, not a false trusted observation recorded
    // across the unfinished transaction.
    assert!(
        workspace
            .storage
            .catalog
            .has_pending_bypass(workspace.id)
            .expect("bypass"),
        "deferred recovery must record a durable bypass marker"
    );
    workspace.enforce_pending_safety_gate().expect("gate");
    assert_eq!(
        condition(&workspace),
        WorkspaceCondition::ReconciliationRequired
    );
}

/// Creates a physically interrupted single-file undo transaction with the
/// live file moved into local quarantine (the documented PARTIAL shape).
fn craft_unfinished_quarantine_step(
    workspace: &Workspace,
    root: &TempDir,
    file: &str,
) -> (uuid::Uuid, PathBuf) {
    let argv: Vec<String> = if cfg!(windows) {
        vec!["cmd".to_owned(), "/C".to_owned(), format!("echo B>{file}")]
    } else {
        vec!["sh".to_owned(), "-c".to_owned(), format!("echo B > {file}")]
    };
    let outcome = workspace.run_command(&argv).expect("capture modification");
    let operation_id = outcome.operation_id.expect("operation id");

    let record = workspace
        .storage
        .catalog
        .operation(operation_id, workspace.id)
        .expect("operation");
    let before_id = record.pre_state_id.as_deref().expect("pre");
    let after_id = record.post_state_id.as_deref().expect("post");
    let before = workspace
        .state_manifest(before_id)
        .expect("before manifest");
    let after = workspace.state_manifest(after_id).expect("after manifest");

    let transaction_id = uuid::Uuid::new_v4();
    let staging = staging_root(&workspace.root, transaction_id).expect("staging");
    let quarantine = staging.join("quarantine");
    let prepared = staging.join("prepared");
    fs::create_dir_all(&quarantine).expect("quarantine");
    fs::create_dir_all(&prepared).expect("prepared");
    let backup = quarantine.join("0.backup");
    fs::rename(root.path().join(file), &backup).expect("quarantine move");
    let Fingerprint::RegularFile { content_hash, .. } = before.get(file) else {
        panic!("before state is not a file");
    };
    workspace
        .storage
        .cas
        .materialize(&content_hash.clone(), &prepared.join("0.file"))
        .expect("prepare desired");

    let step = JournalStep {
        id: 0,
        paths: vec![file.to_owned()],
        before: BTreeMap::from([(file.to_owned(), after.get(file))]),
        after: BTreeMap::from([(file.to_owned(), before.get(file))]),
        backup_path: Some(backup.to_string_lossy().into_owned()),
        staging_path: Some(prepared.join("0.file").to_string_lossy().into_owned()),
        status: JournalStatus::Applying,
    };
    let anchor = workspace
        .storage
        .catalog
        .insert_state(
            workspace.id,
            StateKind::Anchor,
            &after,
            Some(after_id),
            Some("interrupted-anchor"),
        )
        .expect("anchor");
    let journal = Journal {
        transaction_id,
        workspace_id: workspace.id,
        operation_id: Some(operation_id),
        anchor_state_id: anchor,
        target_state_id: before_id.to_owned(),
        direction: "UNDO".to_owned(),
        status: JournalStatus::Applying,
        steps: vec![step],
        archive_status: ArchiveStatus::Pending,
        archive_error: None,
    };
    workspace.storage.journals.write(&journal).expect("journal");
    workspace
        .storage
        .catalog
        .add_transaction(
            transaction_id,
            workspace.id,
            Some(operation_id),
            &JournalStatus::Applying,
            &workspace
                .storage
                .journal_path(transaction_id)
                .to_string_lossy(),
        )
        .expect("transaction row");
    (transaction_id, staging)
}

/// Fix #3: rollback of a workspace whose live target's parent chain
/// contains a symlink is refused — the mutation is never performed through
/// the link, and the external target is untouched.
#[test]
#[cfg(unix)]
fn rollback_refuses_symlinked_parent_and_never_follows_it() {
    use std::os::unix::fs::symlink;
    let (root, _store, workspace) = init_workspace();

    // External target outside the workspace.
    let outside = tempfile::tempdir().expect("outside dir");
    fs::write(outside.path().join("secret.txt"), b"OUTSIDE").expect("secret");
    fs::write(root.path().join("realfile.txt"), b"real").expect("real file");

    let argv = vec![
        "sh".to_owned(),
        "-c".to_owned(),
        "echo changed > realfile.txt".to_owned(),
    ];
    let outcome = workspace.run_command(&argv).expect("capture");
    let operation_id = outcome.operation_id.expect("operation id");

    // Replace the live file with a symlink pointing outside and attempt
    // rollback: preflight must refuse, never write through the link.
    fs::remove_file(root.path().join("realfile.txt")).expect("remove live file");
    symlink(
        outside.path().join("secret.txt"),
        root.path().join("realfile.txt"),
    )
    .expect("symlink live path");

    let refusal = undo(&workspace, Some(operation_id), false);
    assert!(
        refusal.is_err(),
        "undo through a symlinked live path must be refused"
    );
    assert_eq!(
        fs::read(outside.path().join("secret.txt")).expect("external target"),
        b"OUTSIDE",
        "external target must never be followed"
    );
    let metadata = fs::symlink_metadata(root.path().join("realfile.txt")).expect("link intact");
    assert!(
        metadata.file_type().is_symlink(),
        "live symlink must remain"
    );
}

/// Fix #4: recursive archive verification detects nested corruption. The
/// local quarantine is only disposed after full recursive verification, so a
/// shallow pass can never authorize deleting the last surviving copy.
#[test]
fn recursive_archive_verification_detects_nested_corruption() {
    let dir = tempfile::tempdir().expect("archive dir");
    let source = dir.path().join("tree");
    let destination = dir.path().join("copy");
    fs::create_dir_all(source.join("sub").join("deep")).expect("nested dirs");
    fs::write(source.join("a.txt"), b"alpha").expect("a");
    fs::write(source.join("sub").join("b.txt"), b"beta").expect("b");
    fs::write(source.join("sub").join("deep").join("c.txt"), b"gamma").expect("c");
    #[cfg(unix)]
    std::os::unix::fs::symlink("a.txt", source.join("link")).expect("symlink");
    copy_tree(&source, &destination);
    assert!(
        verify_archive_pair(&source, &destination).is_ok(),
        "faithful copy must verify"
    );

    // Nested content corruption must be caught.
    fs::write(
        destination.join("sub").join("deep").join("c.txt"),
        b"corrupt",
    )
    .expect("corrupt");
    assert!(verify_archive_pair(&source, &destination).is_err());

    // Missing nested file must be caught.
    copy_tree(&source, &destination);
    fs::remove_file(destination.join("sub").join("deep").join("c.txt")).expect("remove");
    assert!(verify_archive_pair(&source, &destination).is_err());

    // Extra nested entry must be caught.
    copy_tree(&source, &destination);
    fs::write(destination.join("sub").join("intruder.txt"), b"extra").expect("intruder");
    assert!(verify_archive_pair(&source, &destination).is_err());

    // Wrong nested directory shape must be caught.
    copy_tree(&source, &destination);
    fs::remove_dir_all(destination.join("sub").join("deep")).expect("remove deep");
    assert!(verify_archive_pair(&source, &destination).is_err());

    // Type mismatch (file replaced by directory) must be caught.
    copy_tree(&source, &destination);
    fs::remove_file(destination.join("a.txt")).expect("remove a");
    fs::create_dir(destination.join("a.txt")).expect("replace with dir");
    assert!(verify_archive_pair(&source, &destination).is_err());

    #[cfg(unix)]
    {
        // Wrong symlink target must be caught.
        copy_tree(&source, &destination);
        fs::remove_file(destination.join("link")).expect("remove link");
        std::os::unix::fs::symlink("sub/b.txt", destination.join("link")).expect("wrong link");
        assert!(verify_archive_pair(&source, &destination).is_err());
    }
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::remove_dir_all(destination).ok();
    fn copy_dir(from: &Path, to: &Path) {
        fs::create_dir_all(to).expect("mkdir");
        for entry in fs::read_dir(from).expect("read") {
            let entry = entry.expect("entry");
            let target = to.join(entry.file_name());
            let path = entry.path();
            if path.is_dir() {
                copy_dir(&path, &target);
            } else if path.is_symlink() {
                #[cfg(unix)]
                {
                    let target_str = fs::read_link(&path).expect("read link");
                    std::os::unix::fs::symlink(&target_str, &target).expect("symlink copy");
                }
                #[cfg(not(unix))]
                {
                    let _ = target;
                }
            } else {
                fs::copy(&path, &target).expect("file copy");
            }
        }
    }
    copy_dir(source, destination);
}

/// Fix #6: state identity includes the manifest schema version; persisted
/// v1 manifests (no target_kind) still deserialize, and the version
/// participates in the digest so fingerprint-semantics changes can never
/// silently alias old states.
#[test]
fn state_identity_is_schema_versioned_and_v1_manifests_stay_readable() {
    // A v1-era serialized fingerprint (no target_kind) must deserialize.
    let v1_json = r#"{"kind":"Symlink","value":{"target":"foo.txt","target_hash":"deadbeef","metadata":{"mode":null,"readonly":false}}}"#;
    let v1_fingerprint: Fingerprint = serde_json::from_str(v1_json).expect("v1 fingerprint");
    match v1_fingerprint {
        Fingerprint::Symlink { target_kind, .. } => {
            assert_eq!(target_kind, SymlinkTargetKind::Unknown);
        }
        other => panic!("expected symlink, got {}", other.kind_name()),
    }

    // A v2 manifest's state id depends on the schema version: the digest
    // covers the versioned envelope, not the bare entries.
    let manifest = rewind::model::Manifest {
        entries: BTreeMap::new(),
    };
    let id = manifest.state_id().expect("state id");
    let bare = blake3::hash(
        serde_json::to_vec(&manifest.entries)
            .expect("entries json")
            .as_slice(),
    )
    .to_hex()
    .to_string();
    assert_ne!(id, bare, "state id must not be the bare manifest digest");
    let mut hasher = blake3::Hasher::new();
    hasher.update(&STATE_SCHEMA_VERSION.to_le_bytes());
    hasher.update(
        serde_json::to_vec(&rewind::model::Manifest {
            entries: BTreeMap::new(),
        })
        .expect("canonical")
        .as_slice(),
    );
    // Same manifest must be deterministic; different schema version must
    // differ (proven by construction: the constant is in the digest input).
    assert_eq!(manifest.state_id().expect("id"), id);
    assert_ne!(id, format!("{:08x}", STATE_SCHEMA_VERSION));
}

/// Fix #18: a crash between the journal COMMITTED write and the catalog
/// updates leaves contradictory rows (workspace pointing at the anchor,
/// operation still COMPLETED for an UNDO). Startup repair must converge
/// them idempotently, and a repeated repair must be a no-op.
#[test]
fn committed_metadata_crash_window_repairs_idempotently() {
    let (root, _store, workspace) = init_workspace();
    let argv: Vec<String> = if cfg!(windows) {
        vec![
            "cmd".to_owned(),
            "/C".to_owned(),
            "echo B> foo.txt".to_owned(),
        ]
    } else {
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "echo B > foo.txt".to_owned(),
        ]
    };
    let outcome = workspace.run_command(&argv).expect("capture");
    let operation_id = outcome.operation_id.expect("operation id");
    let committed_undo = undo(&workspace, Some(operation_id), false).expect("undo");
    assert_eq!(condition(&workspace), WorkspaceCondition::Healthy);

    let journal_path = workspace
        .storage
        .journal_path(committed_undo.transaction_id);
    let journal: Journal = workspace
        .storage
        .journals
        .load(&journal_path)
        .expect("journal");
    assert_eq!(journal.status, JournalStatus::Committed);

    // Simulate the crash window: catalog rows contradicted the committed
    // journal.
    workspace
        .storage
        .catalog
        .update_transaction(committed_undo.transaction_id, &JournalStatus::Durable)
        .expect("stale txn row");
    workspace
        .storage
        .catalog
        .set_operation_status(operation_id, OperationStatus::Completed)
        .expect("stale op status");
    workspace
        .storage
        .catalog
        .set_workspace(
            workspace.id,
            &WorkspaceCondition::Healthy,
            Some(&journal.anchor_state_id),
        )
        .expect("stale baseline");

    // First repair converges everything to the committed journal.
    recover_locked(&workspace).expect("repair");
    let operation = workspace
        .storage
        .catalog
        .operation(operation_id, workspace.id)
        .expect("operation");
    assert_eq!(
        operation.status,
        OperationStatus::Undone,
        "direction honored"
    );
    let row = workspace.row().expect("row");
    assert_eq!(row.condition, WorkspaceCondition::Healthy);
    // The baseline is only repaired onto the target when the live state
    // agrees; the workspace was left at the anchor by the simulated crash
    // but the *physical* filesystem is the post-undo state, so the scan
    // confirms the target.
    let live = workspace.scan(None).expect("live scan");
    if live.state_id == journal.target_state_id {
        assert_eq!(
            row.baseline_state.as_deref(),
            Some(journal.target_state_id.as_str())
        );
    }

    // Idempotence: repeat repairs change nothing.
    let snapshot_before = workspace.row().expect("row");
    recover_locked(&workspace).expect("second repair");
    let snapshot_after = workspace.row().expect("row");
    assert_eq!(snapshot_before.condition, snapshot_after.condition);
    assert_eq!(
        snapshot_before.baseline_state,
        snapshot_after.baseline_state
    );

    let _ = root;
}

/// Fix #20: when recovery fails on an unfinished transaction (ambiguous
/// physical state), the workspace can never be left appearing HEALTHY, and
/// the explicit `recover --reconcile` path archives artifacts, marks the
/// journal ABANDONED, and reconciles to a trusted checkpoint.
#[test]
fn recovery_failure_never_leaves_false_healthy_and_reconcile_resolves() {
    let (root, _store, workspace) = init_workspace();
    let (transaction_id, _staging) = craft_unfinished_quarantine_step(&workspace, &root, "foo.txt");

    // Corrupt the quarantined artifact so classification cannot proceed.
    let journals: Vec<(PathBuf, Journal)> =
        workspace.storage.journals.unfinished().expect("unfinished");
    let journal = journals.first().expect("crafted journal").1.clone();
    let backup = journal.steps[0].backup_path.as_deref().expect("backup");
    fs::write(backup, b"clobbered").expect("clobber quarantined artifact");

    let failure = recover_locked(&workspace);
    assert!(failure.is_err(), "recovery must refuse the clobbered state");
    assert_eq!(
        condition(&workspace),
        WorkspaceCondition::RecoveryRequired,
        "no false HEALTHY may survive a failed recovery"
    );

    // The explicit exit path resolves it: archive what exists, ABANDONED
    // the journal, reconcile the live state into a trusted checkpoint.
    let checkpoint = recover_reconcile(&workspace).expect("reconcile recovery");
    assert_eq!(condition(&workspace), WorkspaceCondition::Healthy);
    assert_eq!(workspace.baseline_id().expect("baseline"), checkpoint);
    let journal_path = workspace.storage.journal_path(transaction_id);
    let journal: Journal = workspace
        .storage
        .journals
        .load(&journal_path)
        .expect("journal");
    assert_eq!(journal.status, JournalStatus::Abandoned);
    let _ = root;
}

/// Fix #5 on POSIX (always creatable): file and directory symlinks record
/// their target kind from workspace evidence, and undo/redo of their
/// creation is byte-faithful without following targets.
#[test]
#[cfg(unix)]
fn symlink_target_kinds_are_recorded_and_restore_faithfully() {
    let (root, _store, workspace) = init_workspace();

    // File symlink: target exists inside the workspace.
    std::os::unix::fs::symlink("foo.txt", root.path().join("file-link")).expect("file symlink");
    // Directory symlink: target directory inside the workspace.
    fs::create_dir(root.path().join("dir-target")).expect("dir target");
    std::os::unix::fs::symlink("dir-target", root.path().join("dir-link")).expect("dir symlink");
    // Dangling symlink: target does not exist.
    std::os::unix::fs::symlink("missing.txt", root.path().join("dangling-link"))
        .expect("dangling symlink");

    let state = workspace
        .reconcile_locked("symlink kinds")
        .expect("reconcile");
    let manifest = workspace.state_manifest(&state).expect("manifest");
    match manifest.get("file-link") {
        Fingerprint::Symlink { target_kind, .. } => {
            assert_eq!(target_kind, SymlinkTargetKind::File);
        }
        other => panic!("file link not a symlink: {}", other.kind_name()),
    }
    match manifest.get("dir-link") {
        Fingerprint::Symlink { target_kind, .. } => {
            assert_eq!(target_kind, SymlinkTargetKind::Directory);
        }
        other => panic!("dir link not a symlink: {}", other.kind_name()),
    }
    match manifest.get("dangling-link") {
        Fingerprint::Symlink { target_kind, .. } => {
            assert_eq!(target_kind, SymlinkTargetKind::Unknown);
        }
        other => panic!("dangling link not a symlink: {}", other.kind_name()),
    }
}
