# Implementation Changelog

## Unreleased

- Phase 3 — continuous observation (complete and CI-verified, run
  #36111890265 on `d69fdca`):
  added the optional advisory watcher under `src/watch/` — platform-neutral
  `FsEvent` model, inotify/FSEvents/ReadDirectoryChangesW adapters behind
  an `EventAdapter` trait plus a deterministic fake adapter, a serve loop
  with batching, bounded coalescing into a dirty-path index, raw-evidence
  event log with rotation, heartbeat lifecycle, and durable degradation
  records. Added `rewind watch start/status/stop`. Any prior watcher run
  (including a graceful stop) records an unobserved gap on restart;
  overflow, adapter failure, and the dirty cap degrade immediately and are
  converted into unknown intervals + RECONCILIATION_REQUIRED by the
  existing single-writer enforcement points, closable only by
  reconciliation. The watcher never opens the catalog, takes no lease, and
  never writes inside the workspace. Windows detachment uses a raw
  `CreateProcessW` with `bInheritHandles = FALSE` because std cannot
  restrict handle inheritance (rust-lang/rust#73281) — without it a
  pipe-captured `rewind watch start` hangs forever. Full suite 119
  passed / 0 failed; all Phase 1/2 suites unchanged.
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

## Phase 2 - dependency-aware inspection

Contract: `.ai/PHASE_2_DEPENDENCY_AWARE_INSPECTION.md`.

- Read-only `Catalog::unknown_intervals` returning typed `UnknownIntervalRow`
  rows ordered by `(created_at, id)`. No schema change: the crate has no
  migration mechanism, so a new column or table would silently not apply to
  existing catalogs.
- `ScanOptions { ingest }` with `Workspace::observe`: the same manifest and
  `state_id` as `scan`, computed without writing anything into the CAS. `Default`
  keeps ingestion on, so every Phase 1 capture path is unchanged.
- `src/depgraph.rs`: a derived dependency graph over recorded history. Nodes are
  existing operation/state/boundary ids. Three typed evidence kinds - state
  lineage (Known: entailed by a shared content hash), effect overlap and temporal
  ordering (Advisory: correlations that never widen a closure). Cycles are
  detected, reported rotated to their smallest node, and never broken. Evidence
  that cannot be evaluated is recorded, so "could not tell" is never reported as
  "no dependency".
- `src/plan.rs`: safety-first multi-select rollback planning. Targets are
  validated with the Phase 1 eligibility predicates, the closure is computed on
  known lineage order, and refusal is a value - ten structured conflicts and ten
  block reasons carrying the durable ids that caused them. An open unknown
  interval blocks unconditionally. `plan::execute` re-plans, refuses stale plans
  and drives the existing Phase 1 undo/redo engine.
- CLI: `inspect graph`, `inspect history`, `plan rollback` (all read-only, with
  `--json`) and a separate `apply rollback`. Exit 0 when executable, 3 when
  refused, 1 on error.
- `src/ui.rs` (`rewind ui`): the minimum read-only interactive surface. `parse`
  and `render` are pure, so it is testable without a terminal; the mutating verbs
  are refused by construction, so it cannot decide or execute anything.
- Tests: 25 new (13 unit + 12 integration), covering closure order, refusal
  conditions, determinism under shuffled input, zero mutation during planning and
  execution through the Phase 1 engine. Full suite 76 passed / 0 failed.
- CI: run #23 on `4d89a2f` is green on ubuntu-latest, macos-latest and
  windows-latest. Runs #21 and #22 failed on ubuntu only and each was fixed at
  the root: a missing POSIX shebang in the new tests, then a wait loop that had
  to require both claiming and settlement.
- Verdict: **PHASE 2 VERIFIED**. See `.ai/PHASE_2_VERIFICATION_REPORT.md`.

## Phase 1.4 — passive boundary identity and final verification
- Post-CI follow-ups: `06cb33b` (the 50 ms scan deadline now starts immediately
  before the scan; the observation-path tests assert both branches; the report's
  CI and verdict record was corrected to the runs actually observed) and
  `4b5dddb` (the rapid-command shell test waits for the bookkeeping to settle
  rather than for boundaries to be claimed, and the durable-trace predicate
  matches the product gate `condition != HEALTHY`). CI run #19 on `4b5dddb` is
  green on ubuntu-latest, macos-latest and windows-latest.
- P1 concurrency fix: the post-hook's "newest unconsumed boundary of the
  session" lookup (`pending_boundary`, deleted) could consume another
  command's boundary once background post-hooks coexist. The pre-hook now
  prints the boundary's immutable id, the bash/zsh integrations hold it for
  exactly one command, and the post-hook requires `--boundary` and claims
  exactly that id exactly once (guarded `UPDATE ... consumed = 0`).
- Unknown, already-consumed, and cross-workspace ids fail open with a
  diagnostic and no side effects; a boundary can never be consumed by a
  different workspace or twice.
- Database-enforced invariants: `passive_boundary_consume_once` trigger
  (installed on every open, including pre-existing catalogs) refuses to
  rewrite or resurrect an accounted-for boundary; `CHECK (consumed IN (0, 1))`
  on new catalogs; `BoundaryRow` exposes `ended_at`/`exit_code`/`consumed`.
- Ordering rule: out-of-order background completion advances the trusted
  checkpoint in lease order only — an older background observation finishing
  later can never regress newer trusted state.
- New `tests/boundary_correlation.rs` (10 deterministic tests): older-post
  ordering, genuine three-way overlap behind a lease barrier, duplicate /
  unknown / cross-workspace ids, writer-lease deferral, two-session
  provenance, and SQL-level negative assertions. The key regression test was
  shown to FAIL against the retired lookup and PASS with the fix.
- Real-shell coverage: the stub tests now also prove the wrapper hands the
  pre-hook's id to the post-hook; new per-shell rapid-command tests assert
  A→A / B→B / C→C with exact exit codes through the actual integrations.
- Full suite is 48 tests, all green locally; CI observed on all platforms.
  No Phase 2 functionality was introduced.

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
