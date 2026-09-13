# Phase 1 Test Status

Status after Phase 1.1 (rollback correctness and recovery repair).

The suite uses real temporary workspaces and real filesystem mutations.

Passed: 22 real-filesystem tests.

- 13 foundation tests (Phase 1 suite, unchanged and still passing).
- 9 Phase 1.1 regression tests in `tests/rollback_tree.rs`:
  - undo/redo of non-empty directory creation (10 children);
  - undo of a nested tree with 120 files;
  - redo of a supervised directory deletion (the direction that bricked
    workspaces before V-F01 was fixed);
  - interrupted recursive rollback recovery at the directory step
    (physically simulated crash with real quarantine moves);
  - recovery refusal of an unexpected object inside a removed directory,
    followed by `recover --reconcile` preserving the intruder and
    returning to HEALTHY;
  - recovery refusal of an externally modified unprocessed child with
    byte preservation through reconciliation;
  - Windows junction classified as `UNSUPPORTED(WINDOWS_JUNCTION)` with
    undo refused;
  - archive of quarantined bytes verified by content with staging
    cleanup;
  - CLI surface: `rewind run` parses in debug builds, `recover --help`
    exists, and a full CLI init/run e2e passes through the real parser.

Gate results (this environment, Rust stable 1.98.1,
`x86_64-pc-windows-gnu`):

- `cargo fmt --all -- --check`: passed.
- `cargo check --all-targets`: passed.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo test --all-targets --all-features`: 22 passed, 0 failed.
- Linux/macOS cross-checks still cannot compile (bundled SQLite C
  cross-toolchain absent); MSVC linker remains unavailable.

Known gaps that remain verification work, not passing claims:

- Real power-loss durability is not testable here; kill-based crash
  injection and physical quarantine-move simulation are the strongest
  available evidence, plus one real `kill -9` mid-undo of a 300-file tree
  that recovered correctly during Phase 1.1 verification.
- Windows symlink creation remains capability-skipped when the test
  account lacks SeCreateSymbolicLink; the scan of literal symlinks and
  the no-follow guarantees therefore remain code-reviewed only on this
  account.
- The per-step full-workspace rescan makes large rollbacks slow
  (measured: 120-file tree undo ≈ 1–2 minutes); correctness is
  prioritized and no performance claim is made.
