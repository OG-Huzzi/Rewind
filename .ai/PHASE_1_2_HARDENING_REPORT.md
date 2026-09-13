# Phase 1.2 — Final Foundation Hardening Report

Scope: the Phase 1.2 charter (fixes #1–#6, #18, #20; repo hygiene; GitHub
Actions CI; 38-scenario matrix rerun; measured-not-redesigned rollback
performance; docs). The Phase 0.7 architecture lock and the Phase 1.1
repair semantics are unchanged. The Phase 1 independent-verification
failure record (`.ai/PHASE_1_INDEPENDENT_VERIFICATION.md`) and the
Phase 1.1 repair report remain preserved as history.

**Verdict: PHASE 1 CONDITIONALLY VERIFIED.** All charter fixes are
implemented with real-filesystem regression tests; the full suite (30
integration tests) is green locally on the Windows host and green in
GitHub Actions CI on ubuntu-latest, macos-latest, and windows-latest
(MSVC) — fmt, check, clippy `-D warnings`, and tests on each. The
conditions are enumerated in §8 (scenarios that cannot be fault-injected
in CI, capability-gated Windows symlink creation, the documented residual
TOCTOU window, and the unredesigned per-step rescan cost). No matrix
scenario is FAILED.

## 1. Charter fixes: implementation and evidence

### Fix #1 — bounded fail-open passive hook
- Scan runs under a 50 ms deadline (`HOOK_SCAN_DEADLINE_MS`); the whole
  post-hook path (workspace open, pending-boundary lookup, lease
  acquisition, scan, persistence) runs under a 150 ms total budget
  (`HOOK_BUDGET_MS`). Overrun of either appends a durable bypass marker
  (append-only `boundaries.log` with fsync + catalog row) and exits 0 —
  the shell never waits.
- A scan that hits its deadline records CAPTURE_FAILED plus an open
  unknown interval (via `record_capture_failure`) instead of fabricating
  an observation. The next writer converts bypass markers into the
  reconcile-first gate (`enforce_pending_safety_gate`).
- Deferred recovery is never run inline: `hook_post_locked` sees an
  unfinished transaction, records a bypass marker, and returns. Recovery
  is writer work (`run_command`/`undo`/`redo` call `recover_locked`).
- Evidence: `passive_hook_fails_open_within_budget_on_slow_workspace`
  (real 1500-file workspace, real CAS+SQLite pressure; asserts exit 0,
  bounded wall time, RECONCILIATION_REQUIRED, unknown interval, undo
  refusal, reconcile → HEALTHY, no STRONG operation);
  `passive_hook_defers_recovery_to_writer` (crafted interrupted
  transaction; hook defers, durable bypass marker present, gate
  reconciles-first).

### Fix #2 — lock acquisition order
- `WorkspaceLease::acquire` opens `lock.pid` with
  `create+read+write` and **no truncate**, acquires the exclusive lock,
  and only then `set_len(0)` + writes the owner record + `sync_all`.
- Evidence: `lock_owner_metadata_survives_contended_acquire`
  (in-process contender; on Windows the holder's record survives the
  failed acquire — verifiable only after release because the whole-file
  byte-range lock blocks all readers while held, which the test
  asserts); `lock_owner_metadata_survives_cross_process_writer` (a real
  spawned `rewind run` writer holds the lease; the contender fails to
  acquire, readers are blocked while held, and the writer's `pid=`
  owner record is intact afterward).

### Fix #3 — TOCTOU path confinement
- `verify_mutation_confined` runs after every destructive mutation in
  `apply_step`: after the quarantine rename (leaf must be gone), after
  file/directory installs (leaf must exist and not be a symlink), and
  for symlink installs the literal link target is re-read and compared.
  Parent chains are re-walked with `symlink_metadata` (no follow).
- Residual window: the interval between the final pre-mutation check and
  the mutation itself — accepted by the Phase 0.7 conditional
  guarantee and documented (HANDOFF §0).
- Evidence: `rollback_refuses_symlinked_parent_and_never_follows_it`
  (POSIX CI; live path swapped to a symlink pointing *outside* the
  workspace; undo refused, external target bytes untouched, link
  intact); symlink install target equality in `apply_step`.

### Fix #4 — recursive archive verification
- `verify_archive_pair` recursively verifies: file type, size, and
  BLAKE3 content (128 KiB streaming); symlink literal target equality;
  directory name-set equality (missing *and* unexpected entries) with
  recursion. `archive_committed_transaction` marks Archived and disposes
  of transaction-local staging only after this passes.
