use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};

use crate::db::OperationDraft;
use crate::error::{Result, RewindError};
use crate::model::{
    OperationKind, OperationStatus, Reversibility, TrackingConfidence, WorkspaceCondition,
};
use crate::rollback::{redo, restore_snapshot, undo};
use crate::workspace::{state_summary, Workspace, WorkspaceLease};

/// Passive-hook bookkeeping scan budget (Phase 0.7 observation model): the
/// hook's scan is bounded by this deadline, counted from the moment the
/// scan is about to run (not from process start — workspace open and lease
/// work happen first and must not consume the scan's budget). A scan that
/// cannot finish records a durable capture gap instead of a fabricated
/// observation. The shell-facing guarantee is different and stronger
/// (Phase 1.3): the shell integration launches `rewind hook post` in the
/// background, so the interactive shell never waits for bookkeeping at
/// all. None of these constants is a promise about the hook process's
/// total wall time.
const HOOK_SCAN_DEADLINE_MS: u64 = 50;

/// How long a background post-hook waits for a lease held by another hook
/// or writer before falling back to the durable bypass marker. Rapid
/// typing makes consecutive background hooks overlap; the holder releases
/// within milliseconds, so a short retry avoids spurious reconciliation
/// gates while still failing closed against genuinely long writers.
const HOOK_LEASE_RETRY: Duration = Duration::from_millis(2000);

#[derive(Debug, Parser)]
#[command(
    name = "rewind",
    version,
    about = "Conservative local workspace recovery"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Init(InitArgs),
    Status,
    Reconcile,
    Recover {
        /// Abandon unclassifiable unfinished transactions after archiving
        /// their artifacts, then reconcile the live state into a new trusted
        /// checkpoint. Automatic completion is always attempted first.
        #[arg(long)]
        reconcile: bool,
    },
    Run {
        // `last` cannot be combined with `trailing_var_arg` (clap panics on
        // that combination in debug builds); `trailing_var_arg` alone keeps
        // `rewind run -- <command...>` and `rewind run <command...>` working.
        #[arg(trailing_var_arg = true)]
        command: Vec<String>,
    },
    Undo {
        operation_id: Option<i64>,
        #[arg(long)]
        force: bool,
    },
    Redo {
        operation_id: Option<i64>,
    },
    List,
    Show {
        operation_id: i64,
    },
    Diff {
        operation_id: i64,
    },
    Snapshot {
        name: String,
    },
    Restore {
        name: String,
    },
    Doctor,
    Hook {
        #[command(subcommand)]
        command: HookCommand,
    },
}

