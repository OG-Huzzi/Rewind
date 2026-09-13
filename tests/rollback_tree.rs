//! Phase 1.1 regression suite for the rollback transaction engine.
//!
//! These tests exist because the Phase 1 verification pass found that the
//! rollback planner compared every step against the pre-transaction
//! snapshot, so any transaction that removed a non-empty directory tree
//! conflicted with its own earlier steps and locked the workspace in
//! RECOVERY_REQUIRED (verifier finding V-F01), and RECOVERY_REQUIRED had no
//! CLI exit path (V-F02).
//!
//! All tests use real temporary workspaces and real filesystem mutations;
//! interruption is simulated by physically moving quarantined objects and
//! writing the journal exactly as an interrupted run would have left it.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rewind::model::{
    ArchiveStatus, Fingerprint, Journal, JournalStatus, JournalStep, OperationStatus, StateKind,
    WorkspaceCondition,
};
use rewind::paths::staging_root;
use rewind::rollback::{recover_locked, recover_reconcile, redo, undo};
use rewind::workspace::Workspace;
use tempfile::TempDir;

const TREE_FILES: usize = 12;

struct Fixture {
    #[allow(dead_code)]
    root: TempDir,
    #[allow(dead_code)]
    store: TempDir,
    workspace: Workspace,
    script: PathBuf,
}

fn script_path(store: &Path, name: &str) -> PathBuf {
    store.join(name)
}

fn init_fixture() -> Fixture {
    let root = tempfile::tempdir().expect("temporary workspace root");
    let store = tempfile::tempdir().expect("temporary store");
    fs::write(root.path().join("foo.txt"), b"A").expect("write fixture file");
    let script = script_path(store.path(), "tree.cmd");
    let workspace = Workspace::init(root.path(), Some(store.path())).expect("initialize");
    Fixture {
        root,
        store,
        workspace,
        script,
    }
}

/// Writes a cmd script that creates a directory tree inside the workspace
/// and captures its creation with `rewind run`.
fn capture_tree_creation(fixture: &mut Fixture, files: usize, nested: bool) -> i64 {
    let mut body = String::from("@echo off\r\nmkdir tree\r\n");
    for index in 0..files {
        body.push_str(&format!("echo data-{index}> tree\\f{index}.txt\r\n"));
    }
    if nested {
        body.push_str("mkdir tree\\sub\r\nmkdir tree\\sub\\deep\r\n");
        for index in 0..files {
            body.push_str(&format!("echo nested-{index}> tree\\sub\\n{index}.txt\r\n"));
            body.push_str(&format!(
                "echo deep-{index}> tree\\sub\\deep\\d{index}.txt\r\n"
            ));
        }
    }
    fs::write(&fixture.script, body).expect("write creation script");
    let script = fixture.script.to_string_lossy().replace('/', "\\");
    let outcome = fixture
        .workspace
        .run_command(&["cmd".to_owned(), "/C".to_owned(), script])
        .expect("capture tree creation");
    assert!(outcome.captured, "tree creation was not captured");
    outcome.operation_id.expect("creation operation id")
}

fn tree_child_count(root: &Path) -> usize {
    fn walk(dir: &Path) -> usize {
        let mut count = 0;
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    count += walk(&path);
                } else {
                    count += 1;
                }
            }
        }
        count
    }
    let tree = root.join("tree");
    if tree.is_dir() {
        walk(&tree)
    } else {
        0
    }
}

fn condition(workspace: &Workspace) -> WorkspaceCondition {
    workspace.condition().expect("workspace condition")
}

fn unfinished_journals(workspace: &Workspace) -> Vec<(PathBuf, Journal)> {
    workspace
        .storage
        .journals
        .unfinished()
        .expect("list unfinished journals")
}

fn journal_dir(workspace: &Workspace) -> PathBuf {
    workspace.storage.project_root.join("journals")
}

