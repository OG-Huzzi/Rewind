use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use fs2::FileExt;
use uuid::Uuid;

use crate::cas::Cas;
use crate::db::{Catalog, OperationDraft, WorkspaceRow};
use crate::error::{Result, RewindError};
use crate::journal::JournalStore;
use crate::model::{
    Fingerprint, JournalStatus, Manifest, OperationKind, OperationStatus, Reversibility, StateKind,
    TrackingConfidence, WorkspaceCondition, WorkspacePointer,
};
use crate::paths::{
    canonical_root, default_store_root, discover_marker, ensure_directory, marker_path,
    read_pointer, staging_root, write_pointer,
};
use crate::scan::{diff_manifests, scan_workspace, ScanOptions, ScanResult};

#[derive(Clone, Debug)]
pub struct Storage {
    pub root: PathBuf,
    pub project_root: PathBuf,
    pub catalog: Catalog,
    pub cas: Cas,
    pub journals: JournalStore,
}

impl Storage {
    pub fn open(root: PathBuf, workspace_id: Uuid) -> Result<Self> {
        let project_root = root.join("projects").join(workspace_id.to_string());
        ensure_directory(&root)?;
        ensure_directory(&root.join("projects"))?;
        ensure_directory(&project_root)?;
        ensure_directory(&project_root.join("journals"))?;
        ensure_directory(&project_root.join("anchors"))?;
        ensure_directory(&project_root.join("archive"))?;
        let cas = Cas::new(root.join("cas"))?;
        let catalog = Catalog::initialize(project_root.join("metadata.sqlite"))?;
        let journals = JournalStore::new(project_root.join("journals"))?;
        Ok(Self {
            root,
            project_root,
            catalog,
            cas,
            journals,
        })
    }

    pub fn journal_path(&self, transaction_id: Uuid) -> PathBuf {
        self.journals.path_for(transaction_id)
    }
}

pub struct Workspace {
    pub id: Uuid,
    pub root: PathBuf,
    pub pointer: WorkspacePointer,
    pub storage: Storage,
}

pub struct WorkspaceLease {
    file: File,
}

impl Drop for WorkspaceLease {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl WorkspaceLease {
    /// Acquires the exclusive writer lease. The lock file is opened *without*
    /// truncation: a contender that has not won the lock must never clobber
    /// the owner metadata of the process that holds it. Only after the
    /// exclusive lock is acquired are the contents replaced with this
    /// process's owner record.
    pub fn acquire(workspace: &Workspace, nonblocking: bool) -> Result<Self> {
        use std::io::{Seek, SeekFrom, Write};
        let path = workspace.storage.project_root.join("lock.pid");
        // Deliberately NOT `.truncate(true)`: a contender that has not won the
        // lock must never clobber the current owner's metadata. Truncation
        // happens only after the exclusive lock is acquired below.
        #[allow(clippy::suspicious_open_options)]
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)?;
        let result = if nonblocking {
            file.try_lock_exclusive()
        } else {
            file.lock_exclusive()
        };
        let mut file = match result {
            Ok(()) => file,
            Err(error) => {
                return Err(RewindError::LockUnavailable(format!(
                    "{}: {error}",
                    path.display()
                )));
            }
        };
        // We own the lock: replacing owner metadata is now safe.
        let owner = format!(
            "pid={} transaction-process={}\n",
            std::process::id(),
            Uuid::new_v4()
        );
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(owner.as_bytes())?;
        file.sync_all()?;
        Ok(Self { file })
    }
}

#[derive(Debug)]
pub struct RunOutcome {
    pub operation_id: Option<i64>,
    pub exit_code: i32,
    pub captured: bool,
    pub capture_error: Option<String>,
}

