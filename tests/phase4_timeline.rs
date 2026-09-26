//! Phase 4 verification: the time-range history view
//! (`.ai/PHASE_4_TIME_AND_ECOSYSTEM.md` §7 acceptance criteria).
//!
//! The timeline is read-only by contract, so these tests prove the
//! stronger property instead of trusting the code: the catalog and the
//! workspace are byte-identical across timeline invocations, uncertainty is
//! exposed rather than filled, and validation failures mutate nothing.

use std::fs;
use std::process::Command;

use rewind::model::WorkspaceCondition;
use rewind::timeline::TIMELINE_SCHEMA_VERSION;
use rewind::watch::model::{DegradationReason, DegradationRecord, EventKind, EventSource, FsEvent};
use rewind::watch::run::WatchPaths;
use rewind::workspace::Workspace;

mod common;

use serde_json::Value;

/// A real initialized workspace with an external store.
struct Fixture {
    root: tempfile::TempDir,
    _store: tempfile::TempDir,
    workspace: Workspace,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("temporary root");
    fs::write(root.path().join("foo.txt"), b"A").expect("write fixture");
    let store = tempfile::tempdir().expect("temporary store");
    let workspace = Workspace::init(root.path(), Some(store.path())).expect("initialize");
    Fixture {
        root,
        _store: store,
        workspace,
    }
}

