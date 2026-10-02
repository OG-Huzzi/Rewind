//! Phase 6 verification: the manifest/lockfile evidence layer
//! (`.ai/PHASE_6_RECIPES.md` §8, ADR-022).
//!
//! File-based tests drive the real capture path with ordinary filesystem
//! commands (no package manager required); the real-manager tests run where
//! the probed toolchain exists and report an honest skip otherwise — never
//! a claimed pass. Nothing here weakens or replaces an existing assertion.

use std::fs;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use rewind::model::{OperationKind, OperationRecord, RecipeKind, WorkspaceCondition};
use rewind::rollback::{redo, undo};
use rewind::workspace::Workspace;

mod common;

use serde_json::Value;
use tempfile::TempDir;

/// A real initialized workspace with an external store.
struct Fixture {
    root: TempDir,
    _store: TempDir,
    workspace: Workspace,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("temporary root");
    fs::write(root.path().join("seed.txt"), b"seed").expect("write fixture");
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

/// Captures one strongly attributed command and returns its operation id.
/// A capture that did not complete is a test failure, not a skip.
fn capture(fixture: &Fixture, command: Vec<String>) -> i64 {
    let outcome = fixture
        .workspace
        .run_command(&command)
        .expect("run command");
    assert!(
        outcome.captured,
        "capture failed: {:?} (exit {:?})",
        outcome.capture_error, outcome.exit_code
    );
    outcome.operation_id.expect("captured operation id")
}

fn operation(fixture: &Fixture, id: i64) -> OperationRecord {
    fixture
        .workspace
        .storage
        .catalog
        .operation(id, fixture.workspace.id)
        .expect("read operation")
}

fn verify_cas(fixture: &Fixture, hash: &str) {
    fixture
        .workspace
        .storage
        .cas
        .verify(hash)
        .unwrap_or_else(|error| panic!("CAS must hold recorded hash {hash}: {error}"));
}

/// Writes a platform-native script (house convention: never inline shell
/// commands) and returns the argv that runs it with the workspace root as
/// its working directory. Scripts live outside the workspace so they never
/// appear in the captured state.
fn run_script(name: &str, windows_body: &str, posix_body: &str) -> Vec<String> {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let scratch = std::env::temp_dir().join(format!(
        "rewind-phase6-script-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&scratch).expect("script scratch dir");
    common::shell_script(&scratch, name, windows_body, posix_body)
}

fn write_file_script(name: &str, file: &str, content: &str) -> Vec<String> {
    run_script(
        name,
        &format!("echo {content}>{file}\n"),
        &format!("echo {content} > {file}\n"),
    )
}

fn parse_json(stdout: &str) -> Value {
    serde_json::from_str(stdout).expect("parse JSON output")
}

fn run_cli_owned(fixture: &Fixture, args: &[String]) -> (i32, String, String) {
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

/// A half-open timeline range computed from the recorded operation times,
/// never from the wall clock: the assertion must not rot when CI runs in
/// a later year (the same determinism phase 4 established).
fn timeline_range_args(fixture: &Fixture) -> Vec<String> {
    let operations = fixture
        .workspace
        .storage
        .catalog
        .list_operations(fixture.workspace.id)
        .expect("operations");
    let created_at = operations.last().expect("at least one operation").created_at;
    let since = rewind::humantime::format_rfc3339(created_at);
    let until = rewind::humantime::format_rfc3339(created_at + 1);
    vec![
        "inspect".to_owned(),
        "timeline".to_owned(),
        "--json".to_owned(),
        "--since".to_owned(),
        since,
        "--until".to_owned(),
        until,
    ]
}

fn command_available(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Creates `Cargo.lock` (content "one") and `Cargo.toml` in one captured
/// command and returns the operation id.
fn capture_cargo_pair(fixture: &Fixture) -> i64 {
    capture(
        fixture,
        run_script(
            "phase6-create-cargo",
            "echo one>Cargo.lock\necho m>Cargo.toml\n",
            "echo one > Cargo.lock\necho m > Cargo.toml\n",
        ),
    )
}

/// AC1 + AC3: a captured lockfile lifecycle (created, changed, removed)
/// yields evidence with the recorded CAS hashes, both directions.
#[test]
fn lockfile_lifecycle_yields_evidence_with_verifiable_hashes() {
    let fixture = fixture();
    let created = capture_cargo_pair(&fixture);
    let changed = capture(
        &fixture,
        write_file_script("phase6-change-lock", "Cargo.lock", "two"),
    );
    let removed = capture(
        &fixture,
        run_script("phase6-remove-lock", "del Cargo.lock\n", "rm Cargo.lock\n"),
    );

    let first = operation(&fixture, created);
    assert_eq!(
        first.evidence.len(),
        1,
        "created lockfile must yield evidence"
    );
    let added = &first.evidence[0];
    assert_eq!(added.kind, RecipeKind::Cargo);
    assert_eq!(added.lockfile, "Cargo.lock");
    assert_eq!(added.pre_hash, None, "absent before the command");
    let post_hash = added.post_hash.clone().expect("post hash recorded");
    assert_eq!(added.manifests, vec!["Cargo.toml".to_owned()]);
    verify_cas(&fixture, &post_hash);

    let second = operation(&fixture, changed);
    assert_eq!(
        second.evidence.len(),
        1,
        "changed lockfile must yield evidence"
    );
    let modified = &second.evidence[0];
    assert_eq!(modified.pre_hash.as_deref(), Some(post_hash.as_str()));
    let second_post = modified.post_hash.clone().expect("changed post hash");
    assert_ne!(second_post, post_hash, "distinct content hashes");
    verify_cas(&fixture, &second_post);

    let third = operation(&fixture, removed);
    assert_eq!(
        third.evidence.len(),
        1,
        "removed lockfile must yield evidence"
    );
    let deleted = &third.evidence[0];
    assert_eq!(deleted.pre_hash.as_deref(), Some(second_post.as_str()));
    assert_eq!(deleted.post_hash, None, "absent after the command");
    assert_eq!(
        deleted.manifests,
        vec!["Cargo.toml".to_owned()],
        "the manifest still present after the lockfile was removed"
    );
}

/// AC2: nested, similar, unrecognized, case-deviating, and non-regular
/// lockfile names produce no evidence — recognition is exact and root-only.
#[test]
fn no_false_positives_for_nested_similar_or_deviating_names() {
    let base = fixture();
    let script = run_script(
        "phase6-no-false-positives",
        "mkdir sub\necho one>sub\\Cargo.lock\necho x>package-lock.json.bak\necho x>Cargo.lock.bak\necho x>package-lock.json.old\necho x>Gemfile.lock\n",
        "mkdir sub\necho one > sub/Cargo.lock\necho x > package-lock.json.bak\necho x > Cargo.lock.bak\necho x > package-lock.json.old\necho x > Gemfile.lock\n",
    );
    let operation_id = capture(&base, script);
    assert!(
        operation(&base, operation_id).evidence.is_empty(),
        "nested/similar/unrecognized names must never produce evidence"
    );

    // Case deviation: recognized nowhere (exact byte equality on every
    // platform), including on case-insensitive filesystems.
    let case_fixture = fixture();
    let case_id = capture(
        &case_fixture,
        write_file_script("phase6-case-deviant", "cargo.lock", "one"),
    );
    assert!(
        operation(&case_fixture, case_id).evidence.is_empty(),
        "a deviating casing is not a recognized lockfile name"
    );

    // Non-regular objects: a directory at the lockfile name, and a
    // file-to-directory replacement, both stay out of the evidence layer.
    let object_fixture = fixture();
    let dir_id = capture(
        &object_fixture,
        run_script(
            "phase6-mkdir-lock",
            "mkdir Cargo.lock\n",
            "mkdir Cargo.lock\n",
        ),
    );
    assert!(
        operation(&object_fixture, dir_id).evidence.is_empty(),
        "a directory named Cargo.lock is not a lockfile"
    );
    let file_fixture = fixture();
    capture_cargo_pair(&file_fixture);
    let replaced = capture(
        &file_fixture,
        run_script(
            "phase6-replace-with-dir",
            "del Cargo.lock\nmkdir Cargo.lock\n",
            "rm Cargo.lock\nmkdir Cargo.lock\n",
        ),
    );
    assert!(
        operation(&file_fixture, replaced).evidence.is_empty(),
        "a lockfile replaced by a directory suppresses evidence"
    );
}

/// AC3 boundary policy: a byte-identical rewrite and a manifest-only
/// change produce no evidence (explicit no-change policy).
#[test]
fn same_hash_rewrites_and_manifest_only_changes_are_not_evidence() {
    let fixture = fixture();
    let created = capture_cargo_pair(&fixture);
    assert_eq!(operation(&fixture, created).evidence.len(), 1);

    let rewritten = capture(
        &fixture,
        write_file_script("phase6-rewrite-same", "Cargo.lock", "one"),
    );
    assert!(
        operation(&fixture, rewritten).evidence.is_empty(),
        "re-writing identical bytes is not a dependency-state change"
    );

    let manifest_only = capture(
        &fixture,
        write_file_script("phase6-manifest-only", "Cargo.toml", "m2"),
    );
    assert!(
        operation(&fixture, manifest_only).evidence.is_empty(),
        "a manifest edit without a lockfile change is not evidence"
    );
}

/// AC1 (documented set): one captured command creating every recognized
/// lockfile yields one evidence entry per recipe, in lockfile-path order,
/// with the documented manifests.
#[test]
fn every_recognized_kind_is_derived_through_real_capture() {
    let fixture = fixture();
    let script = run_script(
        "phase6-all-kinds",
        concat!(
            "echo c>Cargo.lock\necho m>Cargo.toml\n",
            "echo n>package-lock.json\necho p>package.json\n",
            "echo k>pnpm-lock.yaml\n",
            "echo g>go.sum\necho t>go.mod\n",
            "echo u>uv.lock\necho y>pyproject.toml\necho s>setup.py\n",
            "echo r>requirements.txt\n"
        ),
        concat!(
            "echo c > Cargo.lock\necho m > Cargo.toml\n",
            "echo n > package-lock.json\necho p > package.json\n",
            "echo k > pnpm-lock.yaml\n",
            "echo g > go.sum\necho t > go.mod\n",
            "echo u > uv.lock\necho y > pyproject.toml\necho s > setup.py\n",
            "echo r > requirements.txt\n"
        ),
    );
    let operation_id = capture(&fixture, script);
    let evidence = operation(&fixture, operation_id).evidence;
    assert_eq!(evidence.len(), 6, "one entry per recognized kind");
    let kinds: Vec<&str> = evidence.iter().map(|entry| entry.kind.as_str()).collect();
    assert_eq!(kinds, vec!["cargo", "go", "npm", "pnpm", "pip", "uv"]);
    let lockfiles: Vec<&str> = evidence
        .iter()
        .map(|entry| entry.lockfile.as_str())
        .collect();
    assert_eq!(
        lockfiles,
        vec![
            "Cargo.lock",
            "go.sum",
            "package-lock.json",
            "pnpm-lock.yaml",
            "requirements.txt",
            "uv.lock",
        ]
    );
    assert_eq!(evidence[0].manifests, vec!["Cargo.toml".to_owned()]);
    assert_eq!(evidence[1].manifests, vec!["go.mod".to_owned()]);
    assert_eq!(evidence[2].manifests, vec!["package.json".to_owned()]);
    assert_eq!(evidence[3].manifests, vec!["package.json".to_owned()]);
    assert_eq!(
        evidence[4].manifests,
        vec!["pyproject.toml".to_owned(), "setup.py".to_owned()]
    );
    assert_eq!(evidence[5].manifests, vec!["pyproject.toml".to_owned()]);
    for entry in &evidence {
        if let Some(hash) = &entry.post_hash {
            verify_cas(&fixture, hash);
        }
    }
}

/// AC5: passive observations and capture failures never carry evidence,
/// even when the observed change is a lockfile change.
#[test]
fn passive_observations_and_capture_failures_carry_no_evidence() {
    let fixture = fixture();
    let store = fixture._store.path().to_string_lossy().into_owned();

    let pre = Command::new(env!("CARGO_BIN_EXE_rewind"))
        .args([
            "hook",
            "pre",
            "--command",
            "external-lockfile-change",
            "--session",
            "phase6",
        ])
        .current_dir(fixture.root.path())
        .env("REWIND_HOME", &store)
        .output()
        .expect("pre-hook");
    assert!(pre.status.success(), "pre-hook failed");
    let boundary_id = String::from_utf8(pre.stdout)
        .expect("utf8")
        .trim()
        .to_owned();
    // The lockfile changes while the "command" runs.
    fs::write(fixture.root.path().join("Cargo.toml"), b"[package]\n").expect("manifest");
    fs::write(fixture.root.path().join("Cargo.lock"), b"one\n").expect("lockfile");
    let post = Command::new(env!("CARGO_BIN_EXE_rewind"))
        .args([
            "hook",
            "post",
            "--boundary",
            &boundary_id,
            "--exit-code",
            "0",
        ])
        .current_dir(fixture.root.path())
        .env("REWIND_HOME", &store)
        .output()
        .expect("post-hook");
    assert!(post.status.success(), "post-hook failed");

    let operations = fixture
        .workspace
        .storage
        .catalog
        .list_operations(fixture.workspace.id)
        .expect("operations");
    let passive = operations
        .iter()
        .find(|entry| entry.command.as_deref() == Some("external-lockfile-change"))
        .expect("passive observation recorded");
    assert_eq!(passive.kind, OperationKind::PassiveObservation);
    assert!(
        passive
            .effects
            .iter()
            .any(|effect| effect.path == "Cargo.lock"),
        "the passive observation saw the lockfile change"
    );
    assert!(
        passive.evidence.is_empty(),
        "passive observations are low-confidence boundaries, not evidence"
    );

    // The shared capture-failure path records no evidence either.
    let baseline = fixture.workspace.baseline_id().expect("baseline");
    let failure_id = fixture
        .workspace
        .record_capture_failure(
            Some(&baseline),
            Some("synthetic-failure".to_owned()),
            None,
            Some(1),
            "synthetic post-scan failure",
        )
        .expect("record capture failure");
    let failure = operation(&fixture, failure_id);
    assert_eq!(failure.kind, OperationKind::CaptureFailed);
    assert!(
        failure.evidence.is_empty(),
        "a failed capture proves nothing"
    );
}

/// AC6: a pre-phase catalog (no evidence table, legacy rows) opens, gains
/// the table idempotently, keeps every old row readable and displayable
/// byte-identically (field skipped), and accepts new evidence; a present
/// but unparseable blob is an explicit error, never a silent empty.
#[test]
fn legacy_catalogs_gain_the_table_idempotently_and_rows_read_unchanged() {
    let fixture = fixture();
    let legacy_id = capture(
        &fixture,
        write_file_script("phase6-legacy-write", "foo.txt", "A"),
    );
    let catalog_path = fixture.workspace.storage.catalog.path().to_path_buf();

    // Simulate the pre-phase catalog: the table does not exist.
    let connection = rusqlite::Connection::open(&catalog_path).expect("open catalog");
    connection
        .execute_batch("DROP TABLE operation_evidence;")
        .expect("drop evidence table");
    drop(connection);

    // Opening the workspace recreates the table (idempotent initialize),
    // and the legacy row reads exactly as before.
    let reopened = Workspace::open_from_current(fixture.root.path()).expect("reopen");
    let legacy = reopened
        .storage
        .catalog
        .operation(legacy_id, reopened.id)
        .expect("legacy row reads after the schema addition");
    assert!(legacy.evidence.is_empty());

    let (code, stdout, stderr) = run_cli(&fixture, &["show", &legacy_id.to_string()]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        !stdout.contains("\"evidence\""),
        "the evidence field must be skipped when empty:\n{stdout}"
    );

    // New captures on the upgraded catalog work, evidence included.
    let new_id = capture_cargo_pair(&fixture);
    assert_eq!(operation(&fixture, new_id).evidence.len(), 1);

    // Idempotent on every subsequent open.
    let _again = Workspace::open_from_current(fixture.root.path()).expect("reopen again");
    assert_eq!(operation(&fixture, new_id).evidence.len(), 1);

    // A present but malformed blob is an explicit read error (parity with
    // the effects JSON), never a silently emptied record.
    let connection = rusqlite::Connection::open(&catalog_path).expect("open catalog");
    connection
        .execute(
            "INSERT INTO operation_evidence(operation_id, evidence_json) VALUES(?1, ?2)",
            rusqlite::params![legacy_id, "{not json"],
        )
        .expect("insert malformed blob");
    drop(connection);
    assert!(
        fixture
            .workspace
            .storage
            .catalog
            .operation(legacy_id, fixture.workspace.id)
            .is_err(),
        "an unparseable evidence blob must be an explicit error"
    );
}

/// AC7: presentation renders evidence on list/show/timeline for stores
/// that contain it, and evidence-free stores render exactly as before.
#[test]
fn presentation_renders_evidence_and_evidence_free_output_is_unchanged() {
    // Evidence-bearing store.
    let with_evidence = fixture();
    let operation_id = capture_cargo_pair(&with_evidence);
    let (code, stdout, stderr) = run_cli(&with_evidence, &["list"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("lockfiles=[Cargo.lock]"),
        "list must render the evidence names:\n{stdout}"
    );

    let (code, stdout, stderr) = run_cli(&with_evidence, &["show", &operation_id.to_string()]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let shown = parse_json(&stdout);
    let evidence = shown["evidence"].as_array().expect("evidence array");
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0]["kind"], "cargo");
    assert_eq!(evidence[0]["lockfile"], "Cargo.lock");
    assert_eq!(evidence[0]["pre_hash"], Value::Null);
    let post_hash = evidence[0]["post_hash"].as_str().expect("post hash");
    verify_cas(&with_evidence, post_hash);

    let (code, stdout, stderr) = run_cli(&with_evidence, &["inspect", "timeline"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("lockfiles=[Cargo.lock]"),
        "timeline human output must render the evidence:\n{stdout}"
    );
    let range = timeline_range_args(&with_evidence);
    let (code, stdout, stderr) = run_cli_owned(&with_evidence, &range);
    assert_eq!(code, 0, "stderr: {stderr}");
    let report = parse_json(&stdout);
    let entry = &report["entries"][0];
    assert_eq!(entry["tier"], "OPERATION");
    assert_eq!(entry["evidence"][0]["lockfile"], "Cargo.lock");

    // Evidence-free store: every surface byte-compatible with the
    // pre-phase form.
    let plain = fixture();
    let plain_id = capture(
        &plain,
        write_file_script("phase6-plain-write", "foo.txt", "A"),
    );
    let stored = operation(&plain, plain_id);
    let expected_line = format!(
        "#{id} STRONG COMPLETED TRACKED FULLY_REVERSIBLE {command}",
        id = plain_id,
        command = stored.command.clone().expect("command"),
    );
    let (code, stdout, stderr) = run_cli(&plain, &["list"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "condition line + one operation line");
    assert_eq!(
        lines[1], expected_line,
        "an evidence-free list line must be byte-identical to the pre-phase form"
    );

    let (code, stdout, _) = run_cli(&plain, &["show", &plain_id.to_string()]);
    assert_eq!(code, 0);
    assert!(!stdout.contains("\"evidence\""));
    let (code, stdout, _) = run_cli(&plain, &["inspect", "history", "--json"]);
    assert_eq!(code, 0);
    assert!(!stdout.contains("\"evidence\""));

    let (code, stdout, _) = run_cli(&plain, &["inspect", "timeline"]);
    assert_eq!(code, 0);
    assert!(!stdout.contains("lockfiles=["));
    let range = timeline_range_args(&plain);
    let (code, stdout, _) = run_cli_owned(&plain, &range);
    assert_eq!(code, 0);
    assert!(!stdout.contains("\"evidence\""));
}

/// AC7 (determinism): the Phase 4 byte-identical-JSON guarantee holds for
/// an evidence-bearing store as well as an evidence-free one.
#[test]
fn timeline_json_is_byte_identical_for_evidence_bearing_and_free_stores() {
    let with_evidence = fixture();
    capture_cargo_pair(&with_evidence);
    let range = timeline_range_args(&with_evidence);
    let first = run_cli_owned(&with_evidence, &range);
    let second = run_cli_owned(&with_evidence, &range);
    assert_eq!(first.0, 0);
    assert_eq!(second.0, 0);
    assert!(
        first.1.contains("\"evidence\""),
        "the range must contain the evidence-bearing operation:\n{}",
        first.1
    );
    assert_eq!(first.1, second.1, "same store, same range, same bytes");

    let plain = fixture();
    capture(
        &plain,
        write_file_script("phase6-plain-write", "foo.txt", "A"),
    );
    let range = timeline_range_args(&plain);
    let first = run_cli_owned(&plain, &range);
    let second = run_cli_owned(&plain, &range);
    assert_eq!(first.0, 0);
    assert_eq!(first.1, second.1, "evidence-free determinism preserved");
    assert!(!first.1.contains("\"evidence\""));
}

/// AC8: undo and redo move exactly the recorded states; the evidence
/// record is inert metadata, unchanged by either direction.
#[test]
fn undo_and_redo_are_unaffected_by_recorded_evidence() {
    let fixture = fixture();
    let operation_id = capture_cargo_pair(&fixture);
    let before = operation(&fixture, operation_id).evidence;
    assert_eq!(before.len(), 1);

    undo(&fixture.workspace, Some(operation_id), false).expect("undo");
    assert!(
        !fixture.root.path().join("Cargo.lock").exists(),
        "undo restores the absent pre-state"
    );
    assert_eq!(
        operation(&fixture, operation_id).evidence,
        before,
        "undo does not alter the recorded evidence"
    );
    assert_eq!(
        fixture.workspace.condition().expect("condition"),
        WorkspaceCondition::Healthy
    );

    redo(&fixture.workspace, Some(operation_id)).expect("redo");
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("Cargo.lock")).expect("redo restores content"),
        String::from_utf8(common::echoed("one")).expect("utf8")
    );
    assert_eq!(
        operation(&fixture, operation_id).evidence,
        before,
        "redo does not alter the recorded evidence"
    );
}

/// AC9 (real manager, cargo): a captured `cargo generate-lockfile
/// --offline` (probed offline-safe: no dependencies) yields evidence whose
/// hashes verify against the CAS; a second captured run after a manifest
/// change yields the changed transition. Skipped only when cargo itself is
/// absent, and the skip is reported.
#[test]
fn real_cargo_action_yields_evidence_with_verifiable_hashes() {
    if !command_available("cargo", &["--version"]) {
        eprintln!("SKIPPED: cargo is unavailable on this runner (reported honestly)");
        return;
    }
    let fixture = fixture();
    fs::write(
        fixture.root.path().join("Cargo.toml"),
        "[package]\nname = \"phase6-cargo-probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("manifest");
    fs::create_dir(fixture.root.path().join("src")).expect("src dir");
    fs::write(
        fixture.root.path().join("src").join("main.rs"),
        "fn main() {}\n",
    )
    .expect("main");

    let cargo = || {
        vec![
            "cargo".to_owned(),
            "generate-lockfile".to_owned(),
            "--offline".to_owned(),
        ]
    };
    let created = capture(&fixture, cargo());
    assert!(fixture.root.path().join("Cargo.lock").exists());
    let first = operation(&fixture, created);
    assert_eq!(first.evidence.len(), 1, "cargo must yield evidence");
    let added = &first.evidence[0];
    assert_eq!(added.kind, RecipeKind::Cargo);
    assert_eq!(added.pre_hash, None);
    let first_post = added.post_hash.clone().expect("post hash");
    verify_cas(&fixture, &first_post);

    // Rewrite the manifest (ordinary captured command), then let cargo
    // regenerate the lockfile: the changed transition through a real
    // manager.
    let rewrite = run_script(
        "phase6-cargo-rename",
        "echo [package]>Cargo.toml\necho name = \"phase6-cargo-probe-b\">>Cargo.toml\necho version = \"0.1.0\">>Cargo.toml\necho edition = \"2021\">>Cargo.toml\n",
        "printf '[package]\\nname = \"phase6-cargo-probe-b\"\\nversion = \"0.1.0\"\\nedition = \"2021\"\\n' > Cargo.toml\n",
    );
    let rewrite_id = capture(&fixture, rewrite);
    assert!(
        operation(&fixture, rewrite_id).evidence.is_empty(),
        "a manifest-only change is not lockfile evidence"
    );
    let changed = capture(&fixture, cargo());
    let second = operation(&fixture, changed);
    assert_eq!(second.evidence.len(), 1);
    let modified = &second.evidence[0];
    assert_eq!(modified.pre_hash.as_deref(), Some(first_post.as_str()));
    let second_post = modified.post_hash.clone().expect("changed post hash");
    assert_ne!(second_post, first_post);
    verify_cas(&fixture, &second_post);
}

/// AC9 (real manager, npm): a captured lock-only npm action (probed
/// offline-safe with a blackholed registry for a zero-dependency project)
/// yields evidence whose hashes verify against the CAS. Skipped only when
/// npm itself is absent, and the skip is reported.
#[test]
fn real_npm_action_yields_evidence_with_verifiable_hashes() {
    let npm_program = if cfg!(windows) { "npm.cmd" } else { "npm" };
    if !command_available(npm_program, &["--version"]) {
        eprintln!("SKIPPED: npm is unavailable on this runner (reported honestly)");
        return;
    }
    let npm = || {
        vec![
            npm_program.to_owned(),
            "install".to_owned(),
            "--package-lock-only".to_owned(),
            "--ignore-scripts".to_owned(),
            "--offline".to_owned(),
            "--no-audit".to_owned(),
            "--no-fund".to_owned(),
        ]
    };
    let fixture = fixture();
    fs::write(
        fixture.root.path().join("package.json"),
        "{\"name\":\"phase6-npm-probe\",\"version\":\"1.0.0\"}\n",
    )
    .expect("package.json");

    let created = capture(&fixture, npm());
    assert!(fixture.root.path().join("package-lock.json").exists());
    let first = operation(&fixture, created);
    assert_eq!(first.evidence.len(), 1, "npm must yield evidence");
    let added = &first.evidence[0];
    assert_eq!(added.kind, RecipeKind::Npm);
    assert_eq!(added.lockfile, "package-lock.json");
    assert_eq!(added.pre_hash, None);
    let first_post = added.post_hash.clone().expect("post hash");
    verify_cas(&fixture, &first_post);

    // Bump the version through an ordinary captured command, then let npm
    // regenerate the lockfile: the changed transition through a real
    // manager (probe: npm lock-only rewrites the root version offline).
    let rewrite = run_script(
        "phase6-npm-bump",
        "echo {\"name\":\"phase6-npm-probe\",\"version\":\"1.0.1\"}>package.json\n",
        "printf '{\"name\":\"phase6-npm-probe\",\"version\":\"1.0.1\"}\\n' > package.json\n",
    );
    capture(&fixture, rewrite);
    let changed = capture(&fixture, npm());
    let second = operation(&fixture, changed);
    assert_eq!(second.evidence.len(), 1);
    let modified = &second.evidence[0];
    assert_eq!(modified.pre_hash.as_deref(), Some(first_post.as_str()));
    let second_post = modified.post_hash.clone().expect("changed post hash");
    assert_ne!(second_post, first_post);
    verify_cas(&fixture, &second_post);
}

/// AC1 (purity witness at the integration level): the evidence presents
/// exactly the recorded fingerprint CAS ids — no re-hashing, no second
/// source of truth.
#[test]
fn evidence_hashes_are_the_recorded_fingerprint_ids() {
    let fixture = fixture();
    let operation_id = capture(
        &fixture,
        write_file_script("phase6-fingerprint-write", "Cargo.lock", "one"),
    );
    let stored = operation(&fixture, operation_id);
    let entry = &stored.evidence[0];
    let recorded = stored
        .effects
        .iter()
        .find(|effect| effect.path == "Cargo.lock")
        .expect("lockfile effect");
    let recorded_hash = recorded
        .post
        .content_hash()
        .expect("regular file content hash")
        .to_owned();
    assert_eq!(
        entry.post_hash.as_deref(),
        Some(recorded_hash.as_str()),
        "the evidence must present the recorded CAS id, not a new hash"
    );
}