- Evidence: `recursive_archive_verification_detects_nested_corruption`
  (faithful copy passes; nested content corruption, missing/extra
  nested entries, wrong directory shape, file→directory type mismatch,
  and wrong symlink target each fail verification).

### Fix #5 — Windows symlink target kinds
- Scan records `target_kind` from the authoritative reparse data
  (`FSCTL_GET_REPARSE_POINT`, `SYMLINK_FLAG_DIRECTORY`), refined by
  no-follow workspace-internal target resolution for POSIX evidence.
  Restoration uses the recorded kind (`symlink_file` vs `symlink_dir`);
  `Unknown` refuses restoration instead of guessing. No filename
  extension is consulted anywhere.
- Evidence: code path + `symlink_target_kinds_are_recorded_and_restore_faithfully`
  (POSIX CI: file, directory, and dangling links record File / Directory
  / Unknown); Windows junctions remain UNSUPPORTED (rollback_tree).

### Fix #6 — schema-versioned state identity
- State ids digest a versioned envelope
  `{"schema_version":2,"entries":{…}}` (`STATE_SCHEMA_VERSION = 2`);
  `Fingerprint::Symlink.target_kind` is `#[serde(default)]` with
  `Default = Unknown`, so v1 manifests deserialize unchanged. Existing
  persisted states keep their stored ids and stay readable; a v1→v2
  drift resolves through the explicit reconciliation checkpoint, never
  silent invalidation.
- Evidence: `state_identity_is_schema_versioned_and_v1_manifests_stay_readable`
  (v1 JSON decodes with `Unknown`; state id ≠ bare-entries digest;
  deterministic).

### Fix #18 — committed-metadata crash-window repair
- `Workspace::repair_committed_metadata` runs on every writer entry:
  for each Committed journal it forces the catalog transaction row to
  COMMITTED, sets the operation status by journal direction
  (UNDO→Undone), and repairs baseline/condition onto the journal's
  target state **only when the live scan's state id equals the target**.
  It never mutates the filesystem and is idempotent.
- Evidence: `committed_metadata_crash_window_repairs_idempotently`
  (catalog rows reverted to a contradictory crash-window state; first
  repair converges; second repair is a no-op).

### Fix #20 — recovery failure states
- `recover_locked`: when recovery of an unfinished transaction cannot
  proceed, the workspace is set RECOVERY_REQUIRED anchored at that
  transaction's state **before** the error propagates — no path leaves
  a failed recovery looking HEALTHY. Ambiguity classes remain
  distinguishable (conflict vs storage error vs unknown state).
- Evidence: `recovery_failure_never_leaves_false_healthy_and_reconcile_resolves`
  (clobbered quarantine artifact → recovery refuses, condition is
  RECOVERY_REQUIRED, `recover --reconcile` archives, marks ABANDONED,
  reconciles to a trusted checkpoint).

## 2. Defects CI surfaced (this phase's independent value)

The local host is `x86_64-pc-windows-gnu`; POSIX-only behavior is
invisible to it. CI caught, in order:

1. **POSIX clippy `-D warnings` failures** (2): `unneeded return` under
   `#[cfg(unix)]` in `scan::metadata_fingerprint` and
   `tests/common`'s symlink helper. Windows clippy never compiled those
   blocks. Fixed; clippy is now clean on all three platforms.
2. **A real POSIX rollback bug (P1)**: on Unix,
   `Permissions::set_readonly(false)` ORs `0o222` into the mode. The
   undo path restored the recorded 0o644 via `set_mode` and then called
   `set_readonly(false)`, producing 0o666; the post-install verification
   scan (correctly) refused the state and every file-restore undo/redo
   failed with `RecoveryRequired` on ubuntu/macos. Windows never sees
   this (mode is `None`). Fix: the recorded mode is authoritative on
   Unix; `readonly` only ever *clears* write bits. Diagnosed via CI
   check-run annotations after enriching the post-install error to
   report both fingerprints (expected mode 420 vs found 438).
3. **Test-encoding bugs**: four hardcoded `"\r\n"`/`b"…\r\n"` content
   assertions replaced with the platform-neutral `common::echoed`
   helper.

