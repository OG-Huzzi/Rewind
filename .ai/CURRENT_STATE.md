# Current Implementation State

Status: Phase 1.2 (final foundation hardening) implemented, tested, and
pushed; CI verification on ubuntu/macos/windows(MSVC) — see
`.ai/PHASE_1_2_HARDENING_REPORT.md` for the recorded verdict.

## Completed

- Phase 0 through Phase 0.7 documentation and architecture finalization.
- Phase 1 implementation: Rust package, external store, SQLite catalog,
  typed scanner, BLAKE3 CAS, strong capture, passive boundary commands,
  degradation/reconciliation, snapshots, journaled rollback, redo,
  recovery inspection, and CLI foundation.
- Phase 1 independent verification (V-F01 P0, V-F02 P1, plus secondary
  defects; see `.ai/PHASE_1_INDEPENDENT_VERIFICATION.md` — the failure
  record is preserved as history).
- Phase 1.1 repairs: transaction-aware rollback expectations (V-F01),
  `rewind recover` / `recover --reconcile` with ABANDONED journal state
  (V-F02), reparse-tag junction classification (V-F03), archive flush
  (V-F04), CLI argument fix (V-F05), fsync durability (V-F06), staging
  cleanup (V-F07), bash/zsh integration (V-F09).
- Phase 1.2 hardening (charter fixes #1–#6, #18, #20):
  - bounded fail-open passive hook (50 ms scan deadline / 150 ms total
    budget; durable bypass markers; deferred recovery is writer work);
  - lock file opened without truncate; owner metadata written only after
    winning the exclusive lock;
  - post-mutation TOCTOU confinement verification on every rollback step;
  - recursive archive verification before any Archived marking or
    quarantine disposal;
  - Windows symlink restoration from recorded reparse target kinds,
    never filename extensions; unknown kinds refuse restoration;
  - schema-versioned state identity (STATE_SCHEMA_VERSION = 2) with v1
    manifests deserializing at `target_kind = unknown`;
  - idempotent journal-authoritative startup repair of committed
    metadata; failed recovery always lands RECOVERY_REQUIRED.
- Test suite: 30 real-filesystem integration tests (13 foundation +
  9 rollback_tree + 8 hardening), all portable across Windows/POSIX via
  `tests/common/mod.rs` script helpers.
- Repo hygiene: `.gitignore`; 5195 tracked `target/` artifacts untracked;
  machine-local `.cargo/config.toml` untracked.
- CI: GitHub Actions (ubuntu-latest, macos-latest, windows-latest MSVC)
  running fmt/check/clippy(-D warnings)/test; failures surface as
  annotations (logs otherwise require admin auth on this repo).
- Measured (not redesigned): 400-file snapshot ≈ 2.5 s; 400-file
  restore/undo ≈ 15.7 min (debug, per-step rescan dominates). Phase 2
  baseline.

## Verification status

- Local gates all green on `x86_64-pc-windows-gnu` (Rust 1.98.1).
- windows-latest (MSVC) CI: passed in full (fmt, check, clippy, 30
  tests) on the first Phase 1.2 run.
- ubuntu/macos: one POSIX-only clippy warning (`unneeded return` in
  `scan::metadata_fingerprint`, not visible to the Windows-host clippy)
  was surfaced via annotations and fixed; final per-platform results
  are in the Phase 1.2 report.

## Not started

- Phase 2 dependency DAG/TUI.
- Phase 3 watcher daemon.
- Phase 4 time/package recipes.
- Phase 5 integrations.

## Known limitations

- Large rollbacks rescan the whole workspace per step (400-file
  restore/undo ≈ 15.7 minutes, debug build, NTFS). Correctness is
  prioritized; no performance guarantee exists. Phase 2 owns the
  redesign; Phase 1.2 measures only.
- True power-loss durability cannot be tested; kill-based crash
  injection and physical quarantine-move simulation are the strongest
  available evidence.
- Windows symlink creation is capability-gated where the account lacks
  SeCreateSymbolicLink (tests skip gracefully; runners typically grant
  it).
- Pre-existing staging directories left by transactions abandoned before
  Phase 1.1 whose archive failed are retained on disk by design (data
  safety) and must be removed manually after inspection.
