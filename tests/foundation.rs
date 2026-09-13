use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use rewind::model::{
    ArchiveStatus, Fingerprint, Journal, JournalStatus, JournalStep, StateKind, WorkspaceCondition,
};
use rewind::paths::staging_root;
use rewind::rollback::{redo, undo};
use rewind::workspace::Workspace;
mod common;

use tempfile::TempDir;

fn fixture() -> (TempDir, TempDir, Workspace) {
    let root = tempfile::tempdir().expect("temporary root");
    fs::write(root.path().join("foo.txt"), b"A").expect("write fixture");
    fs::create_dir(root.path().join("empty")).expect("create directory");
    let store = tempfile::tempdir().expect("temporary store");
    let workspace = Workspace::init(root.path(), Some(store.path())).expect("initialize");
    (root, store, workspace)
}

#[test]
fn initialization_creates_external_catalog_and_typed_state() {
    let (root, _store, workspace) = fixture();
    let row = workspace.row().expect("workspace row");
    assert_eq!(row.condition, WorkspaceCondition::Healthy);
    let baseline = workspace.baseline_id().expect("baseline");
    let state = workspace.storage.catalog.state(&baseline).expect("state");
    assert!(matches!(
        state.manifest.entries.get("foo.txt"),
        Some(Fingerprint::RegularFile { .. })
    ));
    assert!(matches!(
        state.manifest.entries.get("empty"),
        Some(Fingerprint::Directory { .. })
    ));
    assert!(!root.path().join(".rewind").join("metadata.sqlite").exists());
}

#[test]
fn strong_run_undo_and_redo_restore_state_without_rerunning_command() {
    let (root, _store, workspace) = fixture();
    let outcome = workspace
        .run_command(&run_echo("foo.txt", "B"))
        .expect("run command");
    assert!(outcome.captured);
    assert_eq!(
        fs::read_to_string(root.path().join("foo.txt")).expect("read"),
        String::from_utf8(common::echoed("B")).expect("utf8")
    );

    let undone = undo(&workspace, outcome.operation_id, false).expect("undo");
    assert_eq!(
        fs::read_to_string(root.path().join("foo.txt")).expect("read after undo"),
        "A"
    );
    assert_eq!(
        workspace.baseline_id().expect("baseline"),
        undone.target_state_id
    );

    let redone = redo(&workspace, outcome.operation_id).expect("redo");
    assert_eq!(
        fs::read_to_string(root.path().join("foo.txt")).expect("read after redo"),
        String::from_utf8(common::echoed("B")).expect("utf8")
    );
    assert_eq!(
        workspace.baseline_id().expect("baseline"),
        redone.target_state_id
    );
}

#[test]
fn deletion_creation_and_type_replacement_restore_both_directions() {
    let (root, _store, workspace) = fixture();
    let deleted = workspace
        .run_command(&delete_command("foo.txt"))
        .expect("capture deletion");
    assert!(!root.path().join("foo.txt").exists());
    undo(&workspace, deleted.operation_id, false).expect("undo deletion");
    assert_eq!(
        fs::read_to_string(root.path().join("foo.txt")).expect("restored file"),
        "A"
    );
    redo(&workspace, deleted.operation_id).expect("redo deletion");
    assert!(!root.path().join("foo.txt").exists());

    let created = workspace
        .run_command(&run_echo("new.txt", "N"))
        .expect("capture creation");
    undo(&workspace, created.operation_id, false).expect("undo creation");
    assert!(!root.path().join("new.txt").exists());
    redo(&workspace, created.operation_id).expect("redo creation");
    assert_eq!(
        fs::read_to_string(root.path().join("new.txt")).expect("recreated file"),
        "N\r\n"
    );

    fs::write(root.path().join("foo.txt"), b"A").expect("recreate source");
    let type_change = workspace
        .run_command(&replace_with_directory("foo.txt"))
        .expect("capture type replacement");
    assert!(root.path().join("foo.txt").is_dir());
    undo(&workspace, type_change.operation_id, false).expect("undo type replacement");
    assert_eq!(
        fs::read_to_string(root.path().join("foo.txt")).expect("restored type"),
        "A"
    );
}

