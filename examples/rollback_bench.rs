//! Rollback performance benchmark (Phase: rollback performance investigation).
//!
//! Reproducible methodology: create a real workspace with N deterministic
//! files in a temporary directory with an external store, capture one real
//! operation that modifies every file, then time `undo` and `redo` end to
//! end, plus the cost of one bare full scan so the scan share of the total
//! can be computed rather than guessed.
//!
//! Run with a debug build to compare against the recorded historical
//! baseline (`.ai/TEST_STATUS.md`: undo of 400 modified files ≈ 15.7 min,
//! debug):
//!
//! ```text
//! cargo run --example rollback_bench -- --files 100
//! cargo run --release --example rollback_bench -- --files 400
//! ```

use std::fs;
use std::time::Instant;

use rewind::rollback::{redo, undo};
use rewind::workspace::Workspace;

struct Args {
    files: usize,
    payload: usize,
}

fn parse_args() -> Args {
    let mut files = 100usize;
    let mut payload = 200usize;
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
            other => panic!("unknown argument {other}"),
        }
        position += 1;
    }
    Args { files, payload }
}

/// Deterministic pseudo-content: same inputs produce the same bytes on every
/// run and every machine, so runs are comparable.
fn payload_line(index: usize, generation: u32, payload: usize) -> String {
    let mut line = String::with_capacity(payload + 32);
    line.push_str(&format!("gen{generation} file{index} "));
    while line.len() < payload {
        let mix = (index as u64)
            .wrapping_mul(6364136223846793005)
            .wrapping_add((line.len() as u64) << 7)
            .wrapping_add(generation as u64);
        line.push_str(&format!("{:016x}", mix));
    }
    line.truncate(payload);
    line
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
        "bench: files={} payload={}B profile={}",
        args.files,
        args.payload,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );

    let started = Instant::now();
    for index in 0..args.files {
        fs::write(
            root.join(format!("file{index:05}.txt")),
            payload_line(index, 0, args.payload),
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

    // One real captured operation that modifies every file.
    let modify = if cfg!(windows) {
        vec![
            "cmd".to_owned(),
            "/C".to_owned(),
            "for %f in (file*.txt) do @echo M>> %f".to_owned(),
        ]
    } else {
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "for f in file*.txt; do echo M >> \"$f\"; done".to_owned(),
        ]
    };
    let started = Instant::now();
    let outcome = workspace.run_command(&modify).expect("run command");
    println!(
        "{:>26}: {:>10.2?}  (2 full scans + CAS)",
        "capture (run_command)",
        started.elapsed()
    );
    let operation_id = outcome.operation_id.expect("captured operation id");

    // Sanity: the capture must have modified every file (one operation,
    // N steps when rolled back).
    let steps = {
        let record = workspace
            .storage
            .catalog
            .operation(operation_id, workspace.id)
            .expect("operation");
        record.effects.len()
    };
    println!("{:>26}: {steps}", "rollback steps");

    let started = Instant::now();
    undo(&workspace, Some(operation_id), false).expect("undo");
    let undo_time = started.elapsed();
    println!("{:>26}: {:>10.2?}", "UNDO", undo_time);

    let started = Instant::now();
    redo(&workspace, Some(operation_id)).expect("redo");
    let redo_time = started.elapsed();
    println!("{:>26}: {:>10.2?}", "REDO", redo_time);

    // Verify final content matches the post state (redo reapplied the
    // modification): the file must be longer than the original payload.
    let content = fs::read_to_string(root.join("file00000.txt")).expect("final file");
    assert!(
        content.len() > args.payload,
        "redo must reapply modifications"
    );

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
