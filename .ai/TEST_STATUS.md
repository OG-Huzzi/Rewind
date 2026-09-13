# Phase 1 Test Status

Status after Phase 1.3 (passive hook isolation and final verification).
Historical Phase 1.1 status is preserved at the bottom.

The suite uses real temporary workspaces, real filesystem mutations, real
subprocess writers, and real crash simulation. No mocked filesystems.

Passed locally (Rust stable 1.98.1, `x86_64-pc-windows-gnu` host,
NTFS): 36 integration tests, 0 failed.

- 13 foundation tests (portable command vectors: `.cmd` via `cmd /C` on
  Windows, executable `.sh` on POSIX; CRLF/LF handled by `common::echoed`).
- 9 rollback_tree tests (V-F01/V-F02 regressions: non-empty/nested tree
  undo and redo, interrupted rollback recovery, external-modification
  refusals, junction classification, archive verification, CLI e2e).
- 6 shell-integration tests (Phase 1.3, `tests/shell_integration.rs`):
  the shell wrapper exits while a stub 45 s post-hook is still running
  (threshold-free ordering proof; bash everywhere, zsh where the runner
  ships it — windows-latest reports the skip honestly); real-wrapper
  observation/degradation flows; busy catalog, terminated hook process,
  missing workspace, and unwritable CAS all fail open and gate the next
  writer without fabrication.
- 8 hardening tests (Phase 1.2 fixes, `tests/hardening.rs`):
  - lock owner metadata survives a failed contender (in-process, and
    against a real cross-process `rewind run` writer);
  - bounded passive hook fails open on a real 1500-file workspace:
    exit 0 in bounded time, CAPTURE_FAILED + unknown interval recorded,
    undo refused, `reconcile` restores HEALTHY with no fabricated
    STRONG operation;
  - the hook defers unfinished-transaction recovery to the writer and
    records a durable bypass marker that gates reconcile-first;
  - committed-metadata crash window repairs idempotently (journal is
    authority, catalog rows converge, second repair is a no-op);
  - recursive archive verification catches nested corruption, missing
    and extra entries, wrong directory shape, type mismatch, and wrong
    symlink targets (POSIX);
  - v1 manifests (no `target_kind`) deserialize with unknown kind, and
    state ids hash the schema-versioned envelope;
  - recovery failure never leaves a false HEALTHY, and
    `recover --reconcile` archives, marks ABANDONED, and checkpoints;
  - symlinked-parent rollback refusal (POSIX): external target never
    followed (real symlink to outside the workspace);
  - symlink target kinds recorded from evidence and restore faithfully
    (POSIX: file, directory, dangling).

Local gates: `cargo fmt --all -- --check`, `cargo check --all-targets`,
`cargo clippy --all-targets -- -D warnings`, `cargo test` — all passing.

CI (GitHub Actions, `OG-Huzzi/Rewind`): ubuntu-latest, macos-latest,
windows-latest (MSVC host) — fmt/check/clippy(-D warnings)/test per
platform. windows-latest (MSVC) passed in full on the first Phase 1.2
run (compile, clippy, and all 30 tests). Initial runs surfaced one
POSIX-only clippy warning (`unneeded return` in `metadata_fingerprint`,
invisible on the Windows host clippy) — fixed; the final per-platform
results are recorded in `.ai/PHASE_1_2_HARDENING_REPORT.md`.

Measured performance (documented per charter; no performance redesign):

- 400-file tree: snapshot ≈ 2.5 s per invocation (scan + CAS);
  restore/undo of 400 modified files ≈ 15.7 minutes in the debug build
  (per-step full-workspace rescan dominates). Recorded as the Phase 2
  baseline; the Phase 1.2 charter forbids performance redesign.

Known gaps that remain verification work, not passing claims:

- Real power-loss durability is not testable in CI; kill-based crash
  injection and physical quarantine-move simulation are the strongest
  available evidence, plus one real `kill -9` mid-undo of a 300-file
  tree that recovered correctly during Phase 1.1 verification.
- Windows symlink creation is capability-skipped when the runner account
  lacks SeCreateSymbolicLink (GitHub-hosted runners grant it; local
  accounts may not). The graceful-skip path is itself asserted.
- The TOCTOU confinement check narrows the symlink-swap window to the
  interval between the final pre-mutation check and the mutation itself
  (accepted residual race under the Phase 0.7 conditional guarantee).

---

## Phase 1.1 status (historical, 2026-09)

Passed: 22 real-filesystem tests.

- 13 foundation tests (Phase 1 suite).
- 9 rollback_tree regression tests: non-empty/nested/large tree undo and
  redo, interrupted recursive rollback recovery, external-modification
  refusals, junction classification, archive verification with staging
  cleanup, CLI parse/e2e coverage.

Gates at that time: fmt/check/clippy(-D warnings) passed; tests 22/22 on
`x86_64-pc-windows-gnu`; Linux/macOS cross-checks could not compile
(bundled SQLite C cross-toolchain absent) and MSVC was unavailable —
which is exactly what Phase 1.2 CI now covers.