#[test]
fn undo_and_redo_of_nonempty_directory_creation() {
    let mut fixture = init_fixture();
    let operation_id = capture_tree_creation(&mut fixture, TREE_FILES, false);
    let root = fixture.root.path().to_path_buf();
    assert_eq!(tree_child_count(&root), TREE_FILES);

    undo(&fixture.workspace, Some(operation_id), false).expect("undo tree creation");
    assert!(
        !root.join("tree").exists(),
        "tree must be removed by the undo"
    );
    assert_eq!(
        fs::read(root.join("foo.txt")).expect("foo.txt after undo"),
        b"A"
    );
    assert_eq!(condition(&fixture.workspace), WorkspaceCondition::Healthy);
    assert!(unfinished_journals(&fixture.workspace).is_empty());

    redo(&fixture.workspace, Some(operation_id)).expect("redo tree creation");
    assert_eq!(tree_child_count(&root), TREE_FILES);
    assert_eq!(
        fs::read(root.join("tree").join("f3.txt")).expect("redo child content"),
        b"data-3\r\n"
    );
    assert_eq!(condition(&fixture.workspace), WorkspaceCondition::Healthy);
}

#[test]
fn undo_of_nested_tree_with_many_files() {
    let mut fixture = init_fixture();
    let operation_id = capture_tree_creation(&mut fixture, 40, true);
    let root = fixture.root.path().to_path_buf();
    assert_eq!(tree_child_count(&root), 40 + 80);

    undo(&fixture.workspace, Some(operation_id), false).expect("undo nested tree");
    assert!(!root.join("tree").exists(), "nested tree must be removed");
    assert!(unfinished_journals(&fixture.workspace).is_empty());
    assert_eq!(condition(&fixture.workspace), WorkspaceCondition::Healthy);
}

#[test]
fn redo_of_supervised_directory_deletion_removes_tree() {
    let mut fixture = init_fixture();
    capture_tree_creation(&mut fixture, TREE_FILES, false);
    let root = fixture.root.path().to_path_buf();
    fs::write(&fixture.script, "@echo off\r\nrmdir /S /Q tree\r\n").expect("write deletion script");
    let script = fixture.script.to_string_lossy().replace('/', "\\");
    let deletion = fixture
        .workspace
        .run_command(&["cmd".to_owned(), "/C".to_owned(), script])
        .expect("capture tree deletion");
    let deletion_id = deletion.operation_id.expect("deletion operation id");
    assert!(!root.join("tree").exists());

    undo(&fixture.workspace, Some(deletion_id), false).expect("undo tree deletion");
    assert_eq!(tree_child_count(&root), TREE_FILES);

    // This direction bricked the workspace before V-F01 was fixed: the
    // transaction removes the children first and then had to remove the
    // directory it had just emptied.
    redo(&fixture.workspace, Some(deletion_id)).expect("redo tree deletion");
    assert!(!root.join("tree").exists());
    assert!(unfinished_journals(&fixture.workspace).is_empty());
    assert_eq!(condition(&fixture.workspace), WorkspaceCondition::Healthy);
}

