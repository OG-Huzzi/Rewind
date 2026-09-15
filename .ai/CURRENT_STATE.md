# Current Implementation State

Status: Phase 1.4 (passive boundary identity) complete and CI-verified
(run #19 on `4b5dddb`: ubuntu, macOS and windows all green); Phase 1 verdict in
`.ai/PHASE_1_4_BOUNDARY_CORRELATION_REPORT.md` (**Phase 1 VERIFIED**).
Phase 2 (dependency-aware inspection) is implemented and locally green but has
not been pushed, so it is not yet CI-verified: see
`.ai/PHASE_2_VERIFICATION_REPORT.md` (**Phase 2 NOT VERIFIED** pending CI).

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
- Phase 1.2 hardening (charter fixes #1–#6, #18, #20): bounded fail-open
  passive hook; lock owner-metadata protection; post-mutation TOCTOU
  confinement verification; recursive archive verification; Windows symlink
  restoration from recorded reparse target kinds; schema-versioned state
  identity; idempotent startup repair; failed recovery lands
  RECOVERY_REQUIRED.
- Phase 1.3 passive-hook isolation: bash/zsh integrations launch the
  post-hook in the background (shell never waits for bookkeeping); the
  false 150 ms budget is removed (scan deadline starts at the scan; a 2 s
  lease retry avoids spurious gates); every hook failure lands in the
  conservative bypass/CAPTURE_FAILED model.
- Phase 1.4 passive boundary identity: the retired "newest unconsumed
  boundary of the session" post-hook lookup is deleted. The pre-hook
  prints the boundary's immutable id, the shell holds it for exactly one
  command, and the post-hook claims exactly that id exactly once
  (guarded `UPDATE` + a `passive_boundary_consume_once` DB trigger).
  Unknown, duplicate, and cross-workspace ids fail open with no side
  effects. Ordering: out-of-order background completion advances the
  trusted checkpoint in lease order only, never regressing it.
- Test suite: 48 real-filesystem integration tests (13 foundation +
  9 rollback_tree + 8 hardening + 10 boundary-correlation [new] +
  8 shell-integration), all portable across Windows/POSIX.
- CI: GitHub Actions (ubuntu-latest, macos-latest, windows-latest MSVC)
  running fmt/check/clippy(-D warnings)/test per platform.

## Verification status

- Local gates all green on `x86_64-pc-windows-gnu` (Rust 1.98.1): fmt, check,
  clippy (-D warnings), and the full 48-test suite run repeatedly.
- Phase 1.4 CI is green for `4b5dddb` (run #19: ubuntu, macOS, windows/MSVC).
  The two preceding revisions had failed on macOS, and `b69817d` also on
  windows; see the Phase 1.4 report section 11 for the per-platform results.
- Phase 2 local gates are green on `x86_64-pc-windows-gnu`: fmt, check, clippy
  (-D warnings) and the full suite, 76 passed / 0 failed (Phase 1's 48 unchanged
  plus 25 new Phase 2 tests). CI has not run for any Phase 2 commit.
- zsh coverage runs where zsh is available (macOS/Linux CI) and reports an
  honest skip on windows-latest.

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
- Phase 1.3 passive-hook isolation: bash/zsh integrations launch the
  post-hook in the background (shell never waits for bookkeeping); the
  false 150 ms budget is removed (scan deadline starts at the scan; a 2 s
  lease retry avoids spurious gates); every hook failure lands in the
  conservative bypass/CAPTURE_FAILED model.
- Phase 1.4 passive boundary identity: the retired "newest unconsumed
  boundary of the session" post-hook lookup is deleted. The pre-hook
  prints the boundary's immutable id, the shell holds it for exactly one
  command, and the post-hook claims exactly that id exactly once
  (guarded `UPDATE` + a `passive_boundary_consume_once` DB trigger).
  Unknown, duplicate, and cross-workspace ids fail open with no side
  effects. Ordering: out-of-order background completion advances the
  trusted checkpoint in lease order only, never regressing it.
- Test suite: 48 real-filesystem integration tests (13 foundation +
  9 rollback_tree + 8 hardening + 10 boundary-correlation [new] +
  8 shell-integration), all portable across Windows/POSIX.
- Repo hygiene: `.gitignore`; 5195 tracked `target/` artifacts untracked;
  machine-local `.cargo/config.toml` untracked.
- CI: GitHub Actions (ubuntu-latest, macos-latest, windows-latest MSVC)
  running fmt/check/clippy(-D warnings)/test; failures surface as
  annotations (logs otherwise require admin auth on this repo).
- Measured (not redesigned): 400-file snapshot ≈ 2.5 s; 400-file
  restore/undo ≈ 15.7 min (debug, per-step rescan dominates). Phase 2
  baseline.

## Verification status

- Local gates all green on `x86_64-pc-windows-gnu` (Rust 1.98.1): fmt,
  check, clippy (-D warnings), and the full 48-test suite run repeatedly.
- Phase 1.3 CI (ubuntu, macos, windows/MSVC) was green for the Phase 1.3
  commit; the Phase 1.4 commit is verified locally and CI is observed on
  push (see the Phase 1.4 report §11 for the final per-platform results).
- zsh coverage runs where zsh is available (macOS/Linux CI) and reports an
  honest skip on windows-latest.
- ubuntu/macos: one POSIX-only clippy warning (`unneeded return` in
  `scan::metadata_fingerprint`, not visible to the Windows-host clippy)
  was surfaced via annotations and fixed in Phase 1.2; final per-platform
  results are in the Phase 1.2 report.

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
- The recorded boundary `cwd` is the workspace root (marker discovery), not
  the shell's literal `$PWD`; command/exit association is exact. A bash
  compound line (`A; B`) mints one boundary per simple command while a single
  post-hook completes the last; Rewind owns the bash `DEBUG` trap. See the
  Phase 1.4 report §12 for the full list; none of these is a defect.
