//! Rollback performance phase: the path-scoped observation primitive
//! (`.ai/PHASE_ROLLBACK_PERF.md` acceptance criteria).
//!
//! The optimization's core correctness claim is that `scan_fingerprint_at`
//! produces **exactly** the fingerprint a full scan records for the same
//! path — for every object type, including directories (recursive manifest
//! hash), missing paths, and unsupported objects. These tests compare the
//! two observation paths directly against one real workspace.
//!
//! A second claim is adversarial: the per-step narrowing must not weaken the
//! conflict and final-verification semantics. External divergence is still
//! refused before mutation, and a workspace left inconsistent still lands in
//! `RecoveryRequired`.

use std::fs;

use rewind::model::{Fingerprint, WorkspaceCondition};
use rewind::rollback::undo;
use rewind::workspace::Workspace;

fn fixture() -> (tempfile::TempDir, tempfile::TempDir, Workspace) {
    let root = tempfile::tempdir().expect("temporary root");
    fs::write(root.path().join("file.txt"), b"CONTENT").expect("seed file");
    fs::create_dir(root.path().join("nested")).expect("seed dir");
    fs::write(root.path().join("nested").join("inner.txt"), b"INNER").expect("seed inner");
    fs::create_dir_all(root.path().join("deep").join("deeper")).expect("seed deep");
    fs::write(
        root.path().join("deep").join("deeper").join("leaf"),
        b"LEAF",
    )
    .expect("seed leaf");
    let store = tempfile::tempdir().expect("temporary store");
    let workspace = Workspace::init(root.path(), Some(store.path())).expect("initialize");
    (root, store, workspace)
}

/// A platform-native captured modification, mirroring the foundation suite.
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

/// Every fingerprint the path-scoped scan produces must equal the full
/// scan's manifest entry for the same path — files, nested and deep
/// directories (manifest hashes over child maps), absent paths, and, on
/// Unix, symlinks and named pipes. This is the equivalence the rollback
/// step verification now relies on.
#[test]
fn path_scoped_fingerprints_match_the_full_scan() {
    let (root, _store, workspace) = fixture();

    #[cfg(unix)]
    {
        // std::os::unix::fs::mkfifo is unstable (rust-lang/rust#139324):
        // tests create FIFOs through the platform's mkfifo utility.
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                "mkfifo -m 600 '{}'",
                root.path().join("pipe").display()
            ))
            .status()
            .expect("run mkfifo");
        assert!(status.success(), "mkfifo failed");
        std::os::unix::fs::symlink("file.txt", root.path().join("link.txt"))
            .expect("create symlink");
        let _listener = std::os::unix::net::UnixListener::bind(root.path().join("runtime.sock"))
            .expect("bind socket");
        workspace
            .reconcile_locked("special objects")
            .expect("reconcile");
    }

    let baseline_id = workspace.baseline_id().expect("baseline");
    let manifest = workspace
        .state_manifest(&baseline_id)
        .expect("baseline manifest");
    let mut paths: Vec<String> = manifest.entries.keys().cloned().collect();
    // Absent paths are part of the contract too, at several depths.
    paths.push("missing.txt".to_owned());
    paths.push("nested/missing.txt".to_owned());
    paths.push("deep/deeper/missing.txt".to_owned());
    for path in &paths {
        let full = manifest.get(path);
        let scoped = workspace.scan_path(path).expect("scoped scan");
        assert_eq!(
            full, scoped,
            "path-scoped fingerprint diverged from the full scan for {path}"
        );
    }

    // The root itself degenerates to the full root walk: a directory whose
    // children are exactly the manifest's top-level entries.
    let root_scoped = workspace.scan_path("").expect("root scoped scan");
    match &root_scoped {
        Fingerprint::Directory { entry_count, .. } => {
            let top_level = manifest
                .entries
                .keys()
                .filter(|key| !key.contains('/'))
                .count();
            assert_eq!(
                entry_count,
                &(top_level as u64),
                "the root's scoped walk must see exactly the manifest's top level"
            );
        }
        other => panic!(
            "root scoped scan must be a directory, got {}",
            other.kind_name()
        ),
    }

    // After a content change, the scoped scan observes the new fingerprint
    // (nothing is assumed unchanged between steps).
    fs::write(root.path().join("file.txt"), b"CHANGED").expect("modify");
    let changed = workspace.scan_path("file.txt").expect("scoped scan");
    match (&manifest.get("file.txt"), &changed) {
        (
            Fingerprint::RegularFile {
                content_hash: before,
                ..
            },
            Fingerprint::RegularFile {
                content_hash: after,
                ..
            },
        ) => assert_ne!(before, after, "the scoped scan must observe live content"),
        (before, after) => panic!("unexpected fingerprints: {before:?} vs {after:?}"),
    }
}

/// The per-step pre-check must still refuse undo before any mutation when
/// the live state diverges from the journal's expectation — the conflict
/// semantics are unchanged by the narrowing.
#[test]
fn external_divergence_still_refuses_undo_without_mutation() {
    let (root, _store, workspace) = fixture();
    let outcome = workspace
        .run_command(&run_echo("file.txt", "NEW"))
        .expect("capture modification");
    let operation_id = outcome.operation_id.expect("operation id");

    // External writer rewrites the file after capture: the journal's
    // expected before-state no longer matches the live filesystem.
    fs::write(root.path().join("file.txt"), b"EXTERNALLY REWRITTEN").expect("external write");

    let refusal = undo(&workspace, Some(operation_id), false);
    assert!(
        refusal.is_err(),
        "undo must refuse when the live pre-state diverges from the journal"
    );
    let content = fs::read_to_string(root.path().join("file.txt")).expect("content");
    assert_eq!(content, "EXTERNALLY REWRITTEN", "nothing may be mutated");
}

/// A step whose live state diverges before the operation starts must be
/// refused at plan time — cleanly, with no transaction created and the
/// workspace condition untouched. The narrowing must not have turned
/// plan-time conflicts into mid-apply failures; the apply-time
/// `RecoveryRequired` path remains covered by the interrupted-rollback
/// recovery tests.
#[test]
fn a_plan_time_conflict_refuses_cleanly_without_recovery() {
    let (root, _store, workspace) = fixture();
    fs::write(root.path().join("doomed.txt"), b"X").expect("seed");
    workspace
        .reconcile_locked("seed doomed file")
        .expect("reconcile");
    let outcome = workspace
        .run_command(&run_echo("doomed.txt", "Y"))
        .expect("capture modification");
    let operation_id = outcome.operation_id.expect("operation id");
    fs::remove_file(root.path().join("doomed.txt")).expect("external deletion pre-undo");

    let result = undo(&workspace, Some(operation_id), false);
    assert!(
        result.is_err(),
        "the plan cannot restore its expected before state"
    );
    let condition = workspace.row().expect("row").condition;
    assert_eq!(
        condition,
        WorkspaceCondition::Healthy,
        "a plan-time conflict refuses before any transaction exists"
    );
    assert!(
        !root.path().join("doomed.txt").exists(),
        "the refusal must not have mutated anything"
    );
}
