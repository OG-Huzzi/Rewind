//! Rollback performance benchmark (Phase: rollback performance investigation).
//!
//! Reproducible methodology: create a real workspace with N deterministic
//! files in a temporary directory with an external store, capture one real
//! operation of the requested scenario, then time `undo` and `redo` end to
//! end, plus the cost of one bare full scan so the scan share of the old
//! implementation can be computed rather than guessed.
//!
//! Scenarios: `modify` (every seeded file gains a byte), `create` (N new
//! files appear), `delete` (every seeded file disappears), `mixed` (the
//! first third is deleted, the middle third modified, the last third
//! created). Every scenario yields exactly N rollback steps.
//!
//! Run with a debug build to compare against the recorded historical
//! baseline (`.ai/TEST_STATUS.md`: undo of 400 modified files ≈ 15.7 min,
//! debug):
//!
//! ```text
//! cargo run --example rollback_bench -- --files 100 --scenario modify
//! cargo run --example rollback_bench -- --files 100 --payload 10240
//! ```

use std::fs;
use std::time::Instant;

use rewind::rollback::{redo, undo};
use rewind::workspace::Workspace;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Scenario {
    Modify,
    Create,
    Delete,
    Mixed,
}

struct Args {
    files: usize,
    payload: usize,
    scenario: Scenario,
}

fn parse_args() -> Args {
    let mut files = 100usize;
    let mut payload = 200usize;
    let mut scenario = Scenario::Modify;
    let argv: Vec<String> = std::env::args().collect();
    let mut position = 1;
    while position < argv.len() {
        match argv[position].as_str() {
            "--files" => {
                position += 1;
                files = argv[position].parse().expect("--files takes a number");
            }
            "--payload" => {
                position += 1;
                payload = argv[position].parse().expect("--payload takes a number");
            }
            "--scenario" => {
                position += 1;
                scenario = match argv[position].as_str() {
                    "modify" => Scenario::Modify,
                    "create" => Scenario::Create,
                    "delete" => Scenario::Delete,
                    "mixed" => Scenario::Mixed,
                    other => panic!("unknown scenario {other}"),
                };
            }
            other => panic!("unknown argument {other}"),
        }
        position += 1;
    }
    Args {
        files,
        payload,
        scenario,
    }
}

/// Deterministic pseudo-content: same inputs produce the same bytes on every
/// run and every machine, so runs are comparable.
fn payload_line(index: usize, payload: usize) -> String {
    let mut line = String::with_capacity(payload + 32);
    line.push_str(&format!("file{index} "));
    while line.len() < payload {
        let mix = (index as u64)
            .wrapping_mul(6364136223846793005)
            .wrapping_add((line.len() as u64) << 7);
        line.push_str(&format!("{:016x}", mix));
    }
    line.truncate(payload);
    line
}

fn file_name(index: usize) -> String {
    format!("file{index:05}.txt")
}

fn created_name(index: usize) -> String {
    format!("new{index:05}.txt")
}

/// Writes a platform-native script performing the captured operation and
/// returns the command vector that executes it. Commands are generated
/// explicitly per file (no shell loops): deterministic, identical work on
/// every platform, and no shell-specific escaping. The windows/posix
/// conventions follow `tests/common`.
fn operation_command(scenario: Scenario, files: usize, root: &std::path::Path) -> Vec<String> {
    let third = files / 3;
    let mut windows_lines: Vec<String> = Vec::new();
    let mut posix_lines: Vec<String> = Vec::new();
    let mut push = |scenario: Scenario, index: usize| match scenario {
        Scenario::Modify => {
            windows_lines.push(format!("echo M>> {}", file_name(index)));
            posix_lines.push(format!("echo M >> '{}'", file_name(index)));
        }
        Scenario::Create => {
            windows_lines.push(format!("echo C> {}", created_name(index)));
            posix_lines.push(format!("echo C > '{}'", created_name(index)));
        }
        Scenario::Delete => {
            windows_lines.push(format!("del {}", file_name(index)));
            posix_lines.push(format!("rm -f '{}'", file_name(index)));
        }
        Scenario::Mixed => {
            if index < third {
                windows_lines.push(format!("del {}", file_name(index)));
                posix_lines.push(format!("rm -f '{}'", file_name(index)));
            } else if index < 2 * third {
                windows_lines.push(format!("echo M>> {}", file_name(index)));
                posix_lines.push(format!("echo M >> '{}'", file_name(index)));
            } else {
                windows_lines.push(format!("echo C> {}", created_name(index)));
                posix_lines.push(format!("echo C > '{}'", created_name(index)));
            }
        }
    };
    for index in 0..files {
        push(scenario, index);
    }
    let script = if cfg!(windows) {
        root.join("operation.cmd")
    } else {
        root.join("operation.sh")
    };
    let body = if cfg!(windows) {
        // cmd needs CRLF line endings inside a script file.
        windows_lines.join(
            "
",
        )
    } else {
        posix_lines.join(
            "
",
        )
    };
    fs::write(&script, body).expect("write operation script");
    if cfg!(windows) {
        vec![
            "cmd".to_owned(),
            "/C".to_owned(),
            script.to_string_lossy().into_owned(),
        ]
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&script)
                .expect("script metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&script, permissions).expect("make script executable");
        }
        vec![script.to_string_lossy().into_owned()]
    }
}

