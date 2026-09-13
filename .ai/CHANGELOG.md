# Implementation Changelog

## Unreleased

- Began Phase 1 implementation after the locked Phase 0.7 contract.
- Recorded the implementation map and verification environment.
- Added the external workspace store, SQLite catalog, typed manifests, BLAKE3
  CAS, strong/passive capture, degradation gates, reconciliation, snapshots,
  same-filesystem rollback/redo, journal recovery, and diagnostics CLI.
- Added real filesystem tests and corrected parent-directory rollback planning,
  Windows sync tolerance, reparse refusal, and interrupted CAS cleanup.

## Phase 1.1 — rollback correctness and recovery repair

Fixes for the independent verification findings in
`.ai/PHASE_1_INDEPENDENT_VERIFICATION.md`:

- V-F01 (P0): the rollback planner now records the state each step is
  expected to be in at execution time. A directory's expected fingerprint is
  narrowed to the children that survive in the target state, so a
  transaction's own earlier child removals are no longer misread as external
  conflicts. Undo/redo of non-empty directory trees and interrupted
  recursive rollbacks complete and recover correctly; unexpected external
  objects still trip the conflict gate.
- V-F02 (P1): added `rewind recover` (automatic classification) and
  `rewind recover --reconcile` (explicit abandon-and-reconcile exit path
  with artifact archival and a recorded ABANDONED journal state plus an
  unknown interval). Doctor and refusal messages now point at the real
  commands.
- V-F03 (P2): Windows reparse objects are classified by reading the actual
  reparse tag (FSCTL_GET_REPARSE_POINT). Junctions are
  `UNSUPPORTED(WINDOWS_JUNCTION)`; only a positively identified symlink is
  treated as SYMLINK; unreadable tags are refused.
- V-F04 (P2): post-commit archive copies are flushed through a write handle
  (FlushFileBuffers requires write access), so quarantined bytes actually
  reach the external archive.
- V-F05 (P2): removed the invalid clap `last` + `trailing_var_arg`
  combination that panicked `rewind run` in debug builds.
- V-F06 (P2): `atomic_write` now flushes the payload before publication and
  requests directory-entry durability where the platform supports it;
  journal, pointer, and boundary-log writes are durable.
- V-F07 (P3): transaction staging is removed after commit once the archive
  holds every quarantined artifact; staging that still holds the only copy
  of quarantined bytes is retained.
- V-F09 (P3): shipped `integration/rewind.bash` and `integration/rewind.zsh`
  passive-hook integration with automatic per-shell session ids; the user no
  longer has to invent session identifiers.
- Preserve-step backup paths are no longer planned for directory-to-directory
  steps that never quarantine, and interrupted-transaction archives skip
  never-executed steps, so the post-commit archive reports real failures
  only.
- Added the `tests/rollback_tree.rs` regression suite (9 real-filesystem
  tests): non-empty/nested/large tree undo and redo, interrupted recursive
  rollback recovery, genuine external modifications during rollback,
  junction classification, archive verification with staging cleanup, and
  CLI parse/e2e coverage.

## Phase 1.2 — final foundation hardening and cross-platform verification

Fixes for the Phase 1.2 hardening charter (repo live at
`OG-Huzzi/Rewind`):

- Fix #1: the passive hook is bounded and fail-open. The scan runs under a
  50 ms deadline and the whole post-hook path (workspace open, lease,
  scan, persistence) under a 150 ms total budget. Overruns append durable
  bypass markers (log + catalog) and exit 0; a deadline-exceeded scan
  records CAPTURE_FAILED and an unknown interval instead of fabricating
  an observation. Deferred transaction recovery is never run inline in
  the hook — a hook that finds an unfinished transaction records a bypass
  marker and returns; recovery is writer work.
- Fix #2: the writer lease opens `lock.pid` without truncation, acquires
  the exclusive lock, and only then replaces the owner metadata. A
  contender that loses the lock race can no longer zero the holder's
  owner record.
- Fix #3: every rollback step re-verifies path confinement *after* its
  mutation (quarantine move, file/dir install, symlink install), so a
  parent chain swapped to a symlink mid-transaction is detected instead
  of written through. The symlinked-parent preflight refusal stands; the
  post-mutation check narrows the TOCTOU window to the interval between
  the final check and the mutation itself (documented residual race per
  the Phase 0.7 conditional guarantee).