#[test]
fn conflict_refuses_and_force_preserves_live_bytes() {
    let (root, _store, workspace) = fixture();
    let outcome = workspace
        .run_command(&run_echo("foo.txt", "B"))
        .expect("capture");
    fs::write(root.path().join("foo.txt"), b"external").expect("external edit");
    let conflict = undo(&workspace, outcome.operation_id, false);
    assert!(matches!(
        conflict,
        Err(rewind::RewindError::Conflict { .. })
    ));
    assert_eq!(
        fs::read_to_string(root.path().join("foo.txt")).expect("live file"),
        "external"
    );
    undo(&workspace, outcome.operation_id, true).expect("forced undo");
    assert_eq!(
        fs::read_to_string(root.path().join("foo.txt")).expect("forced state"),
        "A"
    );
}

#[test]
fn directory_child_mutation_does_not_quarantine_the_parent_directory() {
    let (root, _store, workspace) = fixture();
    let outcome = workspace
        .run_command(&create_path_then_write("empty", "new.txt", "child"))
        .expect("capture child creation");
    assert!(root.path().join("empty/new.txt").is_file());
    undo(&workspace, outcome.operation_id, false).expect("undo child creation");
    assert!(root.path().join("empty").is_dir());
    assert!(!root.path().join("empty/new.txt").exists());
    redo(&workspace, outcome.operation_id).expect("redo child creation");
    assert_eq!(
        fs::read_to_string(root.path().join("empty/new.txt")).expect("redo child"),
        "child\r\n"
    );
}