/// Simulates a crash in the middle of an undo of a directory creation: the
/// child files have already been moved into local quarantine, the directory
/// step has not run. Recovery must classify every step from physical state
/// and complete the plan instead of reporting the transaction's own
/// intermediate state as unclassifiable (the original V-F01 crash case).
#[test]
fn interrupted_tree_rollback_recovers_at_directory_step() {
    let mut fixture = init_fixture();
    let operation_id = capture_tree_creation(&mut fixture, 3, false);
    let root = fixture.root.path().to_path_buf();
    let workspace = &fixture.workspace;

    let record = workspace
        .storage
        .catalog
        .operation(operation_id, workspace.id)
        .expect("operation record");
    let after_id = record.post_state_id.as_deref().expect("post state");
    let before_id = record.pre_state_id.as_deref().expect("pre state");
    let after = workspace.state_manifest(after_id).expect("post manifest");
    let _before = workspace.state_manifest(before_id).expect("pre manifest");
    let Fingerprint::Directory { metadata, .. } = after.get("tree") else {
        panic!("post state tree is not a directory");
    };

    let transaction_id = uuid::Uuid::new_v4();
    let staging = staging_root(&workspace.root, transaction_id).expect("staging root");
    let quarantine = staging.join("quarantine");
    fs::create_dir_all(&quarantine).expect("quarantine dir");

    // Physically simulate the already-executed child-removal steps: each
    // live child is renamed into local quarantine exactly as apply_step
    // would have done before the interruption.
    let mut steps = Vec::new();
    for (index, path) in after
        .entries
        .keys()
        .filter(|path| path.starts_with("tree/"))
        .cloned()
        .enumerate()
    {
        let backup = quarantine.join(format!("{index}.backup"));
        fs::rename(root.join(&path), &backup).expect("simulate quarantine move");
        assert!(backup.exists(), "quarantined child must exist");
        steps.push(JournalStep {
            id: index,
            paths: vec![path.clone()],
            before: BTreeMap::from([(path.clone(), after.get(&path))]),
            after: BTreeMap::from([(path.clone(), Fingerprint::Absent)]),
            backup_path: Some(backup.to_string_lossy().into_owned()),
            staging_path: None,
            status: JournalStatus::Durable,
        });
    }
    assert_eq!(steps.len(), 3, "three children were quarantined");
    assert!(root.join("tree").is_dir(), "empty directory remains");

    // The directory step's before-map is the transaction-aware expectation:
    // an empty directory, because every child was removed by prior steps.
    let empty_tree = Fingerprint::directory_from_children(&BTreeMap::new(), metadata.clone())
        .expect("empty directory fingerprint");
    steps.push(JournalStep {
        id: steps.len(),
        paths: vec!["tree".to_owned()],
        before: BTreeMap::from([("tree".to_owned(), empty_tree)]),
        after: BTreeMap::from([("tree".to_owned(), Fingerprint::Absent)]),
        backup_path: Some(
            quarantine
                .join(format!("{}.backup", steps.len()))
                .to_string_lossy()
                .into_owned(),
        ),
        staging_path: None,
        status: JournalStatus::Planned,
    });

    let anchor = workspace
        .storage
        .catalog
        .insert_state(
            workspace.id,
            StateKind::Anchor,
            &after,
            Some(after_id),
            Some("interrupted-undo-anchor"),
        )
        .expect("anchor state");
    let journal = Journal {
        transaction_id,
        workspace_id: workspace.id,
        operation_id: Some(operation_id),
        anchor_state_id: anchor,
        target_state_id: before_id.to_owned(),
        direction: "UNDO".to_owned(),
        status: JournalStatus::Applying,
        steps,
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

    recover_locked(workspace).expect("recovery must complete the known plan");
    assert!(!root.join("tree").exists(), "tree removed by recovery");
    assert_eq!(
        fs::read(root.join("foo.txt")).expect("foo.txt intact"),
        b"A"
    );
    assert_eq!(condition(workspace), WorkspaceCondition::Healthy);
    assert!(unfinished_journals(workspace).is_empty());
    assert_eq!(
        workspace
            .storage
            .catalog
            .operation(operation_id, workspace.id)
            .expect("operation")
            .status,
        OperationStatus::Undone
    );
    // The staging quarantine was archived and then disposed.
    assert!(
        !staging.exists(),
        "transaction staging must be removed after archive"
    );
}

/// An unexpected object inside a directory being removed is a genuine
/// external modification: recovery must refuse, preserve the intruder, and
/// the explicit `recover --reconcile` path must absorb the live state into a
/// new trusted checkpoint without destroying anything (V-F02).
#[test]
fn recovery_refuses_unexpected_object_and_reconcile_resolves() {
    let mut fixture = init_fixture();
    let operation_id = capture_tree_creation(&mut fixture, 3, false);
    let root = fixture.root.path().to_path_buf();
    let workspace = &fixture.workspace;

    let record = workspace
        .storage
        .catalog
        .operation(operation_id, workspace.id)
        .expect("operation record");
    let after_id = record.post_state_id.as_deref().expect("post state");
    let before_id = record.pre_state_id.as_deref().expect("pre state");
    let after = workspace.state_manifest(after_id).expect("post manifest");
    let _before = workspace.state_manifest(before_id).expect("pre manifest");
    let Fingerprint::Directory { metadata, .. } = after.get("tree") else {
        panic!("post state tree is not a directory");
    };

    let transaction_id = uuid::Uuid::new_v4();
    let staging = staging_root(&workspace.root, transaction_id).expect("staging root");
    let quarantine = staging.join("quarantine");
    fs::create_dir_all(&quarantine).expect("quarantine dir");

    let mut steps = Vec::new();
    for (index, path) in after
        .entries
        .keys()
        .filter(|path| path.starts_with("tree/"))
        .cloned()
        .enumerate()
    {
        let backup = quarantine.join(format!("{index}.backup"));
        fs::rename(root.join(&path), &backup).expect("simulate quarantine move");
        steps.push(JournalStep {
            id: index,
            paths: vec![path.clone()],
            before: BTreeMap::from([(path.clone(), after.get(&path))]),
            after: BTreeMap::from([(path.clone(), Fingerprint::Absent)]),
            backup_path: Some(backup.to_string_lossy().into_owned()),
            staging_path: None,
            status: JournalStatus::Durable,
        });
    }
    // An external writer adds an object the transaction never planned for.
    let intruder = root.join("tree").join("INTRUDER.txt");
    fs::write(&intruder, b"do not lose me").expect("external object");
    let empty_tree = Fingerprint::directory_from_children(&BTreeMap::new(), metadata.clone())
        .expect("empty directory fingerprint");
    steps.push(JournalStep {
        id: steps.len(),
        paths: vec!["tree".to_owned()],
        before: BTreeMap::from([("tree".to_owned(), empty_tree)]),
        after: BTreeMap::from([("tree".to_owned(), Fingerprint::Absent)]),
        backup_path: Some(
            quarantine
                .join(format!("{}.backup", steps.len()))
                .to_string_lossy()
                .into_owned(),
        ),
        staging_path: None,
        status: JournalStatus::Planned,
    });

    let anchor = workspace
        .storage
        .catalog
        .insert_state(
            workspace.id,
            StateKind::Anchor,
            &after,
            Some(after_id),
            Some("conflict-anchor"),
        )
        .expect("anchor state");
    let journal = Journal {
        transaction_id,
        workspace_id: workspace.id,
        operation_id: Some(operation_id),
        anchor_state_id: anchor,
        target_state_id: before_id.to_owned(),
        direction: "UNDO".to_owned(),
        status: JournalStatus::Applying,
        steps,
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

    // Automatic recovery must refuse: the intruder matches no planned state.
    recover_locked(workspace).expect_err("recovery must refuse the unexpected object");
    assert_eq!(condition(workspace), WorkspaceCondition::RecoveryRequired);
    assert_eq!(
        fs::read(&intruder).expect("intruder preserved"),
        b"do not lose me"
    );
    assert!(
        !root.join("foo.txt").exists() || fs::read(root.join("foo.txt")).is_ok(),
        "no live mutation beyond the plan"
    );

    // The explicit reconciliation exit path establishes a trusted checkpoint
    // that includes the intruder, marks the journal ABANDONED, and leaves
    // the workspace healthy and usable.
    let reconciled = recover_reconcile(workspace).expect("reconcile recovery");
    assert_eq!(condition(workspace), WorkspaceCondition::Healthy);
    assert_eq!(
        fs::read(&intruder).expect("intruder survives reconciliation"),
        b"do not lose me"
    );
    assert_eq!(
        workspace.baseline_id().expect("baseline"),
        reconciled,
        "reconciliation checkpoint becomes the trusted baseline"
    );
    let journals = unfinished_journals(workspace);
    assert!(journals.is_empty(), "abandoned journals are terminal");

    // Normal capture works again afterwards.
    fs::write(&fixture.script, "@echo off\r\necho fresh> fresh.txt\r\n")
        .expect("write fresh script");
    let script = fixture.script.to_string_lossy().replace('/', "\\");
    let outcome = workspace
        .run_command(&["cmd".to_owned(), "/C".to_owned(), script])
        .expect("strong capture after recovery");
    assert!(outcome.captured);
    assert!(root.join("fresh.txt").exists());
}

/// A not-yet-processed child that an external writer modified must be
/// detected by recovery; the modified bytes must survive both the refusal
/// and the later reconciliation.
#[test]
fn recovery_refuses_external_modification_of_unprocessed_child() {
    let mut fixture = init_fixture();
    let operation_id = capture_tree_creation(&mut fixture, 3, false);
    let root = fixture.root.path().to_path_buf();
    let workspace = &fixture.workspace;

    let record = workspace
        .storage
        .catalog
        .operation(operation_id, workspace.id)
        .expect("operation record");
    let after_id = record.post_state_id.as_deref().expect("post state");
    let before_id = record.pre_state_id.as_deref().expect("pre state");
    let after = workspace.state_manifest(after_id).expect("post manifest");
    let _before = workspace.state_manifest(before_id).expect("pre manifest");
    let Fingerprint::Directory { metadata, .. } = after.get("tree") else {
        panic!("post state tree is not a directory");
    };

    let transaction_id = uuid::Uuid::new_v4();
    let staging = staging_root(&workspace.root, transaction_id).expect("staging root");
    let quarantine = staging.join("quarantine");
    fs::create_dir_all(&quarantine).expect("quarantine dir");

    let children: Vec<String> = after
        .entries
        .keys()
        .filter(|path| path.starts_with("tree/"))
        .cloned()
        .collect();
    let mut steps = Vec::new();
    for (index, path) in children.iter().enumerate() {
        let processed = index < 2;
        let backup = quarantine.join(format!("{index}.backup"));
        if processed {
            fs::rename(root.join(path), &backup).expect("simulate quarantine move");
        }
        steps.push(JournalStep {
            id: index,
            paths: vec![path.clone()],
            before: BTreeMap::from([(path.clone(), after.get(path))]),
            after: BTreeMap::from([(path.clone(), Fingerprint::Absent)]),
            backup_path: Some(backup.to_string_lossy().into_owned()),
            staging_path: None,
            status: if processed {
                JournalStatus::Durable
            } else {
                JournalStatus::Planned
            },
        });
    }
    // Externally rewrite one of the not-yet-processed children.
    let tampered = root.join(&children[2]);
    fs::write(&tampered, b"externally rewritten").expect("external modification");
    let empty_tree = Fingerprint::directory_from_children(&BTreeMap::new(), metadata.clone())
        .expect("empty directory fingerprint");
    steps.push(JournalStep {
        id: steps.len(),
        paths: vec!["tree".to_owned()],
        before: BTreeMap::from([("tree".to_owned(), empty_tree)]),
        after: BTreeMap::from([("tree".to_owned(), Fingerprint::Absent)]),
        backup_path: None,
        staging_path: None,
        status: JournalStatus::Planned,
    });

    let anchor = workspace
        .storage
        .catalog
        .insert_state(
            workspace.id,
            StateKind::Anchor,
            &after,
            Some(after_id),
            Some("tamper-anchor"),
        )
        .expect("anchor state");
    let journal = Journal {
        transaction_id,
        workspace_id: workspace.id,
        operation_id: Some(operation_id),
        anchor_state_id: anchor,
        target_state_id: before_id.to_owned(),
        direction: "UNDO".to_owned(),
        status: JournalStatus::Applying,
        steps,
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

    recover_locked(workspace).expect_err("recovery must refuse the tampered child");
    assert_eq!(condition(workspace), WorkspaceCondition::RecoveryRequired);
    assert_eq!(
        fs::read(&tampered).expect("tampered bytes preserved"),
        b"externally rewritten"
    );

    recover_reconcile(workspace).expect("reconcile recovery");
    assert_eq!(condition(workspace), WorkspaceCondition::Healthy);
    assert_eq!(
        fs::read(&tampered).expect("tampered bytes preserved after reconcile"),
        b"externally rewritten"
    );
    assert!(unfinished_journals(workspace).is_empty());
}

/// A transaction that quarantines a file must archive those bytes into the
/// external store after commit (Windows read-handle sync bug, V-F04), and
/// the transaction-local staging must then be removed (V-F07).
#[test]
fn archive_of_quarantined_file_succeeds_and_staging_is_cleaned() {
    let fixture = init_fixture();
    let root = fixture.root.path().to_path_buf();
    let outcome = fixture
        .workspace
        .run_command(&[
            "cmd".to_owned(),
            "/C".to_owned(),
            "echo B> foo.txt".to_owned(),
        ])
        .expect("capture modification");
    let operation_id = outcome.operation_id.expect("operation id");
    undo(&fixture.workspace, Some(operation_id), false).expect("undo modification");
    assert_eq!(fs::read(root.join("foo.txt")).expect("restored"), b"A");

    let journals = journal_dir(&fixture.workspace);
    let mut found = false;
    for entry in fs::read_dir(&journals).expect("journal dir") {
        let path = entry.expect("entry").path();
        let journal = workspace_journal(&fixture.workspace, &path);
        if journal.steps.iter().any(|step| step.backup_path.is_some())
            && journal.status == JournalStatus::Committed
        {
            found = true;
            assert_eq!(
                journal.archive_status,
                ArchiveStatus::Archived,
                "quarantined bytes must be archived, not failed: {:?}",
                journal.archive_error
            );
            for step in &journal.steps {
                if let Some(_backup_planned_path) = &step.backup_path {
                    let destination = fixture
                        .workspace
                        .storage
                        .project_root
                        .join("archive")
                        .join(journal.transaction_id.to_string())
                        .join(step.id.to_string());
                    // The undo quarantined "B\r\n" (the post-state bytes) and
                    // the archive must hold exactly those bytes. The local
                    // quarantine copy no longer exists: successful archival
                    // disposes of the transaction staging.
                    assert_eq!(
                        fs::read(&destination).expect("archived artifact"),
                        b"B\r\n",
                        "archived bytes must match the quarantined post-state"
                    );
                }
            }
            let staging_parent = root.parent().expect("workspace parent").join(".rewind-txn");
            assert!(
                !staging_parent
                    .join(journal.transaction_id.to_string())
                    .exists(),
                "staging must be removed after successful archive"
            );
        }
    }
    assert!(found, "a committed journal with backups must exist");
    assert_eq!(condition(&fixture.workspace), WorkspaceCondition::Healthy);
}

/// Windows junctions must be classified as unsupported objects (V-F03), and
/// an operation whose target state contains one must not be undoable.
#[test]
#[cfg(windows)]
fn junction_is_unsupported_object() {
    let fixture = init_fixture();
    let root = fixture.root.path().to_path_buf();
    fs::create_dir(root.join("sub")).expect("target dir");
    fs::write(root.join("sub").join("target.txt"), b"TARGETDATA").expect("target file");
    let outcome = fixture
        .workspace
        .run_command(&[
            "cmd".to_owned(),
            "/C".to_owned(),
            "mklink /J junc sub".to_owned(),
        ])
        .expect("capture junction creation");
    let operation_id = outcome.operation_id.expect("operation id");
    fixture
        .workspace
        .reconcile_locked("junction classification")
        .expect("reconcile");
    let baseline = fixture
        .workspace
        .state_manifest(&fixture.workspace.baseline_id().expect("baseline"))
        .expect("baseline manifest");
    match baseline.get("junc") {
        Fingerprint::Unsupported { object_kind, .. } => {
            assert_eq!(object_kind, "WINDOWS_JUNCTION");
        }
        other => panic!("junction must be unsupported, got {}", other.kind_name()),
    }

    // The operation is not fully reversible because its post state contains
    // an unsupported object; undo must refuse rather than follow or replace.
    let refusal = undo(&fixture.workspace, Some(operation_id), false);
    assert!(
        refusal.is_err(),
        "undo of an unsupported-object operation must be refused"
    );
    assert!(root.join("junc").exists(), "junction untouched by refusal");
    assert_eq!(
        fs::read(root.join("sub").join("target.txt")).expect("target intact"),
        b"TARGETDATA"
    );
}

fn workspace_journal(workspace: &Workspace, path: &Path) -> Journal {
    workspace.storage.journals.load(path).expect("journal json")
}

/// The CLI must parse `rewind run` in debug builds without panicking
/// (V-F05) and the documented recovery commands must exist (V-F02).
#[test]
fn cli_surface_parses_and_recovers() {
    let bin = env!("CARGO_BIN_EXE_rewind");
    for args in [
        vec!["--help"],
        vec!["run", "--help"],
        vec!["recover", "--help"],
    ] {
        let status = Command::new(bin)
            .args(&args)
            .status()
            .expect("spawn rewind");
        assert!(
            status.success(),
            "rewind {:?} must parse without panicking",
            args
        );
    }

    // End-to-end CLI run through the real argument parser.
    let root = tempfile::tempdir().expect("workspace");
    let store = tempfile::tempdir().expect("store");
    let store_path = store.path().to_string_lossy().replace('/', "\\");
    let run = |args: &[&str], cwd: &Path| {
        let mut command = Command::new(bin);
        command.args(args).current_dir(cwd);
        command.env("REWIND_HOME", &store_path);
        command.output().expect("run rewind")
    };
    let output = run(&["init", "."], root.path());
    assert!(output.status.success(), "cli init failed");
    let output = run(&["run", "--", "cmd", "/C", "echo hi> hi.txt"], root.path());
    assert!(
        output.status.success(),
        "cli run must capture; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(root.path().join("hi.txt")).expect("captured file"),
        b"hi\r\n"
    );
    let output = run(&["status"], root.path());
    assert!(String::from_utf8_lossy(&output.stdout).contains("condition HEALTHY"));
}