fn main() {
    let args = parse_args();
    let run_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis();
    let base = std::env::temp_dir().join(format!("rewind-bench-{run_id}"));
    let root = base.join("ws");
    let store = base.join("store");
    fs::create_dir_all(&root).expect("root");
    fs::create_dir_all(&store).expect("store");

    println!(
        "bench: files={} payload={}B scenario={:?} profile={}",
        args.files,
        args.payload,
        args.scenario,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );

    let started = Instant::now();
    for index in 0..args.files {
        fs::write(
            root.join(file_name(index)),
            payload_line(index, args.payload),
        )
        .expect("seed file");
    }
    println!("{:>26}: {:>10.2?}", "seed files", started.elapsed());

    let started = Instant::now();
    let workspace = Workspace::init(&root, Some(&store)).expect("init");
    println!(
        "{:>26}: {:>10.2?}  (initial full scan + CAS)",
        "workspace init",
        started.elapsed()
    );

    let started = Instant::now();
    let scan = workspace.scan(None).expect("bare scan");
    let bare_scan = started.elapsed();
    println!(
        "{:>26}: {:>10.2?}  ({} files, state {})",
        "one bare full scan",
        bare_scan,
        scan.manifest.entries.len(),
        &scan.state_id[..8]
    );

    // One real captured operation of the requested scenario.
    let command = operation_command(args.scenario, args.files, &root);
    let started = Instant::now();
    let outcome = workspace.run_command(&command).expect("run command");
    println!(
        "{:>26}: {:>10.2?}  (2 full scans + CAS)",
        "capture (run_command)",
        started.elapsed()
    );
    let operation_id = outcome.operation_id.expect("captured operation id");

    // Sanity: the capture must have produced N effects (N rollback steps).
    let steps = {
        let record = workspace
            .storage
            .catalog
            .operation(operation_id, workspace.id)
            .expect("operation");
        record.effects.len()
    };
    println!("{:>26}: {steps}", "rollback steps");
    assert_eq!(steps, args.files, "the scenario must touch exactly N paths");

    let started = Instant::now();
    undo(&workspace, Some(operation_id), false).expect("undo");
    let undo_time = started.elapsed();
    println!("{:>26}: {:>10.2?}", "UNDO", undo_time);

    let started = Instant::now();
    redo(&workspace, Some(operation_id)).expect("redo");
    let redo_time = started.elapsed();
    println!("{:>26}: {:>10.2?}", "REDO", redo_time);

    // Scenario-aware final-state checks: redo must have reapplied the
    // captured operation exactly.
    let third = args.files / 3;
    let seeded_path = |index: usize| root.join(file_name(index));
    let created_path = |index: usize| root.join(created_name(index));
    for index in 0..args.files {
        match args.scenario {
            Scenario::Modify => {
                let content = fs::read_to_string(seeded_path(index)).expect("file after redo");
                assert!(
                    content.len() > args.payload,
                    "redo must reapply the modification to {index}"
                );
            }
            Scenario::Create => {
                assert!(
                    created_path(index).is_file(),
                    "redo must recreate created file {index}"
                );
            }
            Scenario::Delete => {
                assert!(
                    !seeded_path(index).exists(),
                    "redo must re-delete seeded file {index}"
                );
            }
            Scenario::Mixed => {
                if index < third {
                    assert!(!seeded_path(index).exists(), "redo must re-delete {index}");
                } else if index < 2 * third {
                    let content = fs::read_to_string(seeded_path(index)).expect("file after redo");
                    assert!(
                        content.len() > args.payload,
                        "redo must reapply the modification to {index}"
                    );
                } else {
                    assert!(
                        created_path(index).is_file(),
                        "redo must recreate created file {index}"
                    );
                }
            }
        }
    }

    let scan_share = bare_scan.as_secs_f64();
    println!();
    // Historical context only: before the path-scoped step verification
    // (ADR-018) an undo performed 2*steps+2 full scans, so the scan share
    // of the old implementation can be reconstructed from one bare scan.
    println!(
        "historical-model estimate: the pre-ADR-018 undo would spend ≈ {} \
         bare-scan equivalents ({}s) in full scans",
        (2 * steps + 2),
        scan_share * (2 * steps + 2) as f64
    );

    let _ = fs::remove_dir_all(&base);
    println!("scratch cleaned: {}", base.display());
}
