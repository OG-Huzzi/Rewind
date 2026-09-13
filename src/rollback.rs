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
        if let Fingerprint::RegularFile { content_hash, .. } = fingerprint {
            workspace.storage.cas.verify(content_hash)?;
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
    let current = workspace.scan(None)?;
    let actual = current.manifest.get(path);
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
        }
        journal.steps[index].backup_path = Some(backup.clone());
        workspace.storage.journals.write(journal)?;
    }
    match desired {
        Fingerprint::Absent => {}
        Fingerprint::RegularFile { .. } => {
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
            apply_metadata(&target_path, desired)?;
        }
        Fingerprint::Directory { .. } => {
            ensure_parent_directories(&workspace.root, &target_path)?;
            if !target_path.exists() {
                fs::create_dir(&target_path)?;
            }
            apply_metadata(&target_path, desired)?;
        }
        Fingerprint::Symlink { target, .. } => {
            ensure_parent_directories(&workspace.root, &target_path)?;
            create_symlink(target, &target_path)?;
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
    let verify = workspace.scan(None)?;
    let actual_after = verify.manifest.get(path);
    if !compatible_after(&actual_after, desired) {
        journal.status = JournalStatus::RecoveryRequired;
        journal.steps[index].status = JournalStatus::RecoveryRequired;
        workspace.storage.journals.write(journal)?;
        return Err(RewindError::RecoveryRequired(format!(
            "step {} did not reach expected after state at {}",
            index, path
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
            content_hash, size, ..
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
            hasher.finalize().to_hex().as_str() == content_hash
        }
        Fingerprint::Directory { .. } => metadata.is_dir(),
        Fingerprint::Symlink { target, .. } => {
            metadata.file_type().is_symlink()
                && fs::read_link(path)
                    .map(|actual| actual.to_string_lossy() == target.as_str())
                    .unwrap_or(false)
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

fn apply_metadata(path: &Path, fingerprint: &Fingerprint) -> Result<()> {
    let metadata = match fingerprint {
        Fingerprint::RegularFile { metadata, .. } | Fingerprint::Directory { metadata, .. } => {
            metadata
        }
        _ => return Ok(()),
    };
    let mut permissions = fs::metadata(path)?.permissions();
    #[cfg(unix)]
    if let Some(mode) = metadata.mode {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(mode);
    }
    permissions.set_readonly(metadata.readonly);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn create_symlink(target: &str, destination: &Path) -> Result<()> {
    if destination.exists() || fs::symlink_metadata(destination).is_ok() {
        return Ok(());
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, destination)?;
    #[cfg(windows)]
    {
        let target_path = Path::new(target);
        if target_path.extension().is_some() {
            std::os::windows::fs::symlink_file(target, destination)?;
        } else {
            std::os::windows::fs::symlink_dir(target, destination)?;
        }
    }
    Ok(())
}

fn sync_target(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_symlink() {
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
            if compatible_after(&actual, desired) && backup_is_sufficient(&step, expected_before) {
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
                workspace
                    .storage
                    .catalog
                    .update_transaction(journal.transaction_id, &JournalStatus::RecoveryRequired)?;
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
                workspace
                    .storage
                    .catalog
                    .update_transaction(journal.transaction_id, &JournalStatus::RecoveryRequired)?;
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
                workspace
                    .storage
                    .catalog
                    .update_transaction(journal.transaction_id, &JournalStatus::RecoveryRequired)?;
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
            verify_artifact_pair(source, &destination)?;
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
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, destination)?;
        #[cfg(windows)]
        {
            let target_path = Path::new(&target);
            if target_path.extension().is_some() {
                std::os::windows::fs::symlink_file(target, destination)?;
            } else {
                std::os::windows::fs::symlink_dir(target, destination)?;
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
    } else {
        return Err(RewindError::Unsupported(format!(
            "cannot archive unsupported quarantine object {}",
            source.display()
        )));
    }
    Ok(())
}

fn verify_artifact_pair(source: &Path, destination: &Path) -> Result<()> {
    let source_metadata = fs::symlink_metadata(source)?;
    let destination_metadata = fs::symlink_metadata(destination)?;
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
        let source_hash = blake3::hash(&fs::read(source)?);
        let destination_hash = blake3::hash(&fs::read(destination)?);
        if source_hash != destination_hash {
            return Err(RewindError::Storage(format!(
                "archive content mismatch for {}",
                source.display()
            )));
        }
    } else if source_metadata.file_type().is_symlink()
        && fs::read_link(source)? != fs::read_link(destination)?
    {
        return Err(RewindError::Storage(format!(
            "archive link mismatch for {}",
            source.display()
        )));
    } else if source_metadata.is_dir() {
        let mut source_entries = BTreeSet::new();
        for item in fs::read_dir(source)? {
            source_entries.insert(item?.file_name());
        }
        let mut destination_entries = BTreeSet::new();
        for item in fs::read_dir(destination)? {
            destination_entries.insert(item?.file_name());
        }
        if source_entries != destination_entries {
            return Err(RewindError::Storage(format!(
                "archive directory mismatch for {}",
                source.display()
            )));
        }
    }
    Ok(())
}