#[test]
fn recovery_completes_known_crash_after_quarantine_before_install() {
    let (root, _store, workspace) = fixture();
    let operation = workspace
        .run_command(&run_echo("foo.txt", "B"))
        .expect("capture");
    let operation_id = operation.operation_id.expect("operation id");
    let record = workspace
        .storage
        .catalog
        .operation(operation_id, workspace.id)
        .expect("operation");
    let before_id = record.pre_state_id.as_deref().expect("pre-state");
    let after_id = record.post_state_id.as_deref().expect("post-state");
    let before = workspace.storage.catalog.state(before_id).expect("before");
    let after = workspace.storage.catalog.state(after_id).expect("after");
    let transaction_id = uuid::Uuid::new_v4();
    let staging = staging_root(&workspace.root, transaction_id).expect("staging");
    let quarantine = staging.join("quarantine");
    let prepared = staging.join("prepared");
    fs::create_dir_all(&quarantine).expect("quarantine");
    fs::create_dir_all(&prepared).expect("prepared");
    let backup = quarantine.join("0.backup");
    let stage = prepared.join("0.file");
    fs::rename(root.path().join("foo.txt"), &backup).expect("simulate quarantine move");
    let Fingerprint::RegularFile { content_hash, .. } =
        before.manifest.entries.get("foo.txt").expect("before file")
    else {
        panic!("before state is not a file");
    };
    workspace
        .storage
        .cas
        .materialize(content_hash, &stage)
        .expect("prepare desired artifact");
    let step = JournalStep {
        id: 0,
        paths: vec!["foo.txt".to_owned()],
        before: BTreeMap::from([("foo.txt".to_owned(), after.manifest.get("foo.txt"))]),
        after: BTreeMap::from([("foo.txt".to_owned(), before.manifest.get("foo.txt"))]),
        backup_path: Some(backup.to_string_lossy().into_owned()),
        staging_path: Some(stage.to_string_lossy().into_owned()),
        status: JournalStatus::Applying,
    };
    let anchor = workspace
        .storage
        .catalog
        .insert_state(
            workspace.id,
            StateKind::Anchor,
            &after.manifest,
            Some(after_id),
            Some("test-anchor"),
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
        .expect("transaction");
    rewind::rollback::recover_locked(&workspace).expect("recover known partial");
    assert_eq!(
        fs::read_to_string(root.path().join("foo.txt")).expect("recovered"),
        "A"
    );
    assert_eq!(
        workspace
            .storage
            .catalog
            .operation(operation_id, workspace.id)
            .expect("recovered operation")
            .status,
        rewind::model::OperationStatus::Undone
    );
}

#[test]
fn snapshot_restore_is_a_state_transition_and_is_undoable() {
    let (root, _store, workspace) = fixture();
    let snapshot = workspace.create_snapshot("before-c").expect("snapshot");
    workspace
        .run_command(&run_echo("foo.txt", "C"))
        .expect("capture C");
    let restored =
        rewind::rollback::restore_snapshot(&workspace, "before-c").expect("restore snapshot");
    assert_eq!(restored.target_state_id, snapshot);
    assert_eq!(
        fs::read_to_string(root.path().join("foo.txt")).expect("restored"),
        "A"
    );
    undo(&workspace, restored.operation_id, false).expect("undo restore");
    assert_eq!(
        fs::read_to_string(root.path().join("foo.txt")).expect("undo restore"),
        String::from_utf8(common::echoed("C")).expect("utf8")
    );
}

#[test]
fn initialization_is_idempotent_and_external_store_is_required() {
    let root = tempfile::tempdir().expect("temporary root");
    let store = tempfile::tempdir().expect("temporary store");
    let first = Workspace::init(root.path(), Some(store.path())).expect("initialize");
    let second = Workspace::init(root.path(), Some(store.path())).expect("repeat init");
    assert_eq!(first.id, second.id);
    let inside_root = tempfile::tempdir().expect("inside test root");
    let inside = Workspace::init(inside_root.path(), Some(&inside_root.path().join("store")));
    assert!(inside.is_err());
}

#[test]
fn cas_detects_corruption_and_cleans_incomplete_objects() {
    let (root, store, workspace) = fixture();
    let baseline = workspace.baseline_id().expect("baseline");
    let state = workspace.storage.catalog.state(&baseline).expect("state");
    let Some(Fingerprint::RegularFile { content_hash, .. }) = state.manifest.entries.get("foo.txt")
    else {
        panic!("fixture file was not captured");
    };
    let object = workspace
        .storage
        .cas
        .blob_path(content_hash)
        .expect("CAS path");
    let mut permissions = fs::metadata(&object).expect("metadata").permissions();
    #[cfg(windows)]
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o600);
    }
    fs::set_permissions(&object, permissions).expect("make corruptible");
    fs::write(&object, b"corrupt").expect("corrupt CAS object");
    assert!(workspace.storage.cas.verify(content_hash).is_err());

    let partial = store.path().join("cas").join("tmp").join("orphan.part");
    fs::write(&partial, b"partial").expect("partial object");
    let _ = rewind::cas::Cas::new(store.path().join("cas")).expect("reopen CAS");
    assert!(!partial.exists());
    let _ = root;
}

#[test]
fn failed_capture_enters_reconciliation_and_does_not_fabricate_operation() {
    let (root, _store, workspace) = fixture();
    let baseline = workspace.baseline_id().expect("baseline");
    let failed = workspace
        .record_capture_failure(
            Some(&baseline),
            Some("first".to_owned()),
            Some(root.path().to_string_lossy().into_owned()),
            Some(0),
            "injected post-scan failure",
        )
        .expect("record failure");
    assert!(failed > 0);
    assert_eq!(
        workspace.condition().expect("condition"),
        WorkspaceCondition::ReconciliationRequired
    );
    fs::write(root.path().join("foo.txt"), b"C").expect("out-of-band change");
    let outcome = workspace
        .run_command(&run_echo("foo.txt", "D"))
        .expect("run after reconciliation");
    assert!(outcome.captured);
    let operations = workspace
        .storage
        .catalog
        .list_operations(workspace.id)
        .expect("operations");
    assert!(operations
        .iter()
        .any(|operation| operation.id == failed && operation.kind.as_str() == "CAPTURE_FAILED"));
    assert!(!operations.iter().any(|operation| {
        operation.kind.as_str() == "STRONG"
            && operation
                .pre_state_id
                .as_deref()
                .is_some_and(|state| state == baseline)
    }));
}