- Fix #4: archives are verified recursively (type, size, BLAKE3 content
  for files; literal target for symlinks; name-set equality plus
  recursion for directories) before the transaction is marked Archived
  and before local quarantine staging is disposed. A shallow pass can
  never authorize deleting the last surviving copy.
- Fix #5: Windows symlink restoration reads the recorded target kind
  (from reparse data at scan time), never a filename extension; unknown
  or externally-inferred kinds refuse restoration.
- Fix #6: state identity is schema-versioned (envelope
  `{schema_version, entries}` digested with BLAKE3; STATE_SCHEMA_VERSION
  = 2). Manifests persisted before v2 deserialize with
  `target_kind = unknown`, so existing states stay readable; fingerprint
  semantics changes can never silently alias old state ids.
- Fix #18: startup repair of committed-transaction metadata is idempotent
  and journal-authoritative. A crash between the journal COMMITTED write
  and the catalog updates converges on the next open (transaction row,
  operation status by direction, baseline/condition when the live scan
  confirms the target state). It never mutates the filesystem.
- Fix #20: when recovery of an unfinished transaction fails, the
  workspace is set RECOVERY_REQUIRED anchored at that transaction's
  state before the error propagates — no path can leave a failed
  recovery looking HEALTHY. `recover --reconcile` remains the explicit
  archive/abandon/checkpoint exit.
- Test portability: all test suites now build platform-native scripts
  (`cmd /C` .cmd on Windows, executable .sh on POSIX) through
  `tests/common/mod.rs`; content assertions account for CRLF vs LF.
- Test suite: added `tests/hardening.rs` (8 real-filesystem regression
  tests covering every fix above, including a real 1500-file bounded-hook
  overrun, a real cross-process lease contender, and v1-manifest
  backward compatibility). 30 integration tests total, all passing.
- Repo hygiene: added `.gitignore`; untracked 5195 committed `target/`
  build artifacts and the machine-local `.cargo/config.toml`
  windows-gnu linker pin.
- CI: GitHub Actions workflow (ubuntu-latest, macos-latest,
  windows-latest/MSVC) running fmt --check, check --all-targets, clippy
  -D warnings, and the full test suite per platform.
- Measured (not redesigned, per charter): 400-file tree snapshot ≈ 2.5 s
  per snapshot; restore/undo of a 400-file tree ≈ 15.7 minutes (debug
  build, NTFS) — dominated by the per-step full-workspace rescan. The
  Phase 1.2 charter forbids performance redesign; the measurement is
  recorded as the Phase 2 baseline.

## Phase 1.3 — passive hook isolation and final verification

- Shell integration (bash/zsh) launches `rewind hook post` in the
  background (nohup, detached): the interactive shell returns to the
  prompt without waiting for scan/CAS/SQLite/recovery/archive work. The
  pre-hook stays synchronous and lightweight (one boundary INSERT).
- Removed the false 150 ms total-budget promise (`HOOK_BUDGET_MS`); the
  only in-process bound is the 50 ms scan deadline, which now starts when
  the scan begins (a slow workspace open no longer consumes it).
- Overlapping background hooks retry the writer lease for 2 s before the
  durable bypass fallback, so rapid typing no longer causes spurious
  reconciliation gates; a writer mid-rollback still gates.
- Any hook error records a durable bypass marker before failing; hook
  process termination leaves at worst an unconsumed boundary; the next
  writer's reconcile-first pre-scan catches all unobserved drift.
- New `tests/shell_integration.rs` (6 tests) proves the real user
  experience: threshold-free ordering proof that the shell exits while a
  stub 45 s post-hook is still running (bash; zsh where the runner
  provides it, honest skip otherwise), real-wrapper observation and
  degradation flows, busy catalog, terminated hook, missing workspace,
  and unwritable CAS. The old "< 5 s" hook timing assertion was replaced
  by these behavioral tests.
- README rewritten as clean UTF-8 (was UTF-16 with a BOM).
  `.gitattributes` pins LF for shell integration files.