impl Workspace {
    pub fn init(path: &Path, requested_store: Option<&Path>) -> Result<Self> {
        let root = canonical_root(path)?;
        let marker = marker_path(&root);
        if let Ok(metadata) = fs::symlink_metadata(&marker) {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(RewindError::PathEscape(format!(
                    "workspace marker is not a regular file: {}",
                    marker.display()
                )));
            }
        }
        if marker.is_file() {
            let pointer = read_pointer(&marker)?;
            if pointer.root != root.to_string_lossy() {
                return Err(RewindError::WorkspaceIdentity(format!(
                    "existing marker points to {}, not {}",
                    pointer.root,
                    root.display()
                )));
            }
            return Self::open_from_pointer(pointer);
        }
        let id = Uuid::new_v4();
        let store_root = requested_store
            .map(Path::to_path_buf)
            .unwrap_or(default_store_root()?);
        let store_root = if store_root.is_absolute() {
            store_root
        } else {
            std::env::current_dir()?.join(store_root)
        };
        fs::create_dir_all(&store_root)?;
        let store_root = fs::canonicalize(store_root)?;
        if store_root == root || store_root.starts_with(root.join("")) {
            return Err(RewindError::Storage(
                "external store must not be inside the workspace".to_owned(),
            ));
        }
        let pointer = WorkspacePointer {
            workspace_id: id,
            root: root.to_string_lossy().into_owned(),
            store_root: store_root.to_string_lossy().into_owned(),
            schema_version: 1,
        };
        let storage = Storage::open(store_root, id)?;
        storage
            .catalog
            .create_workspace(id, &root.to_string_lossy())?;
        let workspace = Self {
            id,
            root: root.clone(),
            pointer: pointer.clone(),
            storage,
        };
        let _lease = WorkspaceLease::acquire(&workspace, false)?;
        let scan = workspace.scan(None)?;
        let state_id = workspace.storage.catalog.insert_state(
            id,
            StateKind::Baseline,
            &scan.manifest,
            None,
            Some("initial"),
        )?;
        workspace.storage.catalog.set_workspace(
            id,
            &WorkspaceCondition::Healthy,
            Some(&state_id),
        )?;
        // Publish the workspace pointer last. A failed scan or catalog setup
        // therefore cannot leave an apparently initialized workspace behind.
        write_pointer(&root, &pointer)?;
        Ok(workspace)
    }

    pub fn open_from_current(start: &Path) -> Result<Self> {
        let marker = discover_marker(start)?;
        let pointer = read_pointer(&marker)?;
        Self::open_from_pointer(pointer)
    }

    pub fn open_from_pointer(pointer: WorkspacePointer) -> Result<Self> {
        let root = canonical_root(Path::new(&pointer.root))?;
        if root.to_string_lossy() != pointer.root {
            return Err(RewindError::WorkspaceIdentity(format!(
                "canonical workspace root changed from {} to {}",
                pointer.root,
                root.display()
            )));
        }
        let store_path = PathBuf::from(&pointer.store_root);
        ensure_directory(&store_path)?;
        let store_root = fs::canonicalize(&store_path)?;
        if store_root.to_string_lossy() != pointer.store_root {
            return Err(RewindError::WorkspaceIdentity(format!(
                "canonical external store changed from {} to {}",
                pointer.store_root,
                store_root.display()
            )));
        }
        let storage = Storage::open(store_root, pointer.workspace_id)?;
        let row = storage.catalog.workspace(pointer.workspace_id)?;
        if row.root != pointer.root {
            return Err(RewindError::WorkspaceIdentity(format!(
                "catalog root {} differs from pointer {}",
                row.root, pointer.root
            )));
        }
        Ok(Self {
            id: pointer.workspace_id,
            root,
            pointer,
            storage,
        })
    }

    pub fn row(&self) -> Result<WorkspaceRow> {
        self.storage.catalog.workspace(self.id)
    }

    pub fn condition(&self) -> Result<WorkspaceCondition> {
        Ok(self.row()?.condition)
    }

    pub fn baseline_id(&self) -> Result<String> {
        self.row()?
            .baseline_state
            .ok_or_else(|| RewindError::NotFound("trusted baseline".to_owned()))
    }

    pub fn scan(&self, deadline: Option<Instant>) -> Result<ScanResult> {
        scan_workspace(&self.root, &self.storage.cas, ScanOptions { deadline })
    }

    pub fn persist_state(
        &self,
        kind: StateKind,
        scan: &ScanResult,
        parent_id: Option<&str>,
        label: Option<&str>,
    ) -> Result<String> {
        self.storage
            .catalog
            .insert_state(self.id, kind, &scan.manifest, parent_id, label)
    }

    pub fn state_manifest(&self, state_id: &str) -> Result<Manifest> {
        Ok(self.storage.catalog.state(state_id)?.manifest)
    }

    /// Idempotent startup repair of *metadata* that a crash between the
    /// journal COMMITTED write and the catalog updates could have left
    /// contradictory (Phase 1.2 Fix #18): the physical transaction is done —
    /// the journal is its authority — so this must never mutate the
    /// filesystem. For every committed journal it enforces that the catalog
    /// transaction row is COMMITTED, the workspace condition/baseline point
    /// at the journal's target state, and the operation status matches the
    /// journal direction. Repairs are single-row catalog updates, safe to
    /// repeat on every startup.
    pub fn repair_committed_metadata(&self) -> Result<()> {
        let mut repaired = false;
        for entry in
            std::fs::read_dir(self.storage.project_root.join("journals")).map_err(|error| {
                RewindError::Storage(format!("journal directory unreadable: {error}"))
            })?
        {
            let entry = entry.map_err(|error| {
                RewindError::Storage(format!("journal directory entry: {error}"))
            })?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let journal: crate::model::Journal = self.storage.journals.load(&path)?;
            if !matches!(journal.status, crate::model::JournalStatus::Committed) {
                continue;
            }
            if let Err(error) = self.storage.catalog.update_transaction(
                journal.transaction_id,
                &crate::model::JournalStatus::Committed,
            ) {
                // The row may not exist for a workspace recreated from its
                // journal store; a real database failure still surfaces.
                match error {
                    RewindError::Database(_) => {}
                    other => return Err(other),
                }
            }
            if let Some(operation_id) = journal.operation_id {
                let expected_status = if journal.direction == "UNDO" {
                    crate::model::OperationStatus::Undone
                } else {
                    crate::model::OperationStatus::Completed
                };
                if let Ok(operation) = self.storage.catalog.operation(operation_id, self.id) {
                    if operation.status != expected_status {
                        self.storage
                            .catalog
                            .set_operation_status(operation_id, expected_status)?;
                        repaired = true;
                    }
                }
            }
            let row = self.row()?;
            let target_matches = row.baseline_state.as_deref() == Some(&journal.target_state_id);
            let healthy = matches!(row.condition, WorkspaceCondition::Healthy);
            if !target_matches || !healthy {
                // Only point the baseline at the committed target when the
                // live workspace agrees; otherwise the physical state must be
                // re-established by a scan, which the drift rules in
                // `run_command`/`reconcile` already perform. Metadata repair
                // never invents trust in an unverified filesystem.
                let scan = match self.scan(None) {
                    Ok(scan) => scan,
                    Err(_) => continue,
                };
                if scan.state_id == journal.target_state_id {
                    self.storage.catalog.set_workspace(
                        self.id,
                        &WorkspaceCondition::Healthy,
                        Some(&journal.target_state_id),
                    )?;
                    repaired = true;
                }
            }
        }
        let _ = repaired;
        Ok(())
    }

    pub fn reconcile_locked(&self, reason: &str) -> Result<String> {
        let condition = self.condition()?;
        if matches!(condition, WorkspaceCondition::RecoveryRequired) {
            return Err(RewindError::RecoveryRequired(
                "filesystem transaction recovery must complete first".to_owned(),
            ));
        }
        let previous = self.row()?.baseline_state;
        let scan = match self.scan(None) {
            Ok(scan) => scan,
            Err(error) => {
                self.mark_reconciliation_required(&error.to_string())?;
                return Err(error);
            }
        };
        let state_id = self.persist_state(
            StateKind::Reconciliation,
            &scan,
            previous.as_deref(),
            Some("reconciliation"),
        )?;
        if let Some(previous) = previous.as_deref() {
            if previous != state_id && !self.storage.catalog.has_open_unknown(self.id)? {
                self.storage
                    .catalog
                    .open_unknown(self.id, Some(previous), reason)?;
            }
        }
        self.storage.catalog.close_unknown(self.id, &state_id)?;
        self.storage.catalog.consume_bypasses(self.id)?;
        let bypass_log = self.storage.project_root.join("boundaries.log");
        if bypass_log.exists() {
            let file = OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&bypass_log)?;
            file.sync_all()?;
        }
        self.storage.catalog.set_workspace(
            self.id,
            &WorkspaceCondition::Healthy,
            Some(&state_id),
        )?;
        Ok(state_id)
    }

    pub fn ensure_healthy_locked(&self) -> Result<()> {
        self.enforce_pending_safety_gate()?;
        match self.condition()? {
            WorkspaceCondition::Healthy => Ok(()),
            WorkspaceCondition::Degraded | WorkspaceCondition::ReconciliationRequired => {
                self.reconcile_locked("automatic reconciliation before strong operation")?;
                Ok(())
            }
            WorkspaceCondition::RecoveryRequired => Err(RewindError::RecoveryRequired(
                "unfinished rollback requires recovery".to_owned(),
            )),
        }
    }

    pub fn enforce_pending_safety_gate(&self) -> Result<()> {
        let bypass_log = self.storage.project_root.join("boundaries.log");
        let log_pending = bypass_log
            .metadata()
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false);
        if self.storage.catalog.has_pending_bypass(self.id)? || log_pending {
            let baseline = self.row()?.baseline_state;
            if !self.storage.catalog.has_open_unknown(self.id)? {
                self.storage.catalog.open_unknown(
                    self.id,
                    baseline.as_deref(),
                    "passive boundary bypassed an active writer transaction",
                )?;
            }
            self.storage.catalog.set_workspace(
                self.id,
                &WorkspaceCondition::ReconciliationRequired,
                baseline.as_deref(),
            )?;
        }
        Ok(())
    }

    fn mark_reconciliation_required(&self, reason: &str) -> Result<()> {
        let baseline = self.row()?.baseline_state;
        if !self.storage.catalog.has_open_unknown(self.id)? {
            self.storage
                .catalog
                .open_unknown(self.id, baseline.as_deref(), reason)?;
        }
        self.storage.catalog.set_workspace(
            self.id,
            &WorkspaceCondition::ReconciliationRequired,
            baseline.as_deref(),
        )?;
        Ok(())
    }

    pub fn record_capture_failure(
        &self,
        pre_state_id: Option<&str>,
        command: Option<String>,
        cwd: Option<String>,
        exit_code: Option<i32>,
        error: &str,
    ) -> Result<i64> {
        let draft = OperationDraft {
            kind: OperationKind::CaptureFailed,
            status: OperationStatus::Failed,
            pre_state_id: pre_state_id.map(str::to_owned),
            post_state_id: None,
            command,
            cwd,
            exit_code,
            confidence: TrackingConfidence::Degraded,
            reversibility: Reversibility::Unavailable,
            error: Some(error.to_owned()),
            effects: Vec::new(),
        };
        let operation_id = self.storage.catalog.insert_operation(self.id, &draft)?;
        if !self.storage.catalog.has_open_unknown(self.id)? {
            self.storage
                .catalog
                .open_unknown(self.id, pre_state_id, error)?;
        }
        self.storage.catalog.set_workspace(
            self.id,
            &WorkspaceCondition::ReconciliationRequired,
            pre_state_id,
        )?;
        Ok(operation_id)
    }

    pub fn run_command(&self, command: &[String]) -> Result<RunOutcome> {
        if command.is_empty() {
            return Err(RewindError::InvalidCommand(
                "rewind run requires a command after --".to_owned(),
            ));
        }
        let _lease = WorkspaceLease::acquire(self, false)?;
        crate::rollback::recover_locked(self)?;
        self.repair_committed_metadata()?;
        self.ensure_healthy_locked()?;
        let baseline = self.baseline_id()?;
        let pre_scan = match self.scan(None) {
            Ok(scan) => scan,
            Err(error) => {
                self.mark_reconciliation_required(&error.to_string())?;
                return Err(error);
            }
        };
        let pre_state = if pre_scan.state_id != baseline {
            self.reconcile_locked("workspace drift detected before rewind run")?
        } else {
            self.persist_state(
                StateKind::Baseline,
                &pre_scan,
                Some(&baseline),
                Some("run-pre-state"),
            )?
        };
        let status = Command::new(&command[0])
            .args(&command[1..])
            .current_dir(&self.root)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()?;
        let exit_code = status.code().unwrap_or(1);
        let post_scan = match self.scan(None) {
            Ok(scan) => scan,
            Err(error) => {
                let operation_id = self.record_capture_failure(
                    Some(&pre_state),
                    Some(command.join(" ")),
                    Some(self.root.to_string_lossy().into_owned()),
                    Some(exit_code),
                    &error.to_string(),
                )?;
                return Ok(RunOutcome {
                    operation_id: Some(operation_id),
                    exit_code,
                    captured: false,
                    capture_error: Some(error.to_string()),
                });
            }
        };
        let post_state = match self.persist_state(
            StateKind::Baseline,
            &post_scan,
            Some(&pre_state),
            Some("run-post-state"),
        ) {
            Ok(state) => state,
            Err(error) => {
                let operation_id = self.record_capture_failure(
                    Some(&pre_state),
                    Some(command.join(" ")),
                    Some(self.root.to_string_lossy().into_owned()),
                    Some(exit_code),
                    &error.to_string(),
                )?;
                return Ok(RunOutcome {
                    operation_id: Some(operation_id),
                    exit_code,
                    captured: false,
                    capture_error: Some(error.to_string()),
                });
            }
        };
        let before = self.state_manifest(&pre_state)?;
        let effects = diff_manifests(&before, &post_scan.manifest);
        let reversible = if effects.iter().all(|effect| {
            effect.pre.is_supported_for_restore() && effect.post.is_supported_for_restore()
        }) {
            Reversibility::FullyReversible
        } else {
            Reversibility::Unsupported
        };
        let operation_id = match self.storage.catalog.insert_operation(
            self.id,
            &OperationDraft {
                kind: OperationKind::Strong,
                status: OperationStatus::Completed,
                pre_state_id: Some(pre_state.clone()),
                post_state_id: Some(post_state.clone()),
                command: Some(command.join(" ")),
                cwd: Some(self.root.to_string_lossy().into_owned()),
                exit_code: Some(exit_code),
                confidence: TrackingConfidence::Tracked,
                reversibility: reversible,
                error: None,
                effects,
            },
        ) {
            Ok(operation_id) => operation_id,
            Err(error) => {
                let failure_id = self.record_capture_failure(
                    Some(&pre_state),
                    Some(command.join(" ")),
                    Some(self.root.to_string_lossy().into_owned()),
                    Some(exit_code),
                    &error.to_string(),
                )?;
                return Ok(RunOutcome {
                    operation_id: Some(failure_id),
                    exit_code,
                    captured: false,
                    capture_error: Some(error.to_string()),
                });
            }
        };
        self.storage.catalog.set_workspace(
            self.id,
            &WorkspaceCondition::Healthy,
            Some(&post_state),
        )?;
        Ok(RunOutcome {
            operation_id: Some(operation_id),
            exit_code,
            captured: true,
            capture_error: None,
        })
    }

    pub fn create_snapshot(&self, name: &str) -> Result<String> {
        let _lease = WorkspaceLease::acquire(self, false)?;
        crate::rollback::recover_locked(self)?;
        self.ensure_healthy_locked()?;
        let baseline = self.baseline_id()?;
        let scan = self.scan(None)?;
        let state_id =
            self.persist_state(StateKind::Snapshot, &scan, Some(&baseline), Some(name))?;
        self.storage.catalog.snapshot(self.id, name, &state_id)?;
        Ok(state_id)
    }

    pub fn status(&self) -> Result<WorkspaceStatus> {
        let row = self.row()?;
        let unfinished = self.storage.catalog.unfinished_transactions(self.id)?;
        Ok(WorkspaceStatus {
            id: self.id,
            root: self.root.clone(),
            condition: row.condition,
            baseline: row.baseline_state,
            unfinished_transactions: unfinished.len(),
            open_unknown: self.storage.catalog.has_open_unknown(self.id)?,
        })
    }
}

#[derive(Clone, Debug)]
pub struct WorkspaceStatus {
    pub id: Uuid,
    pub root: PathBuf,
    pub condition: WorkspaceCondition,
    pub baseline: Option<String>,
    pub unfinished_transactions: usize,
    pub open_unknown: bool,
}

pub fn state_summary(manifest: &Manifest) -> (usize, usize, usize) {
    let mut files = 0;
    let mut directories = 0;
    let mut unsupported = 0;
    for fingerprint in manifest.entries.values() {
        match fingerprint {
            Fingerprint::RegularFile { .. } | Fingerprint::Symlink { .. } => files += 1,
            Fingerprint::Directory { .. } => directories += 1,
            Fingerprint::Unsupported { .. } => unsupported += 1,
            Fingerprint::Absent => {}
        }
    }
    (files, directories, unsupported)
}

fn _journal_status_is_used(status: JournalStatus) -> bool {
    matches!(status, JournalStatus::Planned)
}

fn _staging_root(root: &Path, transaction_id: Uuid) -> Result<PathBuf> {
    staging_root(root, transaction_id)
}