#[test]
fn file_delete_and_creation_are_explicit_states() {
    let (root, _store, workspace) = fixture();
    fs::remove_file(root.path().join("foo.txt")).expect("delete");
    let reconcile = workspace.reconcile_locked("test").expect("reconcile");
    let state = workspace.storage.catalog.state(&reconcile).expect("state");
    assert_eq!(state.manifest.get("foo.txt"), Fingerprint::Absent);
    fs::write(root.path().join("new.txt"), b"N").expect("create");
    let next = workspace.reconcile_locked("test").expect("reconcile");
    let state = workspace.storage.catalog.state(&next).expect("state");
    assert!(matches!(
        state.manifest.entries.get("new.txt"),
        Some(Fingerprint::RegularFile { .. })
    ));
}

#[test]
fn symlink_is_recorded_without_following_when_supported() {
    let (root, _store, workspace) = fixture();
    #[cfg(windows)]
    if let Err(error) = std::os::windows::fs::symlink_file("foo.txt", root.path().join("link.txt"))
    {
        if error.raw_os_error() == Some(1314) {
            eprintln!("skipping symlink test: Windows symlink privilege is unavailable");
            return;
        }
        panic!("symlink support: {error}");
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink("foo.txt", root.path().join("link.txt")).expect("symlink support");
    let state = workspace
        .reconcile_locked("symlink test")
        .expect("reconcile");
    let manifest = workspace
        .storage
        .catalog
        .state(&state)
        .expect("state")
        .manifest;
    assert!(matches!(
        manifest.entries.get("link.txt"),
        Some(Fingerprint::Symlink { .. })
    ));
}

#[test]
fn path_confinement_rejects_parent_escape() {
    let root = tempfile::tempdir().expect("temporary root");
    let store = tempfile::tempdir().expect("temporary store");
    let workspace = Workspace::init(root.path(), Some(store.path())).expect("initialize");
    let result = rewind::paths::workspace_path(Path::new(workspace.root.as_path()), "../escape");
    assert!(result.is_err());
}

// ---- portable supervised-command vectors --------------------------------
// Each helper returns a single-command argv native to the platform, so the
// suite asserts identical rollback behavior on Windows and POSIX CI.

fn run_command_vector(script: &str, windows: &str, posix: &str) -> Vec<String> {
    common::shell_script(&scratch_dir(), script, windows, posix)
}

use std::sync::atomic::{AtomicUsize, Ordering};
static SCRATCH_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn scratch_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rewind-fixture-{}-{}",
        std::process::id(),
        SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn run_echo(file: &str, content: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![
            "cmd".to_owned(),
            "/C".to_owned(),
            format!("echo {content}>{file}"),
        ]
    } else {
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            format!("echo {content} > {file}"),
        ]
    }
}

fn delete_command(file: &str) -> Vec<String> {
    if cfg!(windows) {
        vec!["cmd".to_owned(), "/C".to_owned(), format!("del {file}")]
    } else {
        vec!["rm".to_owned(), file.to_owned()]
    }
}

fn replace_with_directory(path: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![
            "cmd".to_owned(),
            "/C".to_owned(),
            format!("del {path} & mkdir {path}"),
        ]
    } else {
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            format!("rm {path} && mkdir {path}"),
        ]
    }
}

fn create_path_then_write(dir: &str, file: &str, content: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![
            "cmd".to_owned(),
            "/C".to_owned(),
            format!("echo {content}>{dir}\\{file}"),
        ]
    } else {
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            format!("echo {content} > {dir}/{file}"),
        ]
    }
}

#[allow(dead_code)]
fn unused_script_helper() -> Vec<String> {
    run_command_vector(
        "never",
        "@echo off
",
        "#!/bin/sh
",
    )
}
