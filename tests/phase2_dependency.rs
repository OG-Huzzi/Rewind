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