#[derive(Debug, Args)]
pub struct InitArgs {
    #[arg(default_value = ".")]
    pub path: PathBuf,
    #[arg(long)]
    pub store: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum HookCommand {
    Pre {
        #[arg(long)]
        command: String,
        #[arg(long)]
        session: Option<String>,
    },
    Post {
        /// Immutable boundary identity returned by `rewind hook pre` on
        /// stdout. Required: a post-hook never guesses which boundary it
        /// belongs to (Phase 1.4).
        #[arg(long)]
        boundary: String,
        #[arg(long)]
        exit_code: i32,
    },
}

pub fn run(cli: Cli) -> Result<i32> {
    match cli.command {
        Command::Init(args) => {
            let workspace = Workspace::init(&args.path, args.store.as_deref())?;
            println!("initialized workspace {}", workspace.id);
            println!("root {}", workspace.root.display());
            println!("condition {}", workspace.condition()?);
            Ok(0)
        }
        Command::Status => {
            let workspace = open_workspace()?;
            let status = workspace.status()?;
            println!("workspace {}", status.id);
            println!("root {}", status.root.display());
            println!("condition {}", status.condition);
            println!(
                "baseline {}",
                status.baseline.unwrap_or_else(|| "NONE".to_owned())
            );
            println!("open_unknown {}", status.open_unknown);
            println!("unfinished_transactions {}", status.unfinished_transactions);
            Ok(0)
        }
        Command::Reconcile => {
            let workspace = open_workspace()?;
            let _lease = WorkspaceLease::acquire(&workspace, false)?;
            crate::rollback::recover_locked(&workspace)?;
            let state = workspace.reconcile_locked("explicit reconciliation")?;
            println!("reconciled {state}");
            Ok(0)
        }
        Command::Recover { reconcile } => {
            let workspace = open_workspace()?;
            let _lease = WorkspaceLease::acquire(&workspace, false)?;
            if reconcile {
                let state = crate::rollback::recover_reconcile(&workspace)?;
                println!("recovered {state}");
                return Ok(0);
            }
            let unfinished = workspace.storage.journals.unfinished()?;
            if unfinished.is_empty() {
                let condition = workspace.condition()?;
                if matches!(
                    condition,
                    WorkspaceCondition::RecoveryRequired | WorkspaceCondition::Degraded
                ) {
                    println!("no unfinished transaction; run rewind reconcile");
                    return Ok(3);
                }
                println!("no unfinished transaction; workspace is {condition}");
                return Ok(0);
            }
            match crate::rollback::recover_locked(&workspace) {
                Ok(()) => {
                    println!("recovery complete; unfinished transactions resolved");
                    Ok(0)
                }
                Err(error) => {
                    eprintln!("rewind: automatic recovery failed: {error}");
                    for line in crate::rollback::diagnose_recoverable(&workspace)? {
                        println!("{line}");
                    }
                    eprintln!(
                        "rewind: workspace remains RECOVERY_REQUIRED; inspect the \
                         classification above, then run `rewind recover --reconcile` \
                         to archive the transaction artifacts, abandon the \
                         transaction, and reconcile the live state"
                    );
                    Ok(3)
                }
            }
        }
        Command::Run { command } => {
            let workspace = open_workspace()?;
            let outcome = workspace.run_command(&command)?;
            if let Some(error) = &outcome.capture_error {
                eprintln!("capture failed; reconciliation required: {error}");
            }
            if let Some(operation_id) = outcome.operation_id {
                println!(
                    "{} operation {}",
                    if outcome.captured {
                        "captured"
                    } else {
                        "recorded"
                    },
                    operation_id
                );
            }
            if outcome.capture_error.is_some() && outcome.exit_code == 0 {
                Ok(4)
            } else {
                Ok(outcome.exit_code)
            }
        }
        Command::Undo {
            operation_id,
            force,
        } => {
            let workspace = open_workspace()?;
            let outcome = undo(&workspace, operation_id, force)?;
            println!(
                "undo committed transaction {} at state {}",
                outcome.transaction_id, outcome.target_state_id
            );
            Ok(0)
        }
        Command::Redo { operation_id } => {
            let workspace = open_workspace()?;
            let outcome = redo(&workspace, operation_id)?;
            println!(
                "redo committed transaction {} at state {}",
                outcome.transaction_id, outcome.target_state_id
            );
            Ok(0)
        }
        Command::List => {
            let workspace = open_workspace()?;
            println!("condition {}", workspace.condition()?);
            for operation in workspace.storage.catalog.list_operations(workspace.id)? {
                println!(
                    "#{id} {kind} {status} {confidence} {reversibility} {command}",
                    id = operation.id,
                    kind = operation.kind.as_str(),
                    status = operation.status.as_str(),
                    confidence = operation.confidence.as_str(),
                    reversibility = operation.reversibility.as_str(),
                    command = operation.command.unwrap_or_default()
                );
            }
            Ok(0)
        }
        Command::Show { operation_id } => {
            let workspace = open_workspace()?;
            let operation = workspace
                .storage
                .catalog
                .operation(operation_id, workspace.id)?;
            println!("{}", serde_json::to_string_pretty(&operation)?);
            Ok(0)
        }
        Command::Diff { operation_id } => {
            let workspace = open_workspace()?;
            let operation = workspace
                .storage
                .catalog
                .operation(operation_id, workspace.id)?;
            for effect in operation.effects {
                println!(
                    "{} {} {} -> {}",
                    effect.effect_type.as_str(),
                    effect.path,
                    effect.pre.describe(),
                    effect.post.describe()
                );
            }
            Ok(0)
        }
        Command::Snapshot { name } => {
            let workspace = open_workspace()?;
            let state = workspace.create_snapshot(&name)?;
            println!("snapshot {name} {state}");
            Ok(0)
        }
        Command::Restore { name } => {
            let workspace = open_workspace()?;
            let outcome = restore_snapshot(&workspace, &name)?;
            println!(
                "restore committed transaction {} at state {}",
                outcome.transaction_id, outcome.target_state_id
            );
            Ok(0)
        }
        Command::Doctor => doctor(),
        Command::Hook { command } => passive_hook(command),
    }
}

fn open_workspace() -> Result<Workspace> {
    Workspace::open_from_current(&env::current_dir()?)
}

fn doctor() -> Result<i32> {
    let workspace = open_workspace()?;
    workspace.enforce_pending_safety_gate()?;
    let status = workspace.status()?;
    let mut result = 0;
    println!("workspace {}", status.id);
    println!("condition {}", status.condition);
    let integrity = workspace.storage.catalog.integrity_check()?;
    println!("catalog {integrity}");
    if integrity != "ok" {
        result = 3;
    }
    match WorkspaceLease::acquire(&workspace, true) {
        Ok(lease) => drop(lease),
        Err(error) => {
            println!("writer_lock unavailable: {error}");
            result = 3;
        }
    }
    if let Some(baseline) = status.baseline {
        let state = workspace.storage.catalog.state(&baseline)?;
        let (files, directories, unsupported) = state_summary(&state.manifest);
        println!(
            "baseline {} files={} directories={} unsupported={}",
            baseline, files, directories, unsupported
        );
        for fingerprint in state.manifest.entries.values() {
            if let crate::model::Fingerprint::RegularFile { content_hash, .. } = fingerprint {
                workspace.storage.cas.verify(content_hash)?;
            }
        }
        let live = workspace.scan(None)?;
        if live.state_id != baseline {
            println!(
                "baseline_drift expected={} observed={}",
                baseline, live.state_id
            );
            result = 3;
        }
    }
    for (id, state, journal) in workspace
        .storage
        .catalog
        .unfinished_transactions(workspace.id)?
    {
        println!("unfinished transaction {id} {state} {journal}");
        result = 3;
    }
    for (_path, journal) in workspace.storage.journals.pending_archives()? {
        println!(
            "archive_pending transaction={} status={:?} error={}",
            journal.transaction_id,
            journal.archive_status,
            journal.archive_error.unwrap_or_default()
        );
    }
    if status.condition != WorkspaceCondition::Healthy {
        match status.condition {
            WorkspaceCondition::RecoveryRequired => {
                println!(
                    "action: run `rewind recover` to classify the unfinished \
                     transaction; if it cannot be completed safely, run \
                     `rewind recover --reconcile`"
                );
            }
            _ => {
                println!("action: run rewind reconcile before normal capture or rollback");
            }
        }
        result = 3;
    }
    println!(
        "platform {}",
        if cfg!(windows) {
            "windows"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux-or-posix"
        }
    );
    Ok(result)
}

fn passive_hook(command: HookCommand) -> Result<i32> {
    let result = match command {
        HookCommand::Pre { command, session } => hook_pre(&command, session.as_deref()),
        HookCommand::Post {
            boundary,
            exit_code,
        } => hook_post(&boundary, exit_code),
    };
    match result {
        Ok(code) => Ok(code),
        Err(error) => {
            eprintln!("rewind hook diagnostic: {error}");
            Ok(0)
        }
    }
}

fn session_id(value: Option<&str>) -> String {
    value
        .map(str::to_owned)
        .or_else(|| env::var("REWIND_SESSION_ID").ok())
        .unwrap_or_else(|| format!("shell-{}", std::process::id()))
}

/// Pre-command boundary. Deliberately synchronous and extremely
/// lightweight: marker discovery, catalog open, and a single INSERT —
/// never a scan, recovery, CAS, or archive work. A failed pre-hook is
/// represented safely by absence: no boundary exists, so no observation
/// can be fabricated for the interval, and the next writer's
/// reconcile-first pre-scan catches any live drift.
///
/// The boundary's immutable id is printed as a single line on stdout; that
/// id is the only thing the shell may use to correlate this command's
/// background post-hook with this boundary (Phase 1.4). Diagnostics go to
/// stderr so they can never contaminate the captured id.
fn hook_pre(command: &str, session: Option<&str>) -> Result<i32> {
    let workspace = open_workspace()?;
    let boundary_id = workspace.storage.catalog.add_boundary(
        workspace.id,
        &session_id(session),
        command,
        &workspace.root.to_string_lossy(),
    )?;
    println!("{boundary_id}");
    Ok(0)
}

/// Post-command bookkeeping for exactly one boundary identity (Phase 1.4).
///
/// The id comes from the shell integration, which captured it from the
/// matching pre-hook; this function never searches for a boundary. Any
/// uncertainty is resolved conservatively:
///
/// - unknown id, id from another workspace, or an id another post-hook
///   already accounted for -> diagnostic on stderr, exit 0, and *no*
///   bookkeeping and *no* side effects (never consume another boundary,
///   never fabricate an observation, never gate the workspace for a
///   duplicate that changed nothing);
/// - otherwise the boundary is claimed exactly once, before any further
///   work, so no two hooks can ever account for the same interval;
/// - everything after the claim keeps the Phase 1.3 degradation model
///   (bypass marker / CAPTURE_FAILED / unknown interval), never a
///   fabricated operation.
///
/// Shell-facing responsiveness lives in the shell integration, which
/// launches this command in the background: the interactive shell never
/// waits for bookkeeping, so this process may take as long as the work
/// legitimately needs. The only in-process bound is the bookkeeping scan
/// deadline (HOOK_SCAN_DEADLINE_MS), which starts when the scan is about to
/// run. Deferred transaction recovery is deliberately not run here
/// (Phase 1.3 §9; enforced in hook_post_locked).
fn hook_post(boundary_id: &str, exit_code: i32) -> Result<i32> {
    let workspace = open_workspace()?;
    let Some(boundary) = workspace
        .storage
        .catalog
        .boundary(workspace.id, boundary_id)?
    else {
        eprintln!(
            "rewind hook diagnostic: passive boundary {boundary_id} is not \
             present in this workspace (unknown id or another workspace); \
             no bookkeeping performed"
        );
        return Ok(0);
    };
    if !workspace
        .storage
        .catalog
        .finish_boundary(workspace.id, boundary_id, exit_code)?
    {
        eprintln!(
            "rewind hook diagnostic: passive boundary {boundary_id} was \
             already accounted for; no bookkeeping performed"
        );
        return Ok(0);
    }
    // Overlapping background hooks (rapid typing) serialize on the lease;
    // the holder releases within milliseconds, so wait briefly before the
    // conservative bypass fallback. A writer mid-rollback holds the lease
    // far longer than this and still ends up gated — by design.
    let lease = match lease_with_retry(&workspace)? {
        Some(lease) => lease,
        None => {
            append_bypass_marker(&workspace, &boundary.id)?;
            return Ok(0);
        }
    };
    // The scan deadline is started inside hook_post_locked, immediately
    // before the scan runs (see its comment).
    let result = hook_post_locked(&workspace, &boundary, exit_code);
    drop(lease);
    match result {
        Ok(code) => Ok(code),
        Err(RewindError::ScanIncomplete(reason)) if reason.contains("deadline") => {
            // The bounded scan did not finish; record the capture gap
            // durably instead of fabricating an observation.
            let baseline = workspace.baseline_id().ok();
            let _ = workspace.record_capture_failure(
                baseline.as_deref(),
                Some(boundary.command.clone()),
                Some(boundary.cwd.clone()),
                Some(exit_code),
                &reason,
            );
            Ok(0)
        }
        Err(error) => {
            // Any other failure (storage, CAS, catalog) must not silently
            // drop the boundary: leave a durable bypass marker so the next
            // writer reconciles rather than trusting the interval. If even
            // the marker fails, the writer's reconcile-first pre-scan still
            // catches live drift.
            let _ = append_bypass_marker(&workspace, &boundary.id);
            Err(error)
        }
    }
}

/// Retries a nonblocking lease acquisition for `HOOK_LEASE_RETRY` before
/// giving up. Returns `None` when the lease stays unavailable (the caller
/// then records the durable bypass marker); errors other than
/// "lease unavailable" propagate.
fn lease_with_retry(workspace: &Workspace) -> Result<Option<WorkspaceLease>> {
    let started = Instant::now();
    loop {
        match WorkspaceLease::acquire(workspace, true) {
            Ok(lease) => return Ok(Some(lease)),
            Err(RewindError::LockUnavailable(_)) => {}
            Err(error) => return Err(error),
        }
        if started.elapsed() >= HOOK_LEASE_RETRY {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn hook_post_locked(
    workspace: &Workspace,
    boundary: &crate::db::BoundaryRow,
    exit_code: i32,
) -> Result<i32> {
    // The boundary was already claimed exactly once by hook_post, before
    // this locked section: this function only decides how the claimed
    // interval is represented. Every outcome below keeps the Phase 1.3
    // model -- observation (HEALTHY, complete scan), gated BoundaryOnly, or
    // a durable bypass marker that forces writer reconciliation. A
    // fabricated operation is never an outcome.
    //
    // Deferred transaction recovery is intentionally omitted from the hook
    // path: a writer that needs recovery owns the workspace and the hook
    // must not pay its cost. If recovery is pending, the hook records a
    // bypass marker and returns immediately.
    if !workspace
        .storage
        .catalog
        .unfinished_transactions(workspace.id)?
        .is_empty()
    {
        append_bypass_marker(workspace, &boundary.id)?;
        return Ok(0);
    }
    workspace.enforce_pending_safety_gate()?;
    if workspace.condition()? != WorkspaceCondition::Healthy {
        let baseline = workspace.row()?.baseline_state;
        workspace.storage.catalog.insert_operation(
            workspace.id,
            &OperationDraft {
                kind: OperationKind::BoundaryOnly,
                status: OperationStatus::Untrusted,
                pre_state_id: baseline,
                post_state_id: None,
                command: Some(boundary.command.clone()),
                cwd: Some(boundary.cwd.clone()),
                exit_code: Some(exit_code),
                confidence: TrackingConfidence::Degraded,
                reversibility: Reversibility::Unavailable,
                error: Some("workspace requires reconciliation".to_owned()),
                effects: Vec::new(),
            },
        )?;
        return Ok(0);
    }
    let baseline = workspace.baseline_id()?;
    // The scan deadline bounds the scan itself and starts when the scan is
    // about to run (Phase 1.3 contract, made exact): the workspace open,
    // the lease work, and the gate/baseline reads above are not scan work
    // and must not consume the scan's budget. A scan that cannot finish
    // inside the budget records a durable capture gap instead of a
    // fabricated observation.
    let scan_deadline = Instant::now() + Duration::from_millis(HOOK_SCAN_DEADLINE_MS);
    let scan = match workspace.scan(Some(scan_deadline)) {
        Ok(scan) => scan,
        Err(error) => {
            workspace.record_capture_failure(
                Some(&baseline),
                Some(boundary.command.clone()),
                Some(boundary.cwd.clone()),
                Some(exit_code),
                &error.to_string(),
            )?;
            return Ok(0);
        }
    };
    let post_state = workspace.persist_state(
        crate::model::StateKind::Observation,
        &scan,
        Some(&baseline),
        Some("passive-observation"),
    )?;
    let before = workspace.state_manifest(&baseline)?;
    let effects = crate::scan::diff_manifests(&before, &scan.manifest);
    workspace.storage.catalog.insert_operation(
        workspace.id,
        &OperationDraft {
            kind: OperationKind::PassiveObservation,
            status: OperationStatus::Completed,
            pre_state_id: Some(baseline),
            post_state_id: Some(post_state.clone()),
            command: Some(boundary.command.clone()),
            cwd: Some(boundary.cwd.clone()),
            exit_code: Some(exit_code),
            confidence: TrackingConfidence::LowConfidenceObservation,
            reversibility: Reversibility::Unavailable,
            error: None,
            effects,
        },
    )?;
    workspace.storage.catalog.set_workspace(
        workspace.id,
        &WorkspaceCondition::Healthy,
        Some(&post_state),
    )?;
    Ok(0)
}

fn append_bypass_marker(workspace: &Workspace, boundary_id: &str) -> Result<()> {
    let path = workspace.storage.project_root.join("boundaries.log");
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(
        file,
        "{{\"boundary\":\"{}\",\"kind\":\"BYPASS_PENDING\",\"pid\":{}}}",
        boundary_id,
        std::process::id()
    )?;
    file.sync_all()?;
    if let Err(error) = workspace
        .storage
        .catalog
        .add_bypass_marker(workspace.id, boundary_id)
    {
        eprintln!("rewind hook diagnostic: durable database bypass marker unavailable: {error}");
    }
    Ok(())
}