fn run_cli(fixture: &Fixture, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_rewind"))
        .args(args)
        .current_dir(fixture.root.path())
        .output()
        .expect("spawn rewind");
    (
        output.status.code().unwrap_or(1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Forces the WAL into the main catalog file so byte comparisons across
/// process boundaries are meaningful: the CLI legitimately checkpoints a
/// pending WAL when it opens the database, which must not count as a
/// mutation.
fn checkpoint_wal(fixture: &Fixture) {
    let connection =
        rusqlite::Connection::open(fixture.workspace.storage.catalog.path()).expect("open catalog");
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .expect("checkpoint");
}

fn catalog_bytes(fixture: &Fixture) -> Vec<u8> {
    fs::read(fixture.workspace.storage.catalog.path()).expect("catalog bytes")
}

fn workspace_fingerprint(fixture: &Fixture) -> Vec<(String, u64)> {
    fn walk(dir: &std::path::Path, base: &std::path::Path, out: &mut Vec<(String, u64)>) {
        for entry in fs::read_dir(dir).expect("read dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                out.push((
                    path.strip_prefix(base)
                        .expect("inside root")
                        .to_string_lossy()
                        .replace('\\', "/"),
                    fs::metadata(&path).expect("meta").len(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(fixture.root.path(), fixture.root.path(), &mut out);
    out.sort();
    out
}

fn parse_json(stdout: &str) -> Value {
    serde_json::from_str(stdout).expect("timeline JSON")
}

/// A platform-native captured run, mirroring the foundation suite's vector.
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

/// AC1: a fresh workspace has an empty, complete timeline, end to end
/// through the real CLI (a separate process, so persistence is proven too).
#[test]
fn empty_history_produces_a_complete_empty_timeline() {
    let fixture = fixture();
    let (code, stdout, stderr) = run_cli(&fixture, &["inspect", "timeline", "--json"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let report = parse_json(&stdout);
    assert_eq!(
        report["schema_version"],
        serde_json::json!(TIMELINE_SCHEMA_VERSION)
    );
    assert_eq!(report["entries"].as_array().expect("entries").len(), 0);
    assert_eq!(report["history_complete"], true);
    assert!(report["since_text"].as_str().expect("since").ends_with('Z'));

    let human = run_cli(&fixture, &["inspect", "timeline"]);
    assert_eq!(human.0, 0);
    assert!(human.1.contains("no recorded entries in this range"));
    assert!(human.1.contains("history_complete=true"));
}

/// AC10 + AC2: a real captured operation appears in a range that contains
/// it; since is inclusive, until is exclusive.
#[test]
fn captured_operations_respect_the_half_open_range() {
    let fixture = fixture();
    let outcome = fixture
        .workspace
        .run_command(&run_echo("foo.txt", "B"))
        .expect("run command");
    assert!(outcome.captured);
    let operations = fixture
        .workspace
        .storage
        .catalog
        .list_operations(fixture.workspace.id)
        .expect("operations");
    assert_eq!(operations.len(), 1);
    let created_at = operations[0].created_at;

    let iso_at = rewind::humantime::format_rfc3339(created_at);
    let after = rewind::humantime::format_rfc3339(created_at + 1);

    // since == record time (inclusive): the operation appears.
    let (code, stdout, stderr) = run_cli(
        &fixture,
        &[
            "inspect", "timeline", "--json", "--since", &iso_at, "--until", &after,
        ],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let report = parse_json(&stdout);
    let entries = report["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["tier"], "OPERATION");
    assert!(entries[0]["identifier"]
        .as_str()
        .expect("identifier")
        .starts_with("operation "));

    // until == record time (exclusive): the same instant is excluded.
    let before = rewind::humantime::format_rfc3339(created_at - 1);
    let (code, stdout, _) = run_cli(
        &fixture,
        &[
            "inspect", "timeline", "--json", "--since", &before, "--until", &iso_at,
        ],
    );
    assert_eq!(code, 0);
    assert_eq!(
        parse_json(&stdout)["entries"]
            .as_array()
            .expect("entries")
            .len(),
        0,
        "until is exclusive: the record at `until` must not appear"
    );
}

/// AC3: identical inputs produce byte-identical JSON across invocations.
#[test]
fn json_output_is_byte_identical_for_identical_inputs() {
    let fixture = fixture();
    let args = vec![
        "inspect",
        "timeline",
        "--json",
        "--since",
        "2026-01-01T00:00:00Z",
        "--until",
        "2027-01-01T00:00:00Z",
    ];
    let first = run_cli(&fixture, &args);
    let second = run_cli(&fixture, &args);
    assert_eq!(first.0, 0);
    assert_eq!(first.0, second.0);
    assert_eq!(first.1, second.1, "same store, same range, same bytes");
}

/// AC4 + AC6 + AC7: an open unknown interval and a watcher degradation
/// inside the range make the timeline explicitly incomplete; both are
/// reported without being closed, consumed, or otherwise mutated; the
/// catalog is byte-identical across the invocation.
#[test]
fn uncertainty_is_exposed_and_the_view_is_read_only() {
    let fixture = fixture();
    let paths = WatchPaths::new(&fixture.workspace.storage.project_root);

    // A durable watcher degradation at a known time.
    let degradation_time =
        rewind::humantime::parse_rfc3339("2026-06-01T00:00:00Z").expect("degradation time");
    let record = DegradationRecord {
        reason: DegradationReason::Overflow,
        detail: "platform watcher queue overflow; events may have been lost".to_owned(),
        recorded_at: degradation_time,
        gap_from: None,
        gap_to: None,
    };
    rewind::watch::run::append_degradation(&paths, fixture.workspace.id, record)
        .expect("append degradation");

    // A pending unknown interval (created now by the real catalog API).
    fixture
        .workspace
        .storage
        .catalog
        .open_unknown(fixture.workspace.id, None, "test uncertainty")
        .expect("open unknown");

    checkpoint_wal(&fixture);
    let before_catalog = catalog_bytes(&fixture);
    let before_tree = workspace_fingerprint(&fixture);

    // Explicit bounds: the fabricated degradation (2026-06-01) predates the
    // workspace itself, so the default range (creation..=now) must NOT reach
    // it — the view only ever reports what falls inside the asked-for range.
    let (code, stdout, stderr) = run_cli(
        &fixture,
        &[
            "inspect",
            "timeline",
            "--json",
            "--since",
            "2026-05-31T00:00:00Z",
            "--until",
            "2027-01-01T00:00:00Z",
        ],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let report = parse_json(&stdout);
    assert_eq!(report["history_complete"], false);

    let tiers: Vec<&str> = report["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| entry["tier"].as_str().expect("tier"))
        .collect();
    assert!(tiers.contains(&"UNKNOWN_INTERVAL"), "{tiers:?}");
    assert!(tiers.contains(&"WATCH"), "{tiers:?}");

    // AC7: read-only — catalog, workspace, and the degradation marker are
    // untouched, and the workspace condition did not move.
    assert_eq!(before_catalog, catalog_bytes(&fixture));
    assert_eq!(before_tree, workspace_fingerprint(&fixture));
    assert!(
        paths.degradations.exists(),
        "the marker must not be consumed"
    );
    let row = fixture.workspace.row().expect("row");
    assert_eq!(row.condition, WorkspaceCondition::Healthy);

    // A range that excludes every uncertainty reports completeness.
    let (code, stdout, _) = run_cli(
        &fixture,
        &[
            "inspect",
            "timeline",
            "--json",
            "--since",
            "2020-01-01T00:00:00Z",
            "--until",
            "2020-06-01T00:00:00Z",
        ],
    );
    assert_eq!(code, 0);
    assert_eq!(parse_json(&stdout)["history_complete"], true);
    assert_eq!(
        parse_json(&stdout)["entries"]
            .as_array()
            .expect("entries")
            .len(),
        0
    );
}

/// AC6: the watcher event summary counts in-range events by kind, counts
/// unparseable lines instead of dropping them, sees both log generations,
/// and states honestly when the log cannot speak for the range's start.
#[test]
fn watcher_evidence_is_summarized_with_honest_coverage() {
    let fixture = fixture();
    let paths = WatchPaths::new(&fixture.workspace.storage.project_root);
    std::fs::create_dir_all(&paths.dir).expect("watch dir");

    let event = |sequence: u64, timestamp: i64, kind: EventKind| {
        serde_json::to_string(&FsEvent {
            run_id: uuid::Uuid::new_v4(),
            sequence,
            path: format!("f{sequence}.txt"),
            kind,
            from_path: None,
            timestamp,
            source: EventSource::Fake,
        })
        .expect("event line")
    };

    // Older generation: one CREATE in 2020 (before any realistic range).
    let old_time = rewind::humantime::parse_rfc3339("2020-01-01T00:00:00Z").expect("old");
    fs::write(
        &paths.events_previous,
        format!("{}\n", event(1, old_time, EventKind::Create)),
    )
    .expect("rotated log");
    // Current generation: in-range events plus an anomaly marker plus a
    // malformed line, all of which must be accounted for.
    let base = rewind::humantime::parse_rfc3339("2026-06-02T00:00:00Z").expect("base");
    fs::write(
        &paths.events,
        format!(
            "{}\n{}\n{}\n{{\"anomaly\":\"dropped unconfineable event path /elsewhere/a.txt\",\"ts\":0}}\nnot json at all\n",
            event(2, base, EventKind::Create),
            event(3, base + 1, EventKind::Modify),
            event(4, base + 2, EventKind::Create),
        ),
    )
    .expect("current log");

    let (code, stdout, stderr) = run_cli(
        &fixture,
        &[
            "inspect",
            "timeline",
            "--json",
            "--since",
            "2026-06-01T00:00:00Z",
            "--until",
            "2026-06-03T00:00:00Z",
        ],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let report = parse_json(&stdout);
    let watcher = &report["watcher"];
    assert_eq!(watcher["log_generations"], 2);
    assert_eq!(watcher["events_in_range"]["CREATE"], 2);
    assert_eq!(watcher["events_in_range"]["MODIFY"], 1);
    assert_eq!(watcher["unparseable_log_lines"], 2);
    assert_eq!(
        watcher["log_oldest_observation"], old_time,
        "the oldest observation across both generations"
    );
    // The log reaches back before `since`, so it can speak for the range.
    assert_eq!(watcher["first_event_in_range"], base);
    assert_eq!(watcher["last_event_in_range"], base + 2);

    // A range that starts before the log's oldest observation gets the
    // explicit honesty note in human output.
    let (code, stdout, _) = run_cli(
        &fixture,
        &[
            "inspect",
            "timeline",
            "--since",
            "2019-01-01T00:00:00Z",
            "--until",
            "2026-06-03T00:00:00Z",
        ],
    );
    assert_eq!(code, 0);
    assert!(
        stdout.contains("proves nothing about the earlier portion"),
        "{stdout}"
    );
}

/// AC8: invalid ranges and malformed timestamps are usage errors that
/// mutate nothing.
#[test]
fn invalid_ranges_are_usage_errors_that_mutate_nothing() {
    let fixture = fixture();
    checkpoint_wal(&fixture);
    let before_catalog = catalog_bytes(&fixture);

    let cases: Vec<Vec<&str>> = vec![
        vec![
            "inspect",
            "timeline",
            "--since",
            "2026-06-02T00:00:00Z",
            "--until",
            "2026-06-01T00:00:00Z",
        ], // since > until
        vec![
            "inspect",
            "timeline",
            "--since",
            "2026-06-01T00:00:00Z",
            "--until",
            "2026-06-01T00:00:00Z",
        ], // empty range
        vec!["inspect", "timeline", "--since", "not-a-time"],
        vec!["inspect", "timeline", "--until", "2026-13-01T00:00:00Z"],
        vec!["inspect", "timeline", "--since", "2026-02-29T00:00:00Z"],
    ];
    for args in &cases {
        let (code, stdout, stderr) = run_cli(&fixture, args);
        assert_eq!(
            code, 2,
            "expected usage error for {args:?}: {stdout}{stderr}"
        );
        assert!(!stderr.is_empty(), "a diagnostic is required for {args:?}");
    }
    assert_eq!(
        before_catalog,
        catalog_bytes(&fixture),
        "validation failures must not touch the catalog"
    );
}

/// AC5: the human output labels uncertainty in words rather than implying
/// completeness, and passive boundaries show as low-confidence records.
#[test]
fn human_output_states_uncertainty_and_tier_honesty() {
    let fixture = fixture();
    let paths = WatchPaths::new(&fixture.workspace.storage.project_root);
    std::fs::create_dir_all(&paths.dir).expect("watch dir");
    // The catalog stamps the boundary at `now`, so the default range
    // (workspace creation ..= now) contains it.
    fixture
        .workspace
        .storage
        .catalog
        .add_boundary(
            fixture.workspace.id,
            "session-1",
            "npm install --force",
            "C:/ws",
        )
        .expect("add boundary");

    let (code, stdout, stderr) = run_cli(&fixture, &["inspect", "timeline"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("PASSIVE_BOUNDARY"), "{stdout}");
    assert!(stdout.contains("npm install --force"), "{stdout}");
    assert!(
        stdout.contains("history_complete=true"),
        "a lone boundary is not uncertainty: {stdout}"
    );
}
