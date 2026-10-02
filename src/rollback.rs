use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::fs::OpenOptions;
use std::path::Path;

use uuid::Uuid;

use crate::error::{Result, RewindError};
use crate::model::{
    ArchiveStatus, Fingerprint, Journal, JournalStatus, JournalStep, OperationKind,
    OperationStatus, Reversibility, StateKind, TrackingConfidence, WorkspaceCondition,
};
use crate::paths::{ensure_parent_confinement, staging_root, workspace_path};
use crate::workspace::{Workspace, WorkspaceLease};

#[derive(Clone, Debug)]
pub struct RollbackOutcome {
    pub transaction_id: Uuid,
    pub operation_id: Option<i64>,
    pub target_state_id: String,
}

pub fn undo(
    workspace: &Workspace,
    operation_id: Option<i64>,
    force: bool,
) -> Result<RollbackOutcome> {
    let _lease = WorkspaceLease::acquire(workspace, false)?;
    recover_locked(workspace)?;
    workspace.enforce_pending_safety_gate()?;
    require_healthy(workspace)?;
    let operation = match operation_id {
        Some(id) => workspace.storage.catalog.operation(id, workspace.id)?,
        None => workspace.storage.catalog.latest_undoable(workspace.id)?,
    };
    if !matches!(operation.status, OperationStatus::Completed)
        || !matches!(operation.reversibility, Reversibility::FullyReversible)
    {
        return Err(RewindError::ConditionBlocked(format!(
            "operation {} is not eligible for undo",
            operation.id
        )));
    }
    let current_scan = workspace.scan(None)?;
    let expected = operation
        .post_state_id
        .as_deref()
        .ok_or_else(|| RewindError::NotFound("operation post-state".to_owned()))?;
    if current_scan.state_id != expected && !force {
        return Err(RewindError::Conflict {
            path: "<workspace>".to_owned(),
            expected: expected.to_owned(),
            found: current_scan.state_id,
        });
    }
    let actual_before = current_scan.manifest;
    let target_id = operation
        .pre_state_id
        .as_deref()
        .ok_or_else(|| RewindError::NotFound("operation pre-state".to_owned()))?;
    let target = workspace.state_manifest(target_id)?;
    execute_transition(
        workspace,
        Some(operation.id),
        "UNDO",
        &actual_before,
        &target,
        target_id,
        force,
        OperationStatus::Undone,
    )
}

pub fn redo(workspace: &Workspace, operation_id: Option<i64>) -> Result<RollbackOutcome> {
    let _lease = WorkspaceLease::acquire(workspace, false)?;
    recover_locked(workspace)?;
    workspace.enforce_pending_safety_gate()?;
    require_healthy(workspace)?;
    let operation = match operation_id {
        Some(id) => workspace.storage.catalog.operation(id, workspace.id)?,
        None => workspace.storage.catalog.latest_redoable(workspace.id)?,
    };
    if !matches!(operation.status, OperationStatus::Undone)
        || !matches!(operation.reversibility, Reversibility::FullyReversible)
    {
        return Err(RewindError::ConditionBlocked(format!(
            "operation {} is not eligible for redo",
            operation.id
        )));
    }
    let current_scan = workspace.scan(None)?;
    let expected = operation
        .pre_state_id
        .as_deref()
        .ok_or_else(|| RewindError::NotFound("operation pre-state".to_owned()))?;
    if current_scan.state_id != expected {
        return Err(RewindError::Conflict {
            path: "<workspace>".to_owned(),
            expected: expected.to_owned(),
            found: current_scan.state_id,
        });
    }
    let target_id = operation
        .post_state_id
        .as_deref()
        .ok_or_else(|| RewindError::NotFound("operation post-state".to_owned()))?;
    let target = workspace.state_manifest(target_id)?;
    execute_transition(
        workspace,
        Some(operation.id),
        "REDO",
        &current_scan.manifest,
        &target,
        target_id,
        false,
        OperationStatus::Completed,
    )
}

pub fn restore_snapshot(workspace: &Workspace, name: &str) -> Result<RollbackOutcome> {
    let _lease = WorkspaceLease::acquire(workspace, false)?;
    recover_locked(workspace)?;
    workspace.enforce_pending_safety_gate()?;
    require_healthy(workspace)?;
    let current = workspace.scan(None)?;
    let current_id = current.state_id.clone();
    let target_id = workspace
        .storage
        .catalog
        .snapshot_state(workspace.id, name)?;
    let target = workspace.state_manifest(&target_id)?;
    let operation_id = workspace.storage.catalog.insert_operation(
        workspace.id,
        &crate::db::OperationDraft {
            kind: OperationKind::Restore,
            status: OperationStatus::Untrusted,
            pre_state_id: Some(current_id.clone()),
            post_state_id: Some(target_id.clone()),
            command: None,
            cwd: Some(workspace.root.to_string_lossy().into_owned()),
            exit_code: None,
            confidence: TrackingConfidence::Tracked,
            reversibility: Reversibility::FullyReversible,
            error: None,
            effects: crate::scan::diff_manifests(&current.manifest, &target),
            // Snapshot restores are not package-manager captures; Phase 6
            // evidence is derived only by strong command capture.
            evidence: Vec::new(),
        },
    )?;
    execute_transition(
        workspace,
        Some(operation_id),
        "RESTORE",
        &current.manifest,
        &target,
        &target_id,
        false,
        OperationStatus::Completed,
    )
}