CI runs (in order): aa6a490 (windows ✓, ubuntu/macos clippy ✗) →
23bb251 (annotation plumbing) → a4c5eb8 (color-off annotations) →
739b565 (lib clippy fix; test-crate clippy still ✗) → 9c67cd7 (test
clippy fix; POSIX tests fail) → 39b0ab1 + 9205e88 (diagnostics) →
c5cd938 (full fingerprints) → 26230d7 (**Unix mode fix**) → 0e5b1b2
(CRLF test fixes) → **15038c4: all three platforms green**.

## 3. Test suite

30 real-filesystem integration tests, all green locally and in CI:

- `tests/foundation.rs` — 13 tests (portable command vectors).
- `tests/rollback_tree.rs` — 9 tests (V-F01/V-F02 regressions incl.
  non-empty/nested tree undo+redo, interrupted rollback recovery,
  junction classification, archive verification, CLI e2e).
- `tests/hardening.rs` — 8 new tests covering every charter fix (§1).
- `tests/common/mod.rs` — portability helpers (`shell_script` writes
  `.cmd` run via `cmd /C` on Windows and executable `.sh` on POSIX;
  `echoed` normalizes CRLF/LF content assertions; symlink capability
  detection for error 1314).

## 4. Local gates (Rust 1.98.1, x86_64-pc-windows-gnu, NTFS)

- `cargo fmt --all -- --check`: PASS
- `cargo check --all-targets`: PASS
- `cargo clippy --all-targets -- -D warnings`: PASS
- `cargo test`: 30 passed, 0 failed

## 5. CI (GitHub Actions, OG-Huzzi/Rewind)

Workflow `.github/workflows/ci.yml`: matrix ubuntu-latest / macos-latest
/ windows-latest (default MSVC toolchain), each running fmt --check,
check --all-targets, clippy -D warnings, cargo test; failing clippy/test
output is re-emitted as check annotations (raw logs need repo-admin
auth, which this environment lacks).

**Final run 34759834037 (commit 15038c4): success on all three
platforms** — including the required Windows MSVC verification.
SQLite is compiled from source (`rusqlite` bundled), so no platform
packages are needed.

## 6. 38-Scenario Matrix (Phase 1.2 rerun)

