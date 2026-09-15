//! Phase 2 (dependency-aware inspection) integration tests.
//!
//! Every test in this suite is deterministic and runs without a terminal.
//! It starts with the read-only guarantees that Phase 2 planning depends on:
//! planning must never mutate the workspace or its store.

use std::fs;
use std::path::Path;

use rewind::workspace::Workspace;

/// The sorted names of the objects currently stored in the content-addressed
/// store. Used to prove that a code path did not ingest anything.
fn blob_names(cas_root: &Path) -> Vec<String> {
    let mut names: Vec<String> = match fs::read_dir(cas_root.join("blobs")) {
        Ok(entries) => entries
            .map(|entry| {
                entry
                    .expect("blob entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

#[test]
fn observe_never_ingests_objects_into_the_cas() {
    let root = tempfile::tempdir().expect("workspace root");
    let store = tempfile::tempdir().expect("store");

    // Seeded before init so that the initial capture has something to ingest.
    fs::write(root.path().join("seed.txt"), b"seed").expect("write seed file");
    let workspace = Workspace::init(root.path(), Some(store.path())).expect("init");
    let cas_root = store.path().join("cas");
    let seeded = blob_names(&cas_root);
    assert_eq!(
        seeded.len(),
        1,
        "init must ingest exactly the seeded object"
    );

    // A file written after init has never been ingested.
    fs::write(root.path().join("fresh.txt"), b"fresh").expect("write fresh file");
    let before = blob_names(&cas_root);

    // The read-only observation path: same identity, no new objects.
    let observed = workspace.observe(None).expect("observe");
    assert_eq!(
        blob_names(&cas_root),
        before,
        "observe must not write anything into the CAS"
    );
    assert!(
        observed.manifest.get("fresh.txt").content_hash().is_some(),
        "observe must still hash file contents"
    );

    // The Phase 1 capture path is unchanged: it still ingests.
    let ingested = workspace.scan(None).expect("scan");
    assert_eq!(
        observed.state_id, ingested.state_id,
        "observe and scan must agree on state identity"
    );
    assert_eq!(
        blob_names(&cas_root).len(),
        before.len() + 1,
        "scan must still ingest the new object"
    );
}

mod common;

use rewind::depgraph::{DependencyGraph, EdgeConfidence, EvidenceKind, NodeId};
use rewind::error::RewindError;
use rewind::model::WorkspaceCondition;
use rewind::plan::{
    execute, plan_rollback, BlockReason, IncludedReason, RollbackConflict, RollbackTarget,
};

#[test]
fn lineage_edges_from_real_history_are_known() {
    let root = tempfile::tempdir().expect("workspace root");
    let store = tempfile::tempdir().expect("store");
    // Scripts live outside the workspace: a script written into the workspace
    // between two commands would itself change the tree and break the state
    // chain the test is checking.
    let scratch = tempfile::tempdir().expect("scratch");
    let workspace = Workspace::init(root.path(), Some(store.path())).expect("init");

    for content in ["one", "two"] {
        let argv = common::shell_script(
            scratch.path(),
            content,
            &format!("echo {content}> {content}.txt"),
            &format!("echo {content} > {content}.txt"),
        );
        workspace.run_command(&argv).expect("supervised command");
    }

    let graph = DependencyGraph::build(&workspace).expect("build graph");
    assert!(graph.is_finished(), "build must return a normalised graph");

    let operations: Vec<&NodeId> = graph
        .nodes
        .iter()
        .filter(|node| matches!(node, NodeId::Operation { .. }))
        .collect();
    assert_eq!(operations.len(), 2, "one node per recorded operation");

    let known: Vec<_> = graph
        .edges
        .iter()
        .filter(|edge| edge.confidence == EdgeConfidence::Known)
        .collect();
    assert!(
        !known.is_empty(),
        "two chained strong captures share a state id, so a known edge must exist"
    );
    assert!(
        known
            .iter()
            .all(|edge| edge.evidence == EvidenceKind::StateLineage),
        "only state lineage may be known"
    );
    assert!(
        graph.cycles.is_empty(),
        "recorded history must not produce a cycle"
    );

    let rebuilt = DependencyGraph::build(&workspace).expect("rebuild graph");
    assert_eq!(
        graph.to_json().expect("json"),
        rebuilt.to_json().expect("json")
    );
}

// ---------------------------------------------------------------------------
// Phase 2 planning
// ---------------------------------------------------------------------------

/// Every file under `root`, as `relative-path:length`, sorted. Used to prove a
/// code path did not touch the user's workspace.
fn tree_snapshot(root: &Path) -> Vec<String> {
    fn walk(root: &Path, current: &Path, into: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(current) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                into.push(format!("{relative}/"));
                walk(root, &path, into);
            } else {
                let length = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
                into.push(format!("{relative}:{length}"));
            }
        }
    }
    let mut entries = Vec::new();
    walk(root, root, &mut entries);
    entries.sort();
    entries
}

/// Two recorded strong captures on a fresh workspace, with the script files kept
/// outside the workspace so the state chain stays unbroken.
fn two_command_workspace(
    root: &tempfile::TempDir,
    store: &tempfile::TempDir,
    scratch: &tempfile::TempDir,
) -> Workspace {
    let workspace = Workspace::init(root.path(), Some(store.path())).expect("init");
    for content in ["one", "two"] {
        let argv = common::shell_script(
            scratch.path(),
            content,
            &format!("echo {content}> {content}.txt"),
            &format!("echo {content} > {content}.txt"),
        );
        workspace.run_command(&argv).expect("supervised command");
    }
    workspace
}

fn operation_ids(workspace: &Workspace) -> Vec<i64> {
    let mut ids: Vec<i64> = workspace
        .storage
        .catalog
        .list_operations(workspace.id)
        .expect("operations")
        .into_iter()
        .map(|operation| operation.id)
        .collect();
    ids.sort_unstable();
    ids
}

#[test]
fn planning_never_touches_the_workspace_or_the_cas() {
    let root = tempfile::tempdir().expect("workspace root");
    let store = tempfile::tempdir().expect("store");
    let scratch = tempfile::tempdir().expect("scratch");
    let workspace = two_command_workspace(&root, &store, &scratch);

    let workspace_before = tree_snapshot(root.path());
    let cas_before = blob_names(&store.path().join("cas"));

    let targets = vec![RollbackTarget::undo(operation_ids(&workspace)[0])];
    let plan = plan_rollback(&workspace, &targets).expect("plan");
    assert!(
        plan.is_executable(),
        "a clean two-command history must plan: {:?}",
        plan.block_reasons
    );

    assert_eq!(
        tree_snapshot(root.path()),
        workspace_before,
        "planning must not change the user's workspace"
    );
    // Note: the SQLite catalog is opened through its WAL, which may create
    // sidecar files in the store. The CAS object set is the durable claim.
    assert_eq!(
        blob_names(&store.path().join("cas")),
        cas_before,
        "planning must not ingest objects into the CAS"
    );
}

#[test]
fn one_undo_target_plans_the_whole_newer_suffix_newest_first() {
    let root = tempfile::tempdir().expect("workspace root");
    let store = tempfile::tempdir().expect("store");
    let scratch = tempfile::tempdir().expect("scratch");
    let workspace = two_command_workspace(&root, &store, &scratch);

    let ids = operation_ids(&workspace);
    assert_eq!(ids.len(), 2);

    let plan = plan_rollback(&workspace, &[RollbackTarget::undo(ids[0])]).expect("plan");
    assert!(plan.is_executable(), "{:?}", plan.block_reasons);
    assert_eq!(
        plan.order.len(),
        2,
        "undoing the older operation must also undo the newer one"
    );
    assert_eq!(
        plan.order[0].operation_id, ids[1],
        "the newest operation is undone first"
    );
    assert_eq!(
        plan.order[0].reason,
        IncludedReason::RequiredBy {
            operation_id: ids[0]
        }
    );
    assert_eq!(plan.order[1].operation_id, ids[0]);
    assert_eq!(plan.order[1].reason, IncludedReason::Selected);
    assert!(plan.complete, "no unknown evidence on this history");
}

#[test]
fn a_plan_is_deterministic() {
    let root = tempfile::tempdir().expect("workspace root");
    let store = tempfile::tempdir().expect("store");
    let scratch = tempfile::tempdir().expect("scratch");
    let workspace = two_command_workspace(&root, &store, &scratch);

    let targets = vec![RollbackTarget::undo(operation_ids(&workspace)[0])];
    let first = plan_rollback(&workspace, &targets).expect("plan");
    let second = plan_rollback(&workspace, &targets).expect("plan");
    assert_eq!(
        first.to_json().expect("json"),
        second.to_json().expect("json")
    );
}

#[test]
fn an_open_unknown_interval_refuses_the_plan() {
    let root = tempfile::tempdir().expect("workspace root");
    let store = tempfile::tempdir().expect("store");
    let scratch = tempfile::tempdir().expect("scratch");
    let workspace = two_command_workspace(&root, &store, &scratch);

    let baseline = workspace.baseline_id().expect("baseline");
    workspace
        .record_capture_failure(Some(&baseline), None, None, None, "test gap")
        .expect("record gap");

    let targets = vec![RollbackTarget::undo(operation_ids(&workspace)[0])];
    let plan = plan_rollback(&workspace, &targets).expect("plan");
    assert!(
        !plan.is_executable(),
        "an open unknown interval must refuse"
    );
    assert!(
        plan.block_reasons
            .iter()
            .any(|reason| matches!(reason, BlockReason::UnknownInterval { .. })),
        "the plan must name the unknown interval: {:?}",
        plan.block_reasons
    );
}

#[test]
fn a_refused_plan_executes_nothing() {
    let root = tempfile::tempdir().expect("workspace root");
    let store = tempfile::tempdir().expect("store");
    let scratch = tempfile::tempdir().expect("scratch");
    let workspace = two_command_workspace(&root, &store, &scratch);

    let baseline = workspace.baseline_id().expect("baseline");
    workspace
        .record_capture_failure(Some(&baseline), None, None, None, "test gap")
        .expect("record gap");

    let targets = vec![RollbackTarget::undo(operation_ids(&workspace)[0])];
    let plan = plan_rollback(&workspace, &targets).expect("plan");
    assert!(!plan.is_executable());

    let before = tree_snapshot(root.path());
    let error = execute(&workspace, &plan).expect_err("refused plan must not execute");
    assert!(matches!(error, RewindError::ConditionBlocked(_)));
    assert_eq!(
        tree_snapshot(root.path()),
        before,
        "a refused plan must not touch the workspace"
    );
    assert!(root.path().join("one.txt").exists());
    assert!(root.path().join("two.txt").exists());
}

#[test]
fn an_approved_plan_executes_through_the_phase_1_engine() {
    let root = tempfile::tempdir().expect("workspace root");
    let store = tempfile::tempdir().expect("store");
    let scratch = tempfile::tempdir().expect("scratch");
    let workspace = two_command_workspace(&root, &store, &scratch);

    let ids = operation_ids(&workspace);
    let plan = plan_rollback(&workspace, &[RollbackTarget::undo(ids[1])]).expect("plan");
    assert!(plan.is_executable(), "{:?}", plan.block_reasons);

    let outcomes = execute(&workspace, &plan).expect("execute");
    assert_eq!(outcomes.len(), 1);
    assert_eq!(
        outcomes[0].operation_id,
        Some(ids[1]),
        "the Phase 1 engine records the transaction it committed"
    );
    assert!(
        !root.path().join("two.txt").exists(),
        "undo restores the pre-state of the selected operation"
    );
    assert!(
        root.path().join("one.txt").exists(),
        "unrelated work survives"
    );
    assert_eq!(
        workspace.condition().expect("condition"),
        WorkspaceCondition::Healthy,
        "the workspace returns to HEALTHY through the Phase 1 commit path"
    );
}

#[test]
fn a_stale_plan_is_refused() {
    let root = tempfile::tempdir().expect("workspace root");
    let store = tempfile::tempdir().expect("store");
    let scratch = tempfile::tempdir().expect("scratch");
    let workspace = two_command_workspace(&root, &store, &scratch);

    let ids = operation_ids(&workspace);
    let plan = plan_rollback(&workspace, &[RollbackTarget::undo(ids[1])]).expect("plan");
    assert!(plan.is_executable());

    // History moves on after the plan was built.
    let argv = common::shell_script(
        scratch.path(),
        "three",
        "echo three> three.txt",
        "echo three > three.txt",
    );
    workspace.run_command(&argv).expect("supervised command");

    let error = execute(&workspace, &plan).expect_err("a stale plan must be refused");
    assert!(matches!(error, RewindError::ConditionBlocked(_)));
    assert!(root.path().join("three.txt").exists());
}

#[test]
fn an_already_undone_operation_is_not_an_eligible_target() {
    let root = tempfile::tempdir().expect("workspace root");
    let store = tempfile::tempdir().expect("store");
    let scratch = tempfile::tempdir().expect("scratch");
    let workspace = two_command_workspace(&root, &store, &scratch);

    let ids = operation_ids(&workspace);
    rewind::rollback::undo(&workspace, Some(ids[1]), false).expect("first undo");

    let plan = plan_rollback(&workspace, &[RollbackTarget::undo(ids[1])]).expect("plan");
    assert!(!plan.is_executable());
    assert!(
        plan.conflicts.iter().any(|conflict| matches!(
            conflict,
            RollbackConflict::TargetNotEligible { operation_id, .. } if *operation_id == ids[1]
        )),
        "the plan must say why the target is not eligible: {:?}",
        plan.conflicts
    );
}

// ---------------------------------------------------------------------------
// Phase 2 interactive surface (presentation only)
// ---------------------------------------------------------------------------

#[test]
fn the_interactive_surface_renders_and_refuses_mutation() {
    let root = tempfile::tempdir().expect("workspace root");
    let store = tempfile::tempdir().expect("store");
    let scratch = tempfile::tempdir().expect("scratch");
    let workspace = two_command_workspace(&root, &store, &scratch);
    let ids = operation_ids(&workspace);

    let before = tree_snapshot(root.path());

    let plan_action = rewind::ui::parse(&format!("plan undo {}", ids[0]));
    let text = rewind::ui::render(&workspace, &plan_action).expect("render plan");
    assert!(text.contains("decision EXECUTABLE"), "{text}");
    assert!(text.contains("nothing was executed"), "{text}");

    let refused = rewind::ui::parse("apply");
    let text = rewind::ui::render(&workspace, &refused).expect("render refusal");
    assert!(text.contains("refused: apply"), "{text}");

    assert_eq!(
        tree_snapshot(root.path()),
        before,
        "rendering must not touch the workspace"
    );
}

#[test]
fn the_interactive_surface_answers_over_stdin_and_executes_nothing() {
    use std::io::Write;

    let root = tempfile::tempdir().expect("workspace root");
    let store = tempfile::tempdir().expect("store");
    let scratch = tempfile::tempdir().expect("scratch");
    let workspace = two_command_workspace(&root, &store, &scratch);
    let ids = operation_ids(&workspace);
    let before = tree_snapshot(root.path());

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_rewind"))
        .arg("ui")
        .current_dir(root.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn the interactive surface");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        writeln!(stdin, "history").expect("write");
        writeln!(stdin, "plan undo {}", ids[0]).expect("write");
        writeln!(stdin, "apply").expect("write");
        writeln!(stdin, "frobnicate").expect("write");
        writeln!(stdin, "quit").expect("write");
    }
    let output = child.wait_with_output().expect("output");
    let text = String::from_utf8_lossy(&output.stdout);

    assert!(text.contains("STRONG"), "history renders: {text}");
    assert!(
        text.contains("decision EXECUTABLE"),
        "the plan renders: {text}"
    );
    assert!(
        text.contains("refused: apply"),
        "mutation is refused: {text}"
    );
    assert!(
        text.contains("invalid: frobnicate"),
        "junk is reported: {text}"
    );
    assert_eq!(
        tree_snapshot(root.path()),
        before,
        "the surface must never execute anything"
    );
}

#[test]
fn a_capture_failed_operation_in_the_closure_blocks_the_plan() {
    let root = tempfile::tempdir().expect("workspace root");
    let store = tempfile::tempdir().expect("store");
    let scratch = tempfile::tempdir().expect("scratch");
    let workspace = two_command_workspace(&root, &store, &scratch);

    let baseline = workspace.baseline_id().expect("baseline");
    workspace
        .record_capture_failure(Some(&baseline), None, None, None, "test gap")
        .expect("record gap");

    let ids = operation_ids(&workspace);
    let plan = plan_rollback(&workspace, &[RollbackTarget::undo(ids[1])]).expect("plan");
    assert!(!plan.is_executable());
    assert!(
        plan.block_reasons
            .iter()
            .any(|reason| matches!(reason, BlockReason::NotReversible { .. })),
        "a CAPTURE_FAILED operation cannot be undone and must block: {:?}",
        plan.block_reasons
    );
    assert!(
        plan.unknowns
            .iter()
            .any(|unknown| unknown.evidence == EvidenceKind::EffectOverlap),
        "a kind that records no effects must be reported as missing evidence"
    );
}

#[test]
fn duplicate_targets_collapse_to_one_plan() {
    let root = tempfile::tempdir().expect("workspace root");
    let store = tempfile::tempdir().expect("store");
    let scratch = tempfile::tempdir().expect("scratch");
    let workspace = two_command_workspace(&root, &store, &scratch);

    let ids = operation_ids(&workspace);
    let once = plan_rollback(&workspace, &[RollbackTarget::undo(ids[0])]).expect("plan");
    let twice = plan_rollback(
        &workspace,
        &[
            RollbackTarget::undo(ids[0]),
            RollbackTarget::undo(ids[0]),
            RollbackTarget::undo(ids[0]),
        ],
    )
    .expect("plan");
    assert_eq!(once.order, twice.order, "duplicate targets are one target");
    assert_eq!(twice.targets.len(), 1);
    assert_eq!(
        once.to_json().expect("json"),
        twice.to_json().expect("json")
    );
}

#[test]
fn a_data_only_workspace_plans_nothing_and_errors_on_no_targets() {
    let root = tempfile::tempdir().expect("workspace root");
    let store = tempfile::tempdir().expect("store");
    let workspace = Workspace::init(root.path(), Some(store.path())).expect("init");

    // Empty history: the graph is empty and no operation can be planned.
    let graph = DependencyGraph::build(&workspace).expect("graph");
    assert!(graph.edges.is_empty());
    assert!(graph.cycles.is_empty());

    let error = plan_rollback(&workspace, &[]).expect_err("no targets must be an error");
    assert!(matches!(error, RewindError::InvalidCommand(_)));
}
