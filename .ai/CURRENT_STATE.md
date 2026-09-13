# Current Implementation State

Status: Phase 1.1 corrective phase complete; Phase 1 implementation under
re-verification

## Completed

- Phase 0 through Phase 0.7 documentation and architecture finalization.
- Phase 1 implementation map and repository/toolchain audit.
- Rust stable toolchain installed locally for verification.
- Rust package, external store, SQLite catalog, typed scanner, BLAKE3 CAS,
  strong capture, passive boundary commands, degradation/reconciliation,
  snapshots, journaled rollback, redo, recovery inspection, and CLI foundation.
- Real filesystem tests for capture, degradation, state replacement, conflict,
  snapshots, CAS integrity, directory mutation, and symlink capability.
- Phase 1 independent verification (found V-F01 P0, V-F02 P1, plus secondary
  defects; see `.ai/PHASE_1_INDEPENDENT_VERIFICATION.md`).
- Phase 1.1 repairs: transaction-aware rollback expectations (V-F01),
  `rewind recover` / `recover --reconcile` exit path with ABANDONED journal
  state (V-F02), reparse-tag junction classification (V-F03), archive flush
  through a write handle (V-F04), CLI argument fix (V-F05), journal/pointer
  fsync durability (V-F06), staging cleanup (V-F07), bash/zsh shell
  integration (V-F09).
- Phase 1.1 regression suite: 9 additional real-filesystem tests
  (22 total passing).

## Verification in progress

- 38-scenario matrix rerun after the Phase 1.1 fixes.
- Independent re-verification of the Phase 1.1 repairs.
- Native MSVC, Linux, and macOS toolchains remain unavailable.

## Not started

- Phase 2 dependency DAG/TUI.
- Phase 3 watcher daemon.
- Phase 4 time/package recipes.
- Phase 5 integrations.

## Known limitations

- Large rollbacks rescan the whole workspace per step (measured: 120-file
  tree ≈ 1–2 minutes, 400-file tree ≈ 8 minutes in debug/release CLI on
  NTFS). Correctness is prioritized; no performance guarantee exists.
- True power-loss durability cannot be tested in this environment.
- Windows symlink creation is capability-gated on this account.
- Pre-existing staging directories left by transactions abandoned before
  Phase 1.1 whose archive failed are retained on disk by design (data
  safety) and must be removed manually after inspection.