| ID | Phase 1.1 | Phase 1.2 | Evidence / change |
| --- | --- | --- | --- |
| T01 | TESTED AND PASSED | TESTED AND PASSED | unchanged; suite green |
| T02 | TESTED AND PASSED | TESTED AND PASSED | + hardening hook tests (bounded, fail-open) |
| T03 | TESTED AND PASSED | TESTED AND PASSED | reconcile → HEALTHY (hardening test path) |
| T04 | TESTED AND PASSED | TESTED AND PASSED | unchanged |
| T05 | TESTED AND PASSED | TESTED AND PASSED | unchanged |
| T06 | TESTED AND PASSED | TESTED AND PASSED | unchanged |
| T07 | TESTED AND PASSED | TESTED AND PASSED | unchanged |
| T08 | TESTED AND PASSED | TESTED AND PASSED | unchanged |
| T09 | TESTED AND PASSED | TESTED AND PASSED | unchanged |
| T10 | TESTED AND PASSED | TESTED AND PASSED | rollback_tree |
| T11 | TESTED AND PASSED | TESTED AND PASSED | rollback_tree |
| T12 symlink→file | PLATFORM UNAVAILABLE | TESTED AND PASSED (POSIX) | ubuntu/macos CI create + record + restore symlinks; Windows CI capability-gated skip asserted |
| T13 file→symlink | PLATFORM UNAVAILABLE | TESTED AND PASSED (POSIX) | same |
| T14 | TESTED AND PASSED | TESTED AND PASSED | recursive verification added (Fix #4) |
| T15 crash before quarantine move | PARTIAL | PARTIAL | unchanged; PLANNED retry proven, real pre-move kill not isolated |
| T16 | TESTED AND PASSED | TESTED AND PASSED | unchanged |
| T17 | TESTED AND PASSED | TESTED AND PASSED | unchanged |
| T18 | TESTED AND PASSED | TESTED AND PASSED | unchanged |
| T19 | TESTED AND PASSED | TESTED AND PASSED | rollback_tree |
| T20 | TESTED AND PASSED | TESTED AND PASSED | unchanged |
| T21 | TESTED AND PASSED | TESTED AND PASSED | unchanged |
| T22 | TESTED AND PASSED | TESTED AND PASSED | unchanged |
| T23 | TESTED AND PASSED (deviation) | TESTED AND PASSED (deviation) | single-writer serialization documented |
| T24 | TESTED AND PASSED | TESTED AND PASSED | + hook deferral and bypass-marker tests (Fix #1) |
| T25 IDE edits between preflight and apply | PARTIAL | PARTIAL | per-step re-scan + conflict refusal proven; live race not fault-injected |
| T26 external edit of non-target during rollback | PARTIAL | PARTIAL | refusal proven; non-target drift path reviewed |
| T27 Windows exclusive handle on target | PARTIAL | PARTIAL | scan-side refusal tested; rollback-side sharing refusal code-reviewed |
| T28 workspace moved across volumes during recovery | NOT TESTED | NOT TESTED | adjacent Fix #6 (state-identity versioning) tested; root-identity revalidation itself unchanged |
| T29 symlink points outside during scan | PLATFORM UNAVAILABLE | TESTED AND PASSED (POSIX) | `rollback_refuses_symlinked_parent…`: external target bytes never followed |
| T30 directory swapped for symlink during traversal | NOT TESTED | PARTIAL | Fix #3 post-mutation confinement implemented + tested (POSIX); the pre-mutation race window itself is the documented residual |
| T31 junction | TESTED AND PASSED | TESTED AND PASSED | rollback_tree |
| T32 NTFS case-insensitive collision | PLATFORM UNAVAILABLE | PLATFORM UNAVAILABLE | unchanged |
| T33 EXDEV | NOT APPLICABLE | NOT APPLICABLE | unchanged |
| T34 Linux dir fsync | NOT APPLICABLE | NOT APPLICABLE | code now executes on ubuntu CI, but power-loss durability remains unassertable |
| T35 macOS durability | NOT APPLICABLE | NOT APPLICABLE | suite now runs on macOS CI; power-loss durability unassertable |
| T36 ReplaceFileW | PARTIAL | PARTIAL | rename-based replacement exercised; ReplaceFileW not isolated |
| T37 ACL/xattr-only change | NOT TESTED | NOT TESTED | unchanged (not tracked by design) |
| T38 unknown physical state after crash | TESTED AND PASSED | TESTED AND PASSED | + Fix #20 gates and reconcile exit (hardening test) |

Tally: **26 TESTED AND PASSED, 6 PARTIAL, 1 PLATFORM UNAVAILABLE,
2 NOT TESTED, 3 NOT APPLICABLE — zero FAILED** (Phase 1.1: 24/5/4/3/2,
zero FAILED).

## 7. Measured performance (documented only, per charter)

Debug build, NTFS, 400-file tree (~20 B×20/file payload):

- `snapshot pre` ≈ 2.5 s; `snapshot post` ≈ 2.5 s (scan + CAS).
- `restore pre` (undo of 400 modified files) ≈ 15.7 minutes; output
  verified byte-faithful (spot checks on f0/f399). Dominated by the
  per-step full-workspace rescan. This is the Phase 2 baseline; the
  charter forbids redesigning rollback performance in Phase 1.2.

## 8. Conditions of the verdict (known limitations)

1. Power-loss durability (T34/T35) is not assertable in CI; kill-based
   crash injection and physical quarantine-move simulation remain the
   strongest available evidence (plus the Phase 1.1 real `kill -9`
   mid-undo of a 300-file tree).
2. T25/T26/T27/T30/T36 remain PARTIAL: the race/sharing scenarios are
   proven by refusal logic and crafted tests, not live fault injection.
   T30's pre-mutation race window is an accepted Phase 0.7 residual.
3. T28/T37 remain untested (unchanged scope).
4. T32 needs an environment that can hold case-colliding names on NTFS.
5. Windows symlink creation is capability-gated (error 1314 → asserted
   graceful skip); POSIX CI exercises the symlink paths.
6. Large rollbacks are slow by design (per-step rescan); measured in §7,
   redesign deferred to Phase 2.

## 9. Repo hygiene and docs

- `.gitignore` added; 5195 committed `target/` artifacts untracked;
  machine-local `.cargo/config.toml` (windows-gnu linker pin) untracked.
- Docs updated: `.ai/CHANGELOG.md`, `.ai/TEST_STATUS.md`,
  `.ai/CURRENT_STATE.md`, `.ai/HANDOFF.md` (§0 Phase 1.2 invariants,
  §0.1 cross-platform facts). The Phase 1/1.1 historical records are
  preserved unchanged.
- This report: `.ai/PHASE_1_2_HARDENING_REPORT.md`.