fn require_healthy(workspace: &Workspace) -> Result<()> {
    match workspace.condition()? {
        WorkspaceCondition::Healthy => Ok(()),
        WorkspaceCondition::RecoveryRequired => Err(RewindError::ConditionBlocked(
            "RECOVERY_REQUIRED; run `rewind recover` to classify the unfinished \
             transaction, or `rewind recover --reconcile` to abandon it and \
             reconcile"
                .to_owned(),
        )),
        other => Err(RewindError::ConditionBlocked(format!(
            "{other}; run rewind reconcile before rollback"
        ))),
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_transition(
    workspace: &Workspace,
    operation_id: Option<i64>,
    direction: &str,
    actual_before: &crate::model::Manifest,
    target: &crate::model::Manifest,
    target_state_id: &str,
    force: bool,
    final_operation_status: OperationStatus,
) -> Result<RollbackOutcome> {
    let transaction_id = Uuid::new_v4();
    let anchor_id = create_anchor(workspace, actual_before)?;
    let staging = staging_root(&workspace.root, transaction_id)?;
    let quarantine = staging.join("quarantine");
    let prepared = staging.join("prepared");
    fs::create_dir_all(&quarantine)?;
    fs::create_dir_all(&prepared)?;
    let mut journal = build_journal(
        workspace,
        transaction_id,
        operation_id,
        anchor_id.clone(),
        target_state_id.to_owned(),
        direction.to_owned(),
        actual_before,
        target,
        &quarantine,
        &prepared,
    )?;
    workspace.storage.journals.write(&journal)?;
    workspace.storage.catalog.add_transaction(
        transaction_id,
        workspace.id,
        operation_id,
        &JournalStatus::Planned,
        &workspace
            .storage
            .journal_path(transaction_id)
            .to_string_lossy(),
    )?;
    journal.status = JournalStatus::Prepared;
    prepare_steps(workspace, &mut journal)?;
    workspace.storage.journals.write(&journal)?;
    workspace
        .storage
        .catalog
        .update_transaction(transaction_id, &JournalStatus::Prepared)?;

    for index in 0..journal.steps.len() {
        if let Err(error) = apply_step(workspace, &mut journal, index, force) {
            journal.status = JournalStatus::RecoveryRequired;
            workspace.storage.journals.write(&journal)?;
            workspace
                .storage
                .catalog
                .update_transaction(transaction_id, &JournalStatus::RecoveryRequired)?;
            workspace.storage.catalog.set_workspace(
                workspace.id,
                &WorkspaceCondition::RecoveryRequired,
                Some(&anchor_id),
            )?;
            return Err(error);
        }
    }
    let final_scan = match workspace.scan(None) {
        Ok(scan) => scan,
        Err(error) => {
            journal.status = JournalStatus::RecoveryRequired;
            workspace.storage.journals.write(&journal)?;
            workspace
                .storage
                .catalog
                .update_transaction(transaction_id, &JournalStatus::RecoveryRequired)?;
            workspace.storage.catalog.set_workspace(
                workspace.id,
                &WorkspaceCondition::RecoveryRequired,
                Some(&anchor_id),
            )?;
            return Err(error);
        }
    };
    if final_scan.state_id != target_state_id {
        journal.status = JournalStatus::RecoveryRequired;
        workspace.storage.journals.write(&journal)?;
        workspace
            .storage
            .catalog
            .update_transaction(transaction_id, &JournalStatus::RecoveryRequired)?;
        workspace.storage.catalog.set_workspace(
            workspace.id,
            &WorkspaceCondition::RecoveryRequired,
            Some(&anchor_id),
        )?;
        return Err(RewindError::RecoveryRequired(format!(
            "final state {} differs from target {}",
            final_scan.state_id, target_state_id
        )));
    }
    journal.status = JournalStatus::Committed;
    for step in &mut journal.steps {
        step.status = JournalStatus::Durable;
    }
    if journal.steps.iter().any(|step| step.backup_path.is_some()) {
        journal.archive_status = ArchiveStatus::Pending;
    }
    workspace.storage.journals.write(&journal)?;
    workspace
        .storage
        .catalog
        .update_transaction(transaction_id, &JournalStatus::Committed)?;
    workspace.storage.catalog.set_workspace(
        workspace.id,
        &WorkspaceCondition::Healthy,
        Some(target_state_id),
    )?;
    if let Some(operation_id) = operation_id {
        workspace
            .storage
            .catalog
            .set_operation_status(operation_id, final_operation_status)?;
    }
    archive_committed_transaction(workspace, &mut journal);
    dispose_of_staging(workspace, &journal);
    Ok(RollbackOutcome {
        transaction_id,
        operation_id,
        target_state_id: target_state_id.to_owned(),
    })
}

fn create_anchor(workspace: &Workspace, manifest: &crate::model::Manifest) -> Result<String> {
    for fingerprint in manifest.entries.values() {
        if let Fingerprint::RegularFile {
            content_hash,
            streams,
            ..
        } = fingerprint
        {
            workspace.storage.cas.verify(content_hash)?;
            // Stream content is CAS content: the anchor verifies every
            // stream hash too (Phase 5 slice 3).
            for stream_hash in streams.values() {
                workspace.storage.cas.verify(stream_hash)?;
            }
        }
    }
    workspace.storage.catalog.insert_state(
        workspace.id,
        StateKind::Anchor,
        manifest,
        Some(&workspace.baseline_id()?),
        Some("pre-rollback-anchor"),
    )
}

#[allow(clippy::too_many_arguments)]
fn build_journal(
    workspace: &Workspace,
    transaction_id: Uuid,
    operation_id: Option<i64>,
    anchor_id: String,
    target_state_id: String,
    direction: String,
    before: &crate::model::Manifest,
    after: &crate::model::Manifest,
    quarantine: &Path,
    prepared: &Path,
) -> Result<Journal> {
    let mut paths = BTreeSet::new();
    paths.extend(before.entries.keys().cloned());
    paths.extend(after.entries.keys().cloned());
    let mut removals = Vec::new();
    let mut installations = Vec::new();
    for path in paths {
        if before.get(&path) == after.get(&path) {
            continue;
        }
        if matches!(after.get(&path), Fingerprint::Absent) {
            removals.push(path);
        } else {
            installations.push(path);
        }
    }
    removals.sort_by_key(|path| (std::cmp::Reverse(path.matches('/').count()), path.clone()));
    installations.sort_by_key(|path| {
        (
            if matches!(after.get(path), Fingerprint::Directory { .. }) {
                0
            } else {
                1
            },
            path.matches('/').count(),
            path.clone(),
        )
    });
    removals.extend(installations);
    let ordered = removals;
    let mut steps = Vec::new();
    for (index, path) in ordered.into_iter().enumerate() {
        let pre = before.get(&path);
        let post = after.get(&path);
        if pre == post {
            continue;
        }
        if !post.is_supported_for_restore() {
            return Err(RewindError::Unsupported(format!(
                "cannot restore {path} as {}",
                post.kind_name()
            )));
        }
        // The step's before-map must describe the state the filesystem is
        // expected to be in when THIS step executes, not the state captured
        // before the transaction began. Earlier steps of the same transaction
        // legitimately remove descendants of this path, so a directory's
        // expected fingerprint is narrowed to the children that survive in
        // the target state. Comparing against the original snapshot here
        // would misclassify the transaction's own work as an external
        // conflict (V-F01), while any child outside this narrowed map still
        // trips the conflict gate as a genuine external modification.
        let effective_pre = effective_expected_before(before, after, &path, &pre)?;
        let before_map = BTreeMap::from([(path.clone(), effective_pre)]);
        let after_map = BTreeMap::from([(path.clone(), post.clone())]);
        // A backup path is planned only for steps that will actually move the
        // live object into quarantine. Directory-to-directory steps keep the
        // directory in place (children change through their own steps), so
        // they never produce a backup artifact; planning one would make the
        // post-commit archive report a missing artifact forever.
        let preserves_directory = matches!(pre, Fingerprint::Directory { .. })
            && matches!(post, Fingerprint::Directory { .. });
        let backup_path = if !matches!(pre, Fingerprint::Absent) && !preserves_directory {
            Some(
                quarantine
                    .join(format!("{index}.backup"))
                    .to_string_lossy()
                    .into_owned(),
            )
        } else {
            None
        };
        let staging_path = if let Fingerprint::RegularFile { .. } = post {
            Some(
                prepared
                    .join(format!("{index}.file"))
                    .to_string_lossy()
                    .into_owned(),
            )
        } else {
            None
        };
        steps.push(JournalStep {
            id: index,
            paths: vec![path],
            before: before_map,
            after: after_map,
            backup_path,
            staging_path,
            status: JournalStatus::Planned,
        });
    }
    let archive_status = if steps.iter().any(|step| step.backup_path.is_some()) {
        ArchiveStatus::Pending
    } else {
        ArchiveStatus::NotRequired
    };
    Ok(Journal {
        transaction_id,
        workspace_id: workspace.id,
        operation_id,
        anchor_state_id: anchor_id,
        target_state_id,
        direction,
        status: JournalStatus::Planned,
        steps,
        archive_status,
        archive_error: None,
    })
}

/// Derives the state a path is expected to be in when its own step executes,
/// given the plan of the whole transaction. Only directory expectations can
/// differ from the pre-transaction snapshot: all removal steps run before
/// installation steps and deeper paths run before shallower removals, so by
/// the time a directory's step runs, exactly those descendants that are
/// absent from the target state have been removed by earlier steps. Every
/// other object type is untouched by prior steps.
fn effective_expected_before(
    before: &crate::model::Manifest,
    after: &crate::model::Manifest,
    path: &str,
    pre: &Fingerprint,
) -> Result<Fingerprint> {
    let Fingerprint::Directory { metadata, .. } = pre else {
        return Ok(pre.clone());
    };
    let prefix = format!("{path}/");
    let mut children = BTreeMap::<String, Fingerprint>::new();
    for (entry_path, entry_fingerprint) in &before.entries {
        let Some(rest) = entry_path.strip_prefix(&prefix) else {
            continue;
        };
        if rest.contains('/') {
            continue;
        }
        if matches!(after.get(entry_path), Fingerprint::Absent) {
            continue;
        }
        children.insert(rest.to_owned(), entry_fingerprint.clone());
    }
    Fingerprint::directory_from_children(&children, metadata.clone())
}

fn prepare_steps(workspace: &Workspace, journal: &mut Journal) -> Result<()> {
    for step in &mut journal.steps {
        let Some(path) = step.paths.first() else {
            continue;
        };
        let desired = step.after.get(path).ok_or_else(|| {
            RewindError::Journal(format!("step {} has no destination state", step.id))
        })?;
        if let Fingerprint::RegularFile { content_hash, .. } = desired {
            let stage = step.staging_path.as_ref().ok_or_else(|| {
                RewindError::Journal("regular file has no staging path".to_owned())
            })?;
            workspace
                .storage
                .cas
                .materialize(content_hash, Path::new(stage))?;
        }
        if let Some(backup) = &step.backup_path {
            if let Some(parent) = Path::new(backup).parent() {
                fs::create_dir_all(parent)?;
            }
        }
    }
    Ok(())
}

fn apply_step(
    workspace: &Workspace,
    journal: &mut Journal,
    index: usize,
    force: bool,
) -> Result<()> {
    let before_step = journal
        .steps
        .get(index)
        .cloned()
        .ok_or_else(|| RewindError::Journal(format!("missing step {index}")))?;
    let path = before_step
        .paths
        .first()
        .ok_or_else(|| RewindError::Journal(format!("step {index} has no path")))?;
    let expected_before = before_step
        .before
        .get(path)
        .ok_or_else(|| RewindError::Journal(format!("step {index} has no before state")))?;
    let desired = before_step
        .after
        .get(path)
        .ok_or_else(|| RewindError::Journal(format!("step {index} has no after state")))?;
    // Per-step observation is path-scoped by contract (ADR-018): the full
    // scan's only consumed value here was `manifest.get(path)`, and global
    // correctness is anchored by the final full-scan state comparison.
    let actual = workspace.scan_path(path)?;
    if actual == *desired && backup_is_sufficient(&before_step, expected_before) {
        journal.steps[index].status = JournalStatus::Durable;
        workspace.storage.journals.write(journal)?;
        return Ok(());
    }
    if actual != *expected_before && !force {
        journal.status = JournalStatus::RecoveryRequired;
        journal.steps[index].status = JournalStatus::RecoveryRequired;
        workspace.storage.journals.write(journal)?;
        return Err(RewindError::Conflict {
            path: path.clone(),
            expected: expected_before.describe(),
            found: actual.describe(),
        });
    }
    journal.status = JournalStatus::Applying;
    journal.steps[index].status = JournalStatus::Applying;
    workspace.storage.journals.write(journal)?;
    let target_path = workspace_path(&workspace.root, path)?;
    ensure_parent_confinement(&workspace.root, &target_path)?;
    let preserve_directory = matches!(expected_before, Fingerprint::Directory { .. })
        && matches!(desired, Fingerprint::Directory { .. });
    if !matches!(actual, Fingerprint::Absent) && !preserve_directory {
        let backup = before_step
            .backup_path
            .as_ref()
            .ok_or_else(|| RewindError::Journal("existing source has no backup path".to_owned()))?;
        if fs::symlink_metadata(backup).is_err() {
            if let Some(parent) = Path::new(backup).parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(&target_path, backup).map_err(|error| {
                RewindError::Storage(format!(
                    "quarantine {} -> {}: {error}",
                    target_path.display(),
                    backup
                ))
            })?;
            // Post-mutation confinement: the source path we just moved must
            // still sit under real parent components (Fix #3 TOCTOU).
            crate::paths::verify_mutation_confined(&workspace.root, &target_path, false)?;
        }
        journal.steps[index].backup_path = Some(backup.clone());
        workspace.storage.journals.write(journal)?;
    }
    match desired {
        Fingerprint::Absent => {}
        Fingerprint::RegularFile {
            streams,
            dacl: desired_dacl,
            ..
        } => {
            let stage = before_step
                .staging_path
                .as_ref()
                .ok_or_else(|| RewindError::Journal("regular file has no stage".to_owned()))?;
            if !target_path.exists() {
                ensure_parent_directories(&workspace.root, &target_path)?;
            }
            fs::rename(stage, &target_path).map_err(|error| {
                RewindError::Storage(format!(
                    "install staged file {} -> {}: {error}",
                    stage,
                    target_path.display()
                ))
            })?;
            // The leaf is verified a real regular file BEFORE any stream
            // write: a stream write on a reparse-point path follows it out
            // of the workspace (probe-verified). Streams are written before
            // the recorded readonly attribute is applied, because a stream
            // write on a readonly file is denied (probe-verified both
            // directions).
            crate::paths::verify_mutation_confined(&workspace.root, &target_path, true)?;
            #[cfg(windows)]
            {
                if !streams.is_empty() {
                    install_named_streams(&workspace.storage.cas, &target_path, streams)?;
                }
            }
            #[cfg(not(windows))]
            {
                if !streams.is_empty() {
                    return Err(RewindError::Unsupported(format!(
                        "named streams cannot be restored on this platform: {path}"
                    )));
                }
            }
            apply_metadata(&target_path, desired)?;
            // The recorded explicit DACL is applied last: probe-verified to
            // work on readonly files (no ordering constraint, unlike stream
            // writes), and the freshly installed file inherits only, so the
            // recorded explicit set fully replaces whatever the parent
            // granted. SIDs are restored literally from their recorded
            // string form — no account resolution, no existence checks.
            #[cfg(windows)]
            {
                if let Some(dacl) = &desired_dacl {
                    install_dacl(&target_path, dacl)?;
                }
            }
            #[cfg(not(windows))]
            {
                if desired_dacl.is_some() {
                    return Err(RewindError::Unsupported(format!(
                        "explicit NTFS DACLs cannot be restored on this platform: {path}"
                    )));
                }
            }
        }
        Fingerprint::Directory { .. } => {
            ensure_parent_directories(&workspace.root, &target_path)?;
            if !target_path.exists() {
                fs::create_dir(&target_path)?;
            }
            apply_metadata(&target_path, desired)?;
            crate::paths::verify_mutation_confined(&workspace.root, &target_path, true)?;
        }
        Fingerprint::Symlink {
            target,
            target_kind,
            ..
        } => {
            ensure_parent_directories(&workspace.root, &target_path)?;
            create_symlink(target, *target_kind, &target_path)?;
            // The installed leaf is legitimately a symlink here; verify the
            // literal target matches the recorded bytes so a swapped
            // parent chain is detected rather than followed.
            let installed = fs::read_link(&target_path).map_err(|error| {
                RewindError::Storage(format!(
                    "installed symlink unreadable {}: {error}",
                    target_path.display()
                ))
            })?;
            if installed.to_string_lossy() != target.as_str() {
                return Err(RewindError::PathEscape(format!(
                    "installed symlink target diverged at {}",
                    target_path.display()
                )));
            }
        }
        Fingerprint::NamedPipe { .. } => {
            // A FIFO is a kernel object with no content: create it with a
            // private mode first (no window where a manifest-unspecified
            // world-accessible pipe exists), then apply the recorded
            // authoritative mode. Reading or writing the pipe is never
            // attempted. A platform that cannot create FIFOs refuses before
            // touching anything.
            #[cfg(not(unix))]
            {
                return Err(RewindError::Unsupported(format!(
                    "named pipes cannot be restored on this platform: {path}"
                )));
            }
            #[cfg(unix)]
            {
                ensure_parent_directories(&workspace.root, &target_path)?;
                create_named_pipe(&target_path, 0o600)?;
                apply_metadata(&target_path, desired)?;
                crate::paths::verify_mutation_confined(&workspace.root, &target_path, true)?;
            }
        }
        Fingerprint::Junction {
            substitute,
            print_name,
            metadata: recorded_metadata,
            ..
        } => {
            // A junction is a content-free directory entry whose entire
            // state is its reparse data plus its own attributes (Phase 5
            // slice 2). Restoration recreates a plain directory, applies the
            // recorded attributes while the path is still plain (attribute
            // APIs would follow a junction), then writes the recorded
            // reparse data and reads it back. The target is never resolved,
            // checked for existence, or created. A platform that cannot
            // create junctions refuses before touching anything.
            #[cfg(not(windows))]
            {
                let _ = (substitute, print_name, recorded_metadata);
                return Err(RewindError::Unsupported(format!(
                    "junctions cannot be restored on this platform: {path}"
                )));
            }
            #[cfg(windows)]
            {
                ensure_parent_directories(&workspace.root, &target_path)?;
                create_junction(&target_path, substitute, print_name, recorded_metadata)?;
                // Post-install verification mirrors the symlink arm: a
                // junction IS a reparse point, so the generic
                // mutation-confinement leaf check cannot apply; instead the
                // installed reparse data is read back and must equal the
                // record. The final path-scoped re-scan then compares the
                // complete fingerprint (names and attributes).
                let installed =
                    crate::scan::reparse_junction_names(&target_path).ok_or_else(|| {
                        RewindError::PathEscape(format!(
                            "installed junction reparse data unreadable at {}",
                            target_path.display()
                        ))
                    })?;
                if (&installed.0, &installed.1) != (substitute, print_name) {
                    return Err(RewindError::PathEscape(format!(
                        "installed junction reparse data diverged at {}",
                        target_path.display()
                    )));
                }
            }
        }
        Fingerprint::Unsupported { .. } => {
            return Err(RewindError::Unsupported(format!(
                "cannot install unsupported object at {path}"
            )));
        }
    }
    if matches!(desired, Fingerprint::Absent) {
        sync_parent(&target_path)?;
    } else {
        sync_target(&target_path)?;
    }
    journal.steps[index].status = JournalStatus::Applied;
    workspace.storage.journals.write(journal)?;
    // Same per-path contract: verify exactly what this step was supposed to
    // reach. Anything else in the workspace is judged by the final full
    // scan's state comparison.
    let actual_after = workspace.scan_path(path)?;
    if !compatible_after(&actual_after, desired) {
        journal.status = JournalStatus::RecoveryRequired;
        journal.steps[index].status = JournalStatus::RecoveryRequired;
        workspace.storage.journals.write(journal)?;
        return Err(RewindError::RecoveryRequired(format!(
            "step {} did not reach expected after state at {}: expected {:?}, found {:?}",
            index, path, desired, actual_after
        )));
    }
    journal.steps[index].status = JournalStatus::Durable;
    journal.status = JournalStatus::Durable;
    workspace.storage.journals.write(journal)?;
    Ok(())
}

fn backup_is_sufficient(step: &JournalStep, before: &Fingerprint) -> bool {
    (matches!(before, Fingerprint::Absent)
        || matches!(
            (before, step.after.values().next()),
            (
                Fingerprint::Directory { .. },
                Some(Fingerprint::Directory { .. })
            )
        ))
        || step
            .backup_path
            .as_ref()
            .is_some_and(|path| fs::symlink_metadata(path).is_ok())
}

fn backup_matches_fingerprint(step: &JournalStep, expected: &Fingerprint) -> bool {
    let Some(path) = step.backup_path.as_deref() else {
        return matches!(expected, Fingerprint::Absent);
    };
    artifact_matches_fingerprint(Path::new(path), expected)
}

fn desired_artifact_is_ready(step: &JournalStep, desired: &Fingerprint) -> bool {
    match desired {
        Fingerprint::RegularFile { .. } => step
            .staging_path
            .as_deref()
            .is_some_and(|path| fs::symlink_metadata(path).is_ok()),
        Fingerprint::Directory { .. } | Fingerprint::Symlink { .. } => true,
        // A FIFO has no staged content: existence is the whole state.
        Fingerprint::NamedPipe { .. } => true,
        // A junction has no staged content either: the recorded reparse
        // data is the whole state.
        Fingerprint::Junction { .. } => true,
        Fingerprint::Absent | Fingerprint::Unsupported { .. } => false,
    }
}

fn artifact_matches_fingerprint(path: &Path, expected: &Fingerprint) -> bool {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return matches!(expected, Fingerprint::Absent),
    };
    match expected {
        Fingerprint::Absent => false,
        Fingerprint::RegularFile {
            content_hash,
            size,
            streams: expected_streams,
            dacl: expected_dacl,
            ..
        } => {
            if !metadata.is_file() || metadata.len() != *size {
                return false;
            }
            let Ok(mut file) = fs::File::open(path) else {
                return false;
            };
            let mut hasher = blake3::Hasher::new();
            if std::io::copy(&mut file, &mut hasher).is_err() {
                return false;
            }
            if hasher.finalize().to_hex().as_str() != content_hash {
                return false;
            }
            // Named streams are part of the recorded state: the quarantined
            // object must still carry exactly the recorded stream set, each
            // with the recorded bytes (Phase 5 slice 3). Verifying never
            // resolves a reparse point — quarantined backups are regular
            // files the quarantine rename carried in full.
            #[cfg(windows)]
            {
                let live = match crate::scan::named_streams(path) {
                    Ok(live) => live,
                    Err(_) => return false,
                };
                if live.len() != expected_streams.len() {
                    return false;
                }
                for (name, expected_hash) in expected_streams {
                    if !live.iter().any(|(live_name, _)| live_name == name) {
                        return false;
                    }
                    let spec = crate::scan::stream_spec(path, name);
                    let Ok(mut stream) = fs::File::open(&spec) else {
                        return false;
                    };
                    let mut hasher = blake3::Hasher::new();
                    if std::io::copy(&mut stream, &mut hasher).is_err() {
                        return false;
                    }
                    if hasher.finalize().to_hex().as_str() != expected_hash {
                        return false;
                    }
                }
            }
            #[cfg(not(windows))]
            {
                // A stream set can never exist off Windows; a foreign
                // fingerprint claiming one is not matchable here.
                if !expected_streams.is_empty() {
                    return false;
                }
            }
            // The explicit DACL is part of the recorded state: the
            // quarantined object must carry exactly the recorded explicit
            // ACEs and protected flag (Phase 5 slice 4). An unreadable
            // descriptor is a mismatch — never an assumed match. Verifying
            // never resolves a reparse point.
            #[cfg(windows)]
            {
                let live_dacl = match crate::scan::explicit_dacl(path) {
                    Ok(live) => live,
                    Err(_) => return false,
                };
                if live_dacl != *expected_dacl {
                    return false;
                }
            }
            #[cfg(not(windows))]
            {
                // A DACL record can never exist off Windows; a foreign
                // fingerprint claiming one is not matchable here.
                if expected_dacl.is_some() {
                    return false;
                }
            }
            true
        }
        Fingerprint::Directory { .. } => metadata.is_dir(),
        Fingerprint::Symlink { target, .. } => {
            metadata.file_type().is_symlink()
                && fs::read_link(path)
                    .map(|actual| actual.to_string_lossy() == target.as_str())
                    .unwrap_or(false)
        }
        Fingerprint::NamedPipe {
            metadata: expected_metadata,
        } => {
            // The quarantined object must still be a FIFO with the recorded
            // mode; verifying never opens the pipe.
            #[cfg(unix)]
            {
                use std::os::unix::fs::{FileTypeExt, PermissionsExt};
                if !metadata.file_type().is_fifo() {
                    return false;
                }
                match expected_metadata.mode {
                    Some(mode) => metadata.permissions().mode() & 0o777 == mode,
                    None => true,
                }
            }
            #[cfg(not(unix))]
            {
                let _ = expected_metadata;
                false
            }
        }
        Fingerprint::Junction {
            substitute,
            print_name,
            metadata: expected_metadata,
            ..
        } => {
            // The quarantined object must still be a junction whose reparse
            // data and recorded attributes match the record; verifying
            // never resolves the target.
            #[cfg(windows)]
            {
                metadata.file_type().is_symlink()
                    && crate::scan::reparse_junction_names(path)
                        == Some((substitute.clone(), print_name.clone()))
                    && metadata.permissions().readonly() == expected_metadata.readonly
            }
            #[cfg(not(windows))]
            {
                let _ = (substitute, print_name, expected_metadata);
                // A junction cannot exist off-Windows; a foreign journal
                // naming one is not matchable against this filesystem.
                false
            }
        }
        Fingerprint::Unsupported { .. } => false,
    }
}

fn compatible_after(actual: &Fingerprint, desired: &Fingerprint) -> bool {
    match (actual, desired) {
        (Fingerprint::Directory { .. }, Fingerprint::Directory { .. }) => true,
        _ => actual == desired,
    }
}

fn ensure_parent_directories(root: &Path, target: &Path) -> Result<()> {
    ensure_parent_confinement(root, target)?;
    let parent = target
        .parent()
        .ok_or_else(|| RewindError::PathEscape(target.display().to_string()))?;
    fs::create_dir_all(parent)?;
    Ok(())
}

/// `mode_t` under the POSIX ABIs Rewind's CI covers: `unsigned short` on
/// macOS (`__darwin_mode_t`), `unsigned int` on Linux (glibc and musl).
/// The FIFO modes passed here never exceed 0o7777, so both representations
/// hold the value exactly.
#[cfg(unix)]
#[cfg(target_os = "macos")]
type ModeT = std::os::raw::c_ushort;
#[cfg(unix)]
#[cfg(not(target_os = "macos"))]
type ModeT = std::os::raw::c_uint;

#[cfg(unix)]
extern "C" {
    fn mkfifo(path: *const std::os::raw::c_char, mode: ModeT) -> std::os::raw::c_int;
}

/// Creates a FIFO. `std::os::unix::fs::mkfifo` is unstable
/// (rust-lang/rust#139324), so this declares the POSIX `mkfifo(2)` call
/// directly — the crate's third minimal FFI site, alongside `reparse_tag`
/// (`src/scan.rs`) and `CreateProcessW` (`src/watch/detach_windows.rs`).
/// The requested mode is later superseded by the recorded authoritative
/// mode via `chmod` (`apply_metadata`), which is not umask-affected; the
/// umask can only narrow the initial private mode, never widen it.
#[cfg(unix)]
fn create_named_pipe(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        RewindError::Storage(format!("path contains an interior NUL: {}", path.display()))
    })?;
    let mode_repr = ModeT::try_from(mode)
        .map_err(|_| RewindError::Storage(format!("mode {mode:o} out of range")))?;
    // SAFETY: `c_path` is a NUL-terminated buffer owned for the duration of
    // the call, and `mkfifo`'s only effect is creating the FIFO named by it;
    // the result is checked and errno is read immediately.
    let result = unsafe { mkfifo(c_path.as_ptr(), mode_repr) };
    if result != 0 {
        return Err(RewindError::Storage(format!(
            "create named pipe {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

/// Writes a regular file's named streams from verified CAS objects (Phase 5
/// slice 3). Only ever called on a leaf `verify_mutation_confined` has just
/// confirmed as a real regular file: writing a stream spec on a reparse
/// point follows it out of the workspace. The write is direct — no
/// temp+rename — because a stream-spec temp path is itself a stream spec;
/// durability rides the journal step boundary exactly like the FIFO's mode
/// application. Content is verified, then copied in bounded chunks: a
/// stream blob is never loaded whole into memory, matching the staged
/// protocol used for the default stream.
#[cfg(windows)]
fn install_named_streams(
    cas: &crate::cas::Cas,
    target: &Path,
    streams: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    for (name, hash) in streams {
        // Verify the CAS object first: a missing or corrupt stream blob
        // fails here, before the filesystem is touched at all.
        cas.verify(hash)?;
        let mut blob = fs::File::open(cas.blob_path(hash)?)
            .map_err(|error| RewindError::Cas(format!("open stream blob {hash}: {error}")))?;
        let spec = crate::scan::stream_spec(target, name);
        let mut stream = fs::File::create(&spec).map_err(|error| {
            RewindError::Storage(format!(
                "create stream {name} on {}: {error}",
                target.display()
            ))
        })?;
        std::io::copy(&mut blob, &mut stream).map_err(|error| {
            RewindError::Storage(format!(
                "write stream {name} on {}: {error}",
                target.display()
            ))
        })?;
        stream.sync_all().map_err(|error| {
            RewindError::Storage(format!(
                "flush stream {name} on {}: {error}",
                target.display()
            ))
        })?;
    }
    Ok(())
}

/// Applies a recorded explicit DACL to a freshly installed regular file
/// (Phase 5 slice 4). Only ever called on a leaf `verify_mutation_confined`
/// has just confirmed as a real regular file. The ACL is rebuilt from the
/// recorded ACEs in recorded order — probe-verified to reproduce the
/// original SDDL byte-for-byte, with unprotected application re-deriving the
/// parent's inherited ACEs and protected application leaving only the
/// recorded set. Trustee SIDs are materialized from their recorded string
/// form: a well-formed SID string converts even when no account resolves,
/// so restore is literal, never name-dependent.
#[cfg(windows)]
fn install_dacl(target: &Path, dacl: &crate::model::DaclFingerprint) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const SE_FILE_OBJECT: u32 = 1;
    const DACL_SECURITY_INFORMATION: u32 = 0x4;
    const UNPROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x2000_0000;
    const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;
    const ACL_REVISION: u8 = 2;

    // Each converted SID stays alive until every ACE byte is copied.
    let mut converted: Vec<PSid> = Vec::with_capacity(dacl.aces.len());
    let mut body: Vec<u8> = Vec::new();
    let mut ace_count = 0u16;
    let result = (|| -> Result<()> {
        for ace in &dacl.aces {
            let wide: Vec<u16> = std::ffi::OsStr::new(&ace.sid)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let mut sid: PSid = std::ptr::null_mut();
            // SAFETY: `wide` outlives the call; the returned SID is
            // LocalFree'd before this function returns.
            let ok = unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut sid) };
            if ok == 0 || sid.is_null() {
                return Err(RewindError::Storage(format!(
                    "recorded DACL trustee SID {} cannot be materialized for {}: {}",
                    ace.sid,
                    target.display(),
                    std::io::Error::last_os_error()
                )));
            }
            converted.push(sid);
            // SAFETY: `sid` is a valid SID from ConvertStringSidToSidW.
            let sid_len = unsafe { GetLengthSid(sid) } as usize;
            // Simple ACE body: [type u8][flags u8][size u16][mask u32][SID].
            let ace_size = 8 + sid_len;
            let start = body.len();
            body.resize(start + ace_size, 0);
            body[start] = ace.ace_type;
            body[start + 1] = ace.flags;
            body[start + 2..start + 4].copy_from_slice(&(ace_size as u16).to_le_bytes());
            body[start + 4..start + 8].copy_from_slice(&ace.mask.to_le_bytes());
            // SAFETY: copying `sid_len` bytes out of the converted SID into
            // the ACE body.
            body[start + 8..start + ace_size]
                .copy_from_slice(unsafe { std::slice::from_raw_parts(sid as *const u8, sid_len) });
            ace_count += 1;
        }
        Ok(())
    })();
    if let Err(error) = result {
        for sid in converted {
            unsafe { LocalFree(sid) };
        }
        return Err(error);
    }

    // The ACL buffer must be 4-aligned (SID SubAuthority fields are read as
    // aligned u32s): allocate u32 words and write bytes into them, so the
    // alignment is guaranteed by the allocator rather than assumed.
    let total = 8 + body.len();
    let mut acl: Vec<u32> = vec![0; total.div_ceil(4)];
    acl[0] = ACL_REVISION as u32 | ((total as u32) << 16);
    acl[1] = ace_count as u32;
    for (index, word) in body.chunks(4).enumerate() {
        let mut bytes = [0u8; 4];
        bytes[..word.len()].copy_from_slice(word);
        acl[2 + index] = u32::from_le_bytes(bytes);
    }

    let wide_target: Vec<u16> = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mode = DACL_SECURITY_INFORMATION
        | if dacl.protected {
            PROTECTED_DACL_SECURITY_INFORMATION
        } else {
            UNPROTECTED_DACL_SECURITY_INFORMATION
        };
    // SAFETY: `wide_target` and `acl` are owned and valid for the call; the
    // call mutates only the target object's DACL.
    let status = unsafe {
        SetNamedSecurityInfoW(
            wide_target.as_ptr(),
            SE_FILE_OBJECT,
            mode,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl.as_ptr() as *mut std::ffi::c_void,
            std::ptr::null_mut(),
        )
    };
    for sid in converted {
        unsafe { LocalFree(sid) };
    }
    if status != 0 {
        return Err(RewindError::Storage(format!(
            "apply recorded DACL on {}: status {status}: {}",
            target.display(),
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

/// The DACL applier — the crate's seventh minimal FFI site (the write side;
/// capture lives in `scan.rs`). SIDs are converted from their recorded
/// string form; `GetLengthSid` sizes each ACE body; `SetNamedSecurityInfoW`
/// applies the rebuilt ACL under the recorded protection flag.
#[cfg(windows)]
type PSid = *mut std::ffi::c_void;

#[cfg(windows)]
extern "system" {
    fn ConvertStringSidToSidW(string: *const u16, sid: *mut PSid) -> i32;
    fn GetLengthSid(sid: PSid) -> u32;
    fn SetNamedSecurityInfoW(
        object_name: *const u16,
        object_type: u32,
        security_info: u32,
        psid_owner: PSid,
        psid_group: PSid,
        p_dacl: *mut std::ffi::c_void,
        p_sacl: *mut std::ffi::c_void,
    ) -> u32;
    fn LocalFree(handle: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}

fn apply_metadata(path: &Path, fingerprint: &Fingerprint) -> Result<()> {
    let metadata = match fingerprint {
        Fingerprint::RegularFile { metadata, .. }
        | Fingerprint::Directory { metadata, .. }
        | Fingerprint::NamedPipe { metadata } => metadata,
        _ => return Ok(()),
    };
    let mut permissions = fs::metadata(path)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The recorded mode is authoritative on Unix. `readonly` is the
        // Windows representation of writability; on Unix it must only ever
        // *clear* write bits — `set_readonly(false)` ORs 0o222 into the
        // mode, which would corrupt a restored 0o644 into 0o666.
        if let Some(mode) = metadata.mode {
            let mut mode = mode;
            if metadata.readonly {
                mode &= !0o222;
            }
            permissions.set_mode(mode);
        }
    }
    #[cfg(not(unix))]
    {
        permissions.set_readonly(metadata.readonly);
    }
    fs::set_permissions(path, permissions)?;
    Ok(())
}

/// Creates a symlink from the recorded fingerprint. The file-vs-directory
/// flavor comes from the recorded `target_kind`, never from a filename
/// extension; an unknown kind is refused instead of guessed. The target is
/// applied literally and never followed.
fn create_symlink(
    target: &str,
    target_kind: crate::model::SymlinkTargetKind,
    destination: &Path,
) -> Result<()> {
    if destination.exists() || fs::symlink_metadata(destination).is_ok() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        let _ = target_kind;
        std::os::unix::fs::symlink(target, destination)?;
    }
    #[cfg(windows)]
    {
        use crate::model::SymlinkTargetKind;
        match target_kind {
            SymlinkTargetKind::File => std::os::windows::fs::symlink_file(target, destination)?,
            SymlinkTargetKind::Directory => std::os::windows::fs::symlink_dir(target, destination)?,
            // The scan never records a creatable symlink without a positive
            // kind; a persisted state that lacks one (or was crafted) is
            // refused rather than restored with a guessed flavor.
            SymlinkTargetKind::Unknown => {
                return Err(RewindError::Unsupported(format!(
                    "symlink target kind is unknown; refusing to guess restoration for {}",
                    destination.display()
                )));
            }
        }
    }
    Ok(())
}

/// Creates a junction from the recorded reparse data (Phase 5 slice 2).
/// The substitute and print names are applied literally — never resolved,
/// never followed, never validated against the target's existence (a
/// dangling junction restores exactly like a live one). Attributes are
/// applied while the path is still a plain directory, because attribute
/// APIs follow junctions; the reparse data is then written and the entry
/// becomes the junction the record specifies.
#[cfg(windows)]
fn create_junction(
    destination: &Path,
    substitute: &str,
    print_name: &str,
    metadata: &crate::model::MetadataFingerprint,
) -> Result<()> {
    // Idempotent replay (recovery re-applies a step, and the crash window
    // between journal writes makes re-application legitimate): a destination
    // that is already exactly the recorded junction is done. Anything else
    // is an error — the quarantine moved the prior object away, and the
    // per-step pre-check would have short-circuited a matching state.
    if let Ok(existing) = fs::symlink_metadata(destination) {
        if existing.file_type().is_symlink()
            && crate::scan::reparse_junction_names(destination)
                == Some((substitute.to_owned(), print_name.to_owned()))
            && existing.permissions().readonly() == metadata.readonly
        {
            return Ok(());
        }
        return Err(RewindError::Storage(format!(
            "refusing to replace existing object at {} while restoring a junction",
            destination.display()
        )));
    }
    fs::create_dir(destination)?;
    // The recorded readonly flag is applied to the plain directory BEFORE
    // the reparse data exists: afterwards the path is a junction and
    // attribute APIs would follow it to the target. A fresh directory is
    // never readonly, so there is nothing to clear.
    if metadata.readonly {
        let mut permissions = fs::symlink_metadata(destination)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(destination, permissions)?;
    }
    junction_ffi::set_reparse_point(destination, substitute, print_name)?;
    Ok(())
}

/// The junction-creation FFI — the crate's fourth minimal FFI site, alongside
/// `reparse_tag` (`src/scan.rs`), `CreateProcessW`
/// (`src/watch/detach_windows.rs`), and `mkfifo(2)` (`src/rollback.rs`).
///
/// `FSCTL_SET_REPARSE_POINT` is CTL_CODE(0x0009, 0x029, METHOD_BUFFERED,
/// FILE_ANY_ACCESS) = 0x000900A4. The handle is opened GENERIC_WRITE with
/// FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS on the plain
/// directory; junction (mount-point) creation requires no privilege,
/// unlike symlinks. The buffer layout is probe-verified against a real
/// `mklink /J` junction: 8-byte header, four u16 offset/length fields
/// (offsets relative to PathBuffer, lengths excluding the NUL
/// terminators), then substitute UTF-16 + NUL + print UTF-16 + NUL;
/// ReparseDataLength covers everything after the header.
#[cfg(windows)]
mod junction_ffi {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use crate::error::{Result, RewindError};

    const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
    const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const OPEN_EXISTING: u32 = 0x0000_0003;
    const INVALID_HANDLE_VALUE: *mut c_void = usize::MAX as *mut c_void;
    // Bounded far below the 16 KiB reparse-buffer maximum so every u16
    // offset and length below is exact.
    const MAX_NAME_UNITS: usize = 4000;

    extern "system" {
        fn CreateFileW(
            filename: *const u16,
            desired_access: u32,
            share_mode: u32,
            security_attributes: *mut c_void,
            creation_disposition: u32,
            flags_and_attributes: u32,
            template_file: *mut c_void,
        ) -> *mut c_void;
        fn DeviceIoControl(
            device: *mut c_void,
            control_code: u32,
            in_buffer: *mut c_void,
            in_size: u32,
            out_buffer: *mut c_void,
            out_size: u32,
            bytes_returned: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    pub(super) fn set_reparse_point(path: &Path, substitute: &str, print_name: &str) -> Result<()> {
        let substitute_units: Vec<u16> = substitute.encode_utf16().collect();
        let print_units: Vec<u16> = print_name.encode_utf16().collect();
        if substitute_units.len() > MAX_NAME_UNITS || print_units.len() > MAX_NAME_UNITS {
            return Err(RewindError::Unsupported(format!(
                "junction reparse data exceeds the reparse buffer: {}",
                path.display()
            )));
        }
        let substitute_length = 2 * substitute_units.len();
        let print_length = 2 * print_units.len();
        let data_length = 8 + substitute_length + 2 + print_length + 2;
        let mut buffer = Vec::with_capacity(8 + data_length);
        buffer.extend_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
        buffer.extend_from_slice(&(data_length as u16).to_le_bytes());
        buffer.extend_from_slice(&0u16.to_le_bytes()); // Reserved
        buffer.extend_from_slice(&0u16.to_le_bytes()); // SubstituteNameOffset
        buffer.extend_from_slice(&(substitute_length as u16).to_le_bytes());
        // The print name follows the substitute name's NUL terminator.
        buffer.extend_from_slice(&((substitute_length + 2) as u16).to_le_bytes());
        buffer.extend_from_slice(&(print_length as u16).to_le_bytes());
        for unit in substitute_units.iter().copied().chain(std::iter::once(0)) {
            buffer.extend_from_slice(&unit.to_le_bytes());
        }
        for unit in print_units.iter().copied().chain(std::iter::once(0)) {
            buffer.extend_from_slice(&unit.to_le_bytes());
        }

        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `wide` and `buffer` are owned for the duration of the
        // call; the handle is closed on every return path; the operation
        // writes the reparse data of the object named by `wide` and nothing
        // else. The query is the only system state read.
        unsafe {
            let handle = CreateFileW(
                wide.as_ptr(),
                GENERIC_WRITE,
                0,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            );
            if handle == INVALID_HANDLE_VALUE {
                return Err(RewindError::Storage(format!(
                    "open junction target {}: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                )));
            }
            let ok = DeviceIoControl(
                handle,
                FSCTL_SET_REPARSE_POINT,
                buffer.as_ptr().cast::<c_void>() as *mut c_void,
                buffer.len() as u32,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            let error = std::io::Error::last_os_error();
            CloseHandle(handle);
            if ok == 0 {
                return Err(RewindError::Storage(format!(
                    "set junction reparse point {}: {}",
                    path.display(),
                    error
                )));
            }
        }
        Ok(())
    }
}

fn sync_target(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    // A FIFO must never be opened, not even for reading: open(2) on a FIFO
    // blocks until a writer appears, which is exactly what hung the POSIX CI
    // jobs for hours. A FIFO has no byte content to make durable; the
    // durability of its directory entry is the parent fsync below.
    let is_fifo = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            metadata.file_type().is_fifo()
        }
        #[cfg(not(unix))]
        {
            false
        }
    };
    if !metadata.file_type().is_symlink() && !is_fifo {
        match fs::OpenOptions::new().read(true).open(path) {
            Ok(file) => {
                #[cfg(unix)]
                file.sync_all()?;
                #[cfg(windows)]
                {
                    let _ = file.sync_all();
                }
            }
            Err(error) if cfg!(windows) && metadata.is_dir() => {
                let _ = error;
            }
            Err(error) => return Err(error.into()),
        }
    }
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn sync_parent(path: &Path) -> Result<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub fn recover_locked(workspace: &Workspace) -> Result<()> {
    // Metadata repair for physically committed transactions runs first; it
    // is idempotent, never touches the filesystem, and guarantees that a
    // crash between the journal COMMITTED write and the catalog updates
    // cannot leave contradictory baseline/operation rows behind.
    workspace.repair_committed_metadata()?;
    for (_path, mut journal) in workspace.storage.journals.pending_archives()? {
        archive_committed_transaction(workspace, &mut journal);
        dispose_of_staging(workspace, &journal);
    }
    for (_id, _status, journal_path) in workspace
        .storage
        .catalog
        .unfinished_transactions(workspace.id)?
    {
        if !Path::new(&journal_path).is_file() {
            workspace.storage.catalog.set_workspace(
                workspace.id,
                &WorkspaceCondition::RecoveryRequired,
                None,
            )?;
            return Err(RewindError::RecoveryRequired(format!(
                "transaction journal is missing: {journal_path}"
            )));
        }
    }
    let unfinished = workspace.storage.journals.unfinished()?;
    if unfinished.is_empty() {
        return Ok(());
    }
    // Fix #20: from this point an unfinished transaction exists. Any
    // failure in classification or completion — including transient
    // storage, CAS, or I/O errors raised through `?` — must leave the
    // workspace in RECOVERY_REQUIRED, never falsely HEALTHY. The inner
    // closure returns the classification outcome and the wrapper enforces
    // the condition transition before propagating the error.
    let outcome = (|| -> Result<()> {
        for (_path, mut journal) in unfinished {
            if journal.workspace_id != workspace.id {
                return Err(RewindError::RecoveryRequired(
                    "journal belongs to another workspace".to_owned(),
                ));
            }
            if matches!(
                journal.status,
                JournalStatus::Planned | JournalStatus::Prepared
            ) {
                prepare_steps(workspace, &mut journal)?;
                journal.status = JournalStatus::Prepared;
                workspace.storage.journals.write(&journal)?;
            }
            let target = workspace.state_manifest(&journal.target_state_id)?;
            for index in 0..journal.steps.len() {
                let step = journal.steps[index].clone();
                let Some(path) = step.paths.first() else {
                    continue;
                };
                let scan = workspace.scan(None)?;
                let actual = scan.manifest.get(path);
                let desired = step.after.get(path).ok_or_else(|| {
                    RewindError::Journal(format!("recovery step {index} has no after state"))
                })?;
                let expected_before = step.before.get(path).ok_or_else(|| {
                    RewindError::Journal(format!("recovery step {index} has no before state"))
                })?;
                if compatible_after(&actual, desired)
                    && backup_is_sufficient(&step, expected_before)
                {
                    journal.steps[index].status = JournalStatus::Durable;
                    workspace.storage.journals.write(&journal)?;
                    continue;
                }
                if actual != *expected_before {
                    let known_quarantine_partial = matches!(actual, Fingerprint::Absent)
                        && backup_matches_fingerprint(&step, expected_before)
                        && desired_artifact_is_ready(&step, desired);
                    if known_quarantine_partial {
                        if let Err(error) = apply_step(workspace, &mut journal, index, true) {
                            journal.status = JournalStatus::RecoveryRequired;
                            journal.steps[index].status = JournalStatus::RecoveryRequired;
                            workspace.storage.journals.write(&journal)?;
                            workspace.storage.catalog.update_transaction(
                                journal.transaction_id,
                                &JournalStatus::RecoveryRequired,
                            )?;
                            workspace.storage.catalog.set_workspace(
                                workspace.id,
                                &WorkspaceCondition::RecoveryRequired,
                                Some(&journal.anchor_state_id),
                            )?;
                            return Err(error);
                        }
                        continue;
                    }
                    journal.status = JournalStatus::RecoveryRequired;
                    journal.steps[index].status = JournalStatus::RecoveryRequired;
                    workspace.storage.journals.write(&journal)?;
                    workspace.storage.catalog.update_transaction(
                        journal.transaction_id,
                        &JournalStatus::RecoveryRequired,
                    )?;
                    workspace.storage.catalog.set_workspace(
                        workspace.id,
                        &WorkspaceCondition::RecoveryRequired,
                        Some(&journal.anchor_state_id),
                    )?;
                    return Err(RewindError::RecoveryRequired(format!(
                        "cannot classify recovery state at {path}"
                    )));
                }
                if let Err(error) = apply_step(workspace, &mut journal, index, false) {
                    journal.status = JournalStatus::RecoveryRequired;
                    workspace.storage.journals.write(&journal)?;
                    workspace.storage.catalog.update_transaction(
                        journal.transaction_id,
                        &JournalStatus::RecoveryRequired,
                    )?;
                    workspace.storage.catalog.set_workspace(
                        workspace.id,
                        &WorkspaceCondition::RecoveryRequired,
                        Some(&journal.anchor_state_id),
                    )?;
                    return Err(error);
                }
            }
            let final_scan = match workspace.scan(None) {
                Ok(scan) => scan,
                Err(error) => {
                    journal.status = JournalStatus::RecoveryRequired;
                    workspace.storage.journals.write(&journal)?;
                    workspace.storage.catalog.update_transaction(
                        journal.transaction_id,
                        &JournalStatus::RecoveryRequired,
                    )?;
                    workspace.storage.catalog.set_workspace(
                        workspace.id,
                        &WorkspaceCondition::RecoveryRequired,
                        Some(&journal.anchor_state_id),
                    )?;
                    return Err(error);
                }
            };
            if final_scan.state_id != journal.target_state_id || final_scan.manifest != target {
                journal.status = JournalStatus::RecoveryRequired;
                workspace.storage.journals.write(&journal)?;
                workspace
                    .storage
                    .catalog
                    .update_transaction(journal.transaction_id, &JournalStatus::RecoveryRequired)?;
                workspace.storage.catalog.set_workspace(
                    workspace.id,
                    &WorkspaceCondition::RecoveryRequired,
                    Some(&journal.anchor_state_id),
                )?;
                return Err(RewindError::RecoveryRequired(
                    "recovered steps do not match target state".to_owned(),
                ));
            }
            journal.status = JournalStatus::Committed;
            workspace.storage.journals.write(&journal)?;
            workspace
                .storage
                .catalog
                .update_transaction(journal.transaction_id, &JournalStatus::Committed)?;
            workspace.storage.catalog.set_workspace(
                workspace.id,
                &WorkspaceCondition::Healthy,
                Some(&journal.target_state_id),
            )?;
            if let Some(operation_id) = journal.operation_id {
                let status = if journal.direction == "UNDO" {
                    OperationStatus::Undone
                } else {
                    OperationStatus::Completed
                };
                workspace
                    .storage
                    .catalog
                    .set_operation_status(operation_id, status)?;
            }
            archive_committed_transaction(workspace, &mut journal);
            dispose_of_staging(workspace, &journal);
        }
        Ok(())
    })();
    if let Err(error) = outcome {
        let anchor = workspace
            .storage
            .journals
            .unfinished()?
            .first()
            .map(|(_path, journal)| journal.anchor_state_id.clone());
        let condition = workspace.condition().unwrap_or(WorkspaceCondition::Healthy);
        if !matches!(condition, WorkspaceCondition::RecoveryRequired) {
            workspace.storage.catalog.set_workspace(
                workspace.id,
                &WorkspaceCondition::RecoveryRequired,
                anchor.as_deref(),
            )?;
        }
        return Err(error);
    }
    Ok(())
}

/// Produces a per-step physical classification of every unfinished
/// transaction so `rewind recover` can explain exactly why a state could not
/// be completed automatically. Read-only.
pub fn diagnose_recoverable(workspace: &Workspace) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for (_path, journal) in workspace.storage.journals.unfinished()? {
        lines.push(format!(
            "transaction {} direction={} status={} anchor={} target={}",
            journal.transaction_id,
            journal.direction,
            journal.status.as_str(),
            journal.anchor_state_id,
            journal.target_state_id
        ));
        let scan = workspace.scan(None).ok();
        for step in &journal.steps {
            let Some(path) = step.paths.first() else {
                continue;
            };
            let expected_before = step
                .before
                .get(path)
                .map(Fingerprint::describe)
                .unwrap_or_else(|| "missing".to_owned());
            let desired = step
                .after
                .get(path)
                .map(Fingerprint::describe)
                .unwrap_or_else(|| "missing".to_owned());
            let actual = scan
                .as_ref()
                .map(|result| result.manifest.get(path).describe())
                .unwrap_or_else(|| "workspace could not be scanned".to_owned());
            lines.push(format!(
                "  step {} {path}: expected_before={expected_before} planned_after={desired} actual={actual} backup={} staging={}",
                step.id,
                step.backup_path.as_deref().unwrap_or("-"),
                step.staging_path.as_deref().unwrap_or("-"),
            ));
        }
    }
    Ok(lines)
}

/// Explicit recovery exit path for RECOVERY_REQUIRED (V-F02). It first runs
/// the ordinary automatic classification and completion. If the physical
/// state cannot be classified into a known plan, this function abandons the
/// unfinished transactions of this workspace: quarantined artifacts are
/// archived, journals are marked ABANDONED, an unknown interval is recorded,
/// and a full reconciliation establishes the current live state as the new
/// trusted checkpoint. Nothing on disk is overwritten and no journal evidence
/// is deleted; if an artifact cannot be archived, its transaction-local
/// staging is retained.
pub fn recover_reconcile(workspace: &Workspace) -> Result<String> {
    match recover_locked(workspace) {
        Ok(()) => return workspace.baseline_id(),
        // Only a classification failure justifies abandonment. Transient
        // storage, CAS, or I/O errors must not convert a completable
        // transaction into an abandoned one.
        Err(RewindError::RecoveryRequired(_)) | Err(RewindError::Conflict { .. }) => {}
        Err(error) => return Err(error),
    }
    let unfinished = workspace.storage.journals.unfinished()?;
    let mut abandoned = 0_usize;
    for (_path, mut journal) in unfinished {
        if journal.workspace_id != workspace.id {
            return Err(RewindError::RecoveryRequired(
                "journal belongs to another workspace".to_owned(),
            ));
        }
        if journal.steps.iter().any(|step| step.backup_path.is_some())
            && matches!(journal.archive_status, ArchiveStatus::NotRequired)
        {
            journal.archive_status = ArchiveStatus::Pending;
        }
        // Steps of an interrupted transaction that never executed have no
        // quarantined artifact to preserve; their planned backup paths are
        // cleared so the archive reports exactly what exists instead of
        // failing on artifacts that were never created.
        for step in &mut journal.steps {
            if let Some(backup) = &step.backup_path {
                if !std::path::Path::new(backup).is_file() && !std::path::Path::new(backup).is_dir()
                {
                    step.backup_path = None;
                }
            }
        }
        archive_committed_transaction(workspace, &mut journal);
        journal.status = JournalStatus::Abandoned;
        workspace.storage.journals.write(&journal)?;
        workspace
            .storage
            .catalog
            .update_transaction(journal.transaction_id, &JournalStatus::Abandoned)?;
        if !workspace.storage.catalog.has_open_unknown(workspace.id)? {
            workspace.storage.catalog.open_unknown(
                workspace.id,
                Some(&journal.anchor_state_id),
                "unfinished transaction abandoned by explicit recovery",
            )?;
        }
        dispose_of_staging(workspace, &journal);
        abandoned += 1;
    }
    if abandoned == 0 {
        return Err(RewindError::RecoveryRequired(
            "no unfinished transaction was present to abandon".to_owned(),
        ));
    }
    let baseline = workspace.row()?.baseline_state;
    workspace.storage.catalog.set_workspace(
        workspace.id,
        &WorkspaceCondition::Degraded,
        baseline.as_deref(),
    )?;
    workspace.reconcile_locked("recovery after abandoned transaction")
}

/// Removes the transaction-local staging directory once it can no longer be
/// needed by recovery: the journal must be terminal and every quarantined
/// artifact must already have been copied into the external archive, or none
/// was required. Staging that still holds the only copy of quarantined bytes
/// is retained on disk.
fn dispose_of_staging(workspace: &Workspace, journal: &Journal) {
    if !journal.status.is_terminal() {
        return;
    }
    if !matches!(
        journal.archive_status,
        ArchiveStatus::Archived | ArchiveStatus::NotRequired
    ) {
        return;
    }
    let Some(parent) = workspace.root.parent() else {
        return;
    };
    let staging = parent
        .join(".rewind-txn")
        .join(journal.transaction_id.to_string());
    if staging.exists() {
        if let Err(error) = fs::remove_dir_all(&staging) {
            eprintln!(
                "rewind: could not remove transaction staging {}: {error}",
                staging.display()
            );
        }
    }
}

fn archive_committed_transaction(workspace: &Workspace, journal: &mut Journal) {
    if !matches!(
        journal.archive_status,
        ArchiveStatus::Pending | ArchiveStatus::Failed
    ) {
        return;
    }
    let archive_root = workspace
        .storage
        .project_root
        .join("archive")
        .join(journal.transaction_id.to_string());
    let result = (|| -> Result<()> {
        fs::create_dir_all(&archive_root)?;
        for step in &journal.steps {
            let Some(source) = step.backup_path.as_deref() else {
                continue;
            };
            let source = Path::new(source);
            if fs::symlink_metadata(source).is_err() {
                return Err(RewindError::Storage(format!(
                    "local quarantine artifact is missing: {}",
                    source.display()
                )));
            }
            let destination = archive_root.join(step.id.to_string());
            copy_artifact(source, &destination)?;
            verify_archive_pair(source, &destination)?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            journal.archive_status = ArchiveStatus::Archived;
            journal.archive_error = None;
        }
        Err(error) => {
            journal.archive_status = ArchiveStatus::Failed;
            journal.archive_error = Some(error.to_string());
        }
    }
    if let Err(error) = workspace.storage.journals.write(journal) {
        eprintln!(
            "rewind archive diagnostic: could not persist archive status for {}: {error}",
            journal.transaction_id
        );
    }
}

fn copy_artifact(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    // A junction is a name-surrogate reparse point: std reports it as a
    // symlink, but recreating it as a symlink would fabricate a different
    // object class, and a junction carries no content to archive — the
    // journal's recorded fingerprint is the recovery record. Archival is
    // refused with a named reason and the local quarantine remains,
    // exactly as for FIFOs (Phase 5 slice 2). The check runs at every
    // recursion depth.
    #[cfg(windows)]
    {
        if metadata.file_type().is_symlink()
            && crate::scan::reparse_junction_names(source).is_some()
        {
            return Err(RewindError::Unsupported(format!(
                "junction quarantine objects are not archived; the recorded fingerprint is the recovery record: {}",
                source.display()
            )));
        }
    }
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, destination)?;
        #[cfg(windows)]
        {
            // The copy preserves the link's own file/dir flavor by inspecting
            // the local reparse data, never by guessing from the target
            // string.
            match crate::scan::reparse_symlink_kind(source) {
                Some(crate::model::SymlinkTargetKind::Directory) => {
                    std::os::windows::fs::symlink_dir(&target, destination)?
                }
                Some(crate::model::SymlinkTargetKind::File) => {
                    std::os::windows::fs::symlink_file(&target, destination)?
                }
                _ => {
                    return Err(RewindError::Unsupported(format!(
                        "cannot archive symlink with unreadable kind: {}",
                        source.display()
                    )));
                }
            }
        }
    } else if metadata.is_dir() {
        fs::create_dir_all(destination)?;
        for item in fs::read_dir(source)? {
            let item = item?;
            copy_artifact(&item.path(), &destination.join(item.file_name()))?;
        }
    } else if metadata.is_file() {
        fs::copy(source, destination)?;
        // FlushFileBuffers on Windows requires a handle opened with write
        // access, so the copied file is reopened for writing before the
        // durability request; a read-only handle fails with os error 5.
        let file = OpenOptions::new().write(true).open(destination)?;
        file.sync_all()?;
        // `fs::copy` (CopyFileW) drops the source's explicit DACL ACEs
        // (probe-verified): re-apply the source's live explicit DACL to the
        // copy, or the archive would silently hold de-permissioned state.
        // A failed read or apply fails the archive — never a silently
        // stripped copy. `verify_archive_pair` still compares both sides
        // before anything is marked Archived.
        #[cfg(windows)]
        {
            if let Some(dacl) = crate::scan::explicit_dacl(source)? {
                install_dacl(destination, &dacl)?;
            }
        }
    } else {
        return Err(RewindError::Unsupported(format!(
            "cannot archive unsupported quarantine object {}",
            source.display()
        )));
    }
    Ok(())
}

/// Recursively verifies that an external archive copy is byte- and
/// type-faithful to the quarantined original (Fix #4). Regular files are
/// compared by size and BLAKE3 content hash; symlinks by literal target and
/// recorded kind; directories by child-name sets and recursive descent.
/// `ArchiveStatus::Archived` is only ever set after this passes, so a
/// shallow match can never authorize disposal of the last surviving copy.
pub fn verify_archive_pair(source: &Path, destination: &Path) -> Result<()> {
    let source_metadata = fs::symlink_metadata(source).map_err(|error| {
        RewindError::Storage(format!("quarantine {}: {error}", source.display()))
    })?;
    let destination_metadata = fs::symlink_metadata(destination).map_err(|error| {
        RewindError::Storage(format!("archive {}: {error}", destination.display()))
    })?;
    if source_metadata.file_type().is_symlink() != destination_metadata.file_type().is_symlink()
        || source_metadata.is_dir() != destination_metadata.is_dir()
        || source_metadata.is_file() != destination_metadata.is_file()
    {
        return Err(RewindError::Storage(format!(
            "archive type mismatch for {}",
            source.display()
        )));
    }
    if source_metadata.is_file() {
        if source_metadata.len() != destination_metadata.len() {
            return Err(RewindError::Storage(format!(
                "archive size mismatch for {}",
                source.display()
            )));
        }
        let source_hash = blake3_hash_path(source)?;
        let destination_hash = blake3_hash_path(destination)?;
        if source_hash != destination_hash {
            return Err(RewindError::Storage(format!(
                "archive content mismatch for {}",
                source.display()
            )));
        }
        // Named streams are content: the archived copy must carry every
        // stream with identical bytes (`fs::copy` carries them on Windows,
        // probe-verified) — a shallow copy can never authorize disposal of
        // stream content (Phase 5 slice 3).
        #[cfg(windows)]
        {
            let source_streams = crate::scan::named_streams(source)?;
            let archive_streams = crate::scan::named_streams(destination)?;
            let source_names: std::collections::BTreeSet<&str> = source_streams
                .iter()
                .map(|(name, _)| name.as_str())
                .collect();
            let archive_names: std::collections::BTreeSet<&str> = archive_streams
                .iter()
                .map(|(name, _)| name.as_str())
                .collect();
            if source_names != archive_names {
                return Err(RewindError::Storage(format!(
                    "archive stream set mismatch for {}: {source_names:?} vs {archive_names:?}",
                    source.display()
                )));
            }
            for (name, _size) in &source_streams {
                let source_stream_hash =
                    blake3_hash_path(crate::scan::stream_spec(source, name).as_ref())?;
                let archive_stream_hash =
                    blake3_hash_path(crate::scan::stream_spec(destination, name).as_ref())?;
                if source_stream_hash != archive_stream_hash {
                    return Err(RewindError::Storage(format!(
                        "archive stream {name} content mismatch for {}",
                        source.display()
                    )));
                }
            }
            // The explicit DACL is recorded state: the archived copy must
            // carry exactly the quarantined source's explicit ACEs and
            // protected flag (`copy_artifact` re-applied them because
            // `fs::copy` drops explicit ACEs, probe-verified) — a
            // permission-stripping copy can never authorize disposal of the
            // last surviving copy (Phase 5 slice 4). An unreadable
            // descriptor fails the verification outright.
            let source_dacl = crate::scan::explicit_dacl(source)?;
            let archive_dacl = crate::scan::explicit_dacl(destination)?;
            if source_dacl != archive_dacl {
                return Err(RewindError::Storage(format!(
                    "archive DACL mismatch for {}: {source_dacl:?} vs {archive_dacl:?}",
                    source.display()
                )));
            }
        }
    } else if source_metadata.file_type().is_symlink() {
        let source_target = fs::read_link(source)?;
        let destination_target = fs::read_link(destination)?;
        if source_target != destination_target {
            return Err(RewindError::Storage(format!(
                "archive link target mismatch for {}: {} vs {}",
                source.display(),
                source_target.display(),
                destination_target.display()
            )));
        }
    } else if source_metadata.is_dir() {
        let mut source_entries = BTreeMap::new();
        for item in fs::read_dir(source)? {
            let item = item?;
            source_entries.insert(item.file_name().to_string_lossy().into_owned(), item.path());
        }
        let mut destination_entries = BTreeMap::new();
        for item in fs::read_dir(destination)? {
            let item = item?;
            destination_entries
                .insert(item.file_name().to_string_lossy().into_owned(), item.path());
        }
        let source_names: BTreeSet<String> = source_entries.keys().cloned().collect();
        let destination_names: BTreeSet<String> = destination_entries.keys().cloned().collect();
        if source_names != destination_names {
            let missing: Vec<String> = source_names
                .difference(&destination_names)
                .cloned()
                .collect();
            let unexpected: Vec<String> = destination_names
                .difference(&source_names)
                .cloned()
                .collect();
            return Err(RewindError::Storage(format!(
                "archive directory mismatch for {}: missing={missing:?} unexpected={unexpected:?}",
                source.display()
            )));
        }
        for (name, source_path) in &source_entries {
            let destination_path = &destination_entries[name];
            verify_archive_pair(source_path, destination_path)?;
        }
    }
    Ok(())
}

fn blake3_hash_path(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let count = std::io::Read::read(&mut file, &mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}
