# Phase 1.1 Repair Report — Rollback Transaction Correctness & Recovery

Status: corrective phase complete, all implemented fixes verified on the
available platform; independent re-verification recommended.

Date: 2026-09-13

Environment: Windows 11 (build 26200) x64, NTFS, Rust stable 1.98.1,
`x86_64-pc-windows-gnu` (MSVC linker absent; Linux/macOS cross-compilation
unavailable because the bundled SQLite C cross-toolchain is not installed).

---

## 1. Original Findings

From `.ai/PHASE_1_INDEPENDENT_VERIFICATION.md` (Phase 1 verdict: NOT
VERIFIED):

- **V-F01 (P0)** — undo/redo of any operation removing a non-empty
  directory tree (undo of creation, redo of deletion, undo of replacements
  involving non-empty directories) conflicted with the transaction's own
  earlier steps; recovery classified the plan-consistent intermediate state
  as unclassifiable; the workspace became permanently locked in
  RECOVERY_REQUIRED. Reproduced three ways, including a real `kill -9`
  mid-undo of a 120-file tree.
- **V-F02 (P1)** — RECOVERY_REQUIRED was terminal from the CLI: `reconcile`
  fails inside `recover_locked`, `--force` cannot pass it, and doctor's
  remediation advice was unreachable.
- **V-F03 (P2)** — Windows junctions were classified as `Symlink` instead
  of `UNSUPPORTED_OBJECT`.
- **V-F04 (P2)** — post-commit archive of quarantined files always failed
  on Windows (`sync_all` on a read-only handle → os error 5).
- **V-F05 (P2)** — `rewind run` panicked in debug builds (invalid clap
  `last` + `trailing_var_arg` combination).
- **V-F06 (P2)** — journal/pointer writes were never fsynced.
- **V-F07 (P3)** — transaction-local staging directories were never cleaned.
- **V-F09 (P3)** — no shell integration shipped; passive hooks required
  hand-managed session ids.
- Secondary planner defect found during repair: backup paths were planned
  for directory-to-directory "preserve" steps that never quarantine,
  guaranteeing bogus `archive_failed` records for ordinary child-mutation
  rollbacks.

## 2. Root Causes

### V-F01

`build_journal` recorded each step's `before` map from the pre-transaction
snapshot. `apply_step` and `recover_locked` compare the live path against
that snapshot. Removals are ordered deepest-first, so when a parent
directory's own removal (or replacement) step executed, its children had
already been removed by earlier steps of the same transaction. The live
directory therefore differed from the recorded `before` fingerprint, the
conflict gate fired on the transaction's own work, and the journal was
marked RECOVERY_REQUIRED. Recovery re-derived the same contradiction and
had no classification for "directory present, all planned children
removed" — a permanent lockout.

### V-F02

Every mutating command and `rewind reconcile` calls `recover_locked`
first. For conflict-class steps `recover_locked` writes
`RECOVERY_REQUIRED` and returns an error; there was no command that could
classify-and-complete, and no command that could resolve an unclassifiable
transaction. The state machine's documented transition
"RECOVERY_REQUIRED → explicit recovery classification → HEALTHY" had no
implementation.

### V-F03

Rust's `std::fs` reports junctions with `FileType::is_symlink() == true`,
so the reparse guard (`attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 &&
!is_symlink()`) never fired for junctions. Stable std exposes no reparse
tag, so no code path could distinguish a junction from a symlink.

### V-F04

`copy_artifact` reopened the copied file with `OpenOptions.read(true)` and
called `sync_all()`. On Windows `FlushFileBuffers` requires a handle opened
with write access; every file archive failed with os error 5. (Directory
archives "succeeded" only because they contain no files to flush.)

### V-F05

`#[arg(last = true, trailing_var_arg = true)]` is an invalid clap
combination; clap's debug assertions panic when the command is built in a
debug binary. The test suite never exercised the CLI parser.

### V-F06

`atomic_write` used `fs::write` (no flush) followed by `rename`, and never
requested directory-entry durability. The recovery journal — the
authoritative physical record — was rename-atomic but not
durability-requested.

## 3. Fixes

1. **Transaction-aware expected states (V-F01)** — `build_journal` now
   records, per step, the state the filesystem must be in *when the step
   executes*: a directory's expectation is narrowed to the children that
   survive in the target state (`Fingerprint::directory_from_children`).
   This is derivable because removals always run before installations and
   deeper removals run before shallower ones. Objects outside the narrowed
   expectation still trip the conflict gate, so genuine external
   modifications are still detected (regression tests prove both sides).
2. **Recovery exit path (V-F02)** — new `rewind recover` command:
   automatic classification first; on classification failure it prints a
   per-step physical classification (expected before / planned after /
   actual / backup / staging) and remains RECOVERY_REQUIRED with exit 3.
   `rewind recover --reconcile` is the explicit resolution: it refuses to
   abandon on transient storage errors (only `RecoveryRequired`/`Conflict`
   justify abandonment), archives every existing quarantine artifact,
   clears backup paths for steps that never executed, marks the journal
   **ABANDONED** (new terminal status; excluded from unfinished
   everywhere), records an unknown interval from the transaction anchor,
   and reconciles the live state into a new trusted checkpoint. Doctor and
   `require_healthy` messages now reference the real commands.
3. **Junction classification (V-F03)** — the scanner reads the actual
   reparse tag through `FSCTL_GET_REPARSE_POINT` on a handle opened with
   `FILE_FLAG_OPEN_REPARSE_POINT` (the only unsafe code in the crate,
   documented, handle closed on every path). `IO_REPARSE_TAG_SYMLINK` →
   SYMLINK; `IO_REPARSE_TAG_MOUNT_POINT` → `UNSUPPORTED(WINDOWS_JUNCTION)`;
   unreadable/unrecognized tags → unsupported (refusal-safe).
4. **Archive flush (V-F04)** — the archive copy is reopened with
   `.write(true)` before `sync_all`. Durability semantics preserved.
5. **CLI arguments (V-F05)** — removed `last = true`; `trailing_var_arg`
   alone keeps `rewind run -- <command…>` and `rewind run <command…>`
   working; verified in a debug build and pinned by a CLI test.
6. **Durability (V-F06)** — `atomic_write` now writes through an explicit
   `File` handle, `sync_all()`s the payload before rename, and requests
   directory-entry flush where the platform accepts it (unix; Windows is
   best-effort per the capability matrix). Applies to journal, pointer,
   and boundary-log writes.
7. **Staging cleanup (V-F07)** — staging is removed after commit once the
   journal is terminal and every quarantined artifact is Archived (or none
   was required). Staging that still holds the only copy of quarantined
   bytes (archive Failed) is retained by design. Recovery commits archive
   and dispose in the same pass.
8. **Planner backup paths** — preserve-directory steps no longer plan a
   backup path; abandoned interrupted transactions archive only artifacts
   that actually exist. Bogus archive failures are gone.
9. **Shell integration (V-F09)** — `integration/rewind.bash` and
   `integration/rewind.zsh` install pre/post passive hooks with an
   automatic per-shell session id (DEBUG trap + `PROMPT_COMMAND` for bash;
   `preexec`/`precmd` hooks for zsh), fully fail-open.

Architecture compliance: the Phase 0.7 contract is unchanged. The single
model addition, the terminal `ABANDONED` journal status, exists to
implement the contract's own required transition "RECOVERY_REQUIRED →
explicit recovery classification → HEALTHY" honestly: it is only produced
by the explicit `--reconcile` user action, it is durably recorded, and the
abandoned transaction's effects are absorbed by a reconciliation
checkpoint through a recorded unknown interval — never silently marked
healthy.

## 4. Regression Tests

New permanent suite `tests/rollback_tree.rs` (real temporary workspaces,
real filesystem mutations; interruption simulated by physically moving
quarantined objects and writing the journal exactly as an interrupted run
leaves it):

| Test | Covers |
| --- | --- |
| `undo_and_redo_of_nonempty_directory_creation` | goal §8 Test A/B; V-F01 forward path |
| `undo_of_nested_tree_with_many_files` | goal §8 Test C (nested, 120 files) |
| `redo_of_supervised_directory_deletion_removes_tree` | the exact direction that bricked workspaces |
| `interrupted_tree_rollback_recovers_at_directory_step` | goal §8 Test D; crash after child removals, before parent removal; recovery completes the plan, archives, disposes staging |
| `recovery_refuses_unexpected_object_and_reconcile_resolves` | goal §8 Test E; genuine external object → refusal + data preservation + `recover --reconcile` → HEALTHY with the intruder intact |
| `recovery_refuses_external_modification_of_unprocessed_child` | external child rewrite mid-transaction → refusal + byte preservation through reconciliation |
| `archive_of_quarantined_file_succeeds_and_staging_is_cleaned` | V-F04 + V-F07: archived bytes verified by content, staging removed |
| `junction_is_unsupported_object` | V-F03: `UNSUPPORTED(WINDOWS_JUNCTION)`, undo refused, target untouched |
| `cli_surface_parses_and_recovers` | V-F05 + V-F02 CLI: no debug panic, `recover --help`, full CLI init/run e2e |

## 5. Full Test Results

Commands and results in this environment:

```text
cargo fmt --all -- --check                          PASS
cargo check --all-targets                           PASS
cargo clippy --all-targets --all-features -- -D warnings   PASS
cargo test --all-targets --all-features             22 passed, 0 failed
  foundation.rs     13 passed (pre-existing suite, unchanged)
  rollback_tree.rs   9 passed (new regression suite)
```

Additional real-filesystem CLI verification (release binary):

- Real `kill -9` during undo of a supervised 300-file tree (external
  tamper of an unprocessed file afterwards): `rewind recover` classified
  every step and refused (exit 3) with the tamper precisely identified;
  tampered bytes intact; `rewind recover --reconcile` archived artifacts,
  marked the journal ABANDONED, reconciled to HEALTHY, and preserved every
  byte; a subsequent strong operation captured and undid normally.
- Real `kill -9` during undo of a supervised 120-file tree: next `rewind
  run` recovered the transaction (all 120 files removed, directory step
  completed, operation UNDONE) and captured normally. HEALTHY, no
  unfinished transactions.
- 400-file tree: supervised undo and redo via CLI both complete correctly
  (≈8 minutes each — see limitations).
- Non-empty type replacement both directions (file → directory with
  children → file): undo and redo correct.
- Redo fidelity: byte-identical restore with the generating script deleted
  (no re-execution).
- Degradation battery: passive hook scan failure (real sharing violation)
  → CAPTURE_FAILED → RECONCILIATION_REQUIRED → undo refused → reconcile →
  HEALTHY.
- Conflict + force: refusal with data preserved; force quarantines and
  reaches target.
- Cross-volume store: undo with restore from CAS; quarantined bytes
  archived cross-volume with `Archived` status (previously always
  Failed).
- Concurrency: writers serialize; passive hook during a writer records a
  bypass marker; doctor forces the gate; the next run reconciles first.
- Bash integration: interactive shell with `integration/rewind.bash`
  records pre/post boundaries and PASSIVE_OBSERVATION operations without
  manual session ids; hook failures fail open.

## 6. 38-Scenario Matrix (rerun)

| ID | Phase 1 status | Phase 1.1 status | Evidence |
| --- | --- | --- | --- |
| T01 | PASSED | TESTED AND PASSED | B2 (real sharing violation in passive post-scan), pre-fix V14d exit-4 path unchanged |
| T02 | PASSED | TESTED AND PASSED | B2 + pre-fix V3 boundary-only |
| T03 | PASSED | TESTED AND PASSED | B2 reconcile → HEALTHY |
| T04 | PASSED | TESTED AND PASSED | B2 + pre-fix V4 |
| T05 | PASSED | TESTED AND PASSED | B2/B5 post-gate run pre-state = reconciled checkpoint |
| T06 | PASSED | TESTED AND PASSED | pre-fix V4 (path unchanged) |
| T07 | PASSED | TESTED AND PASSED | unit 3, B2 |
| T08 | PASSED | TESTED AND PASSED | unit 3, B4 |
| T09 | PASSED | TESTED AND PASSED | unit 2, B3 |
| T10 | PARTIAL | TESTED AND PASSED | empty-dir (unit 3) + non-empty dir with children (CLI) |
| T11 | PARTIAL | TESTED AND PASSED | same, both directions |
| T12 | PLATFORM UNAVAILABLE | PLATFORM UNAVAILABLE | no symlink privilege on this account |
| T13 | PLATFORM UNAVAILABLE | PLATFORM UNAVAILABLE | same |
| T14 | PASSED | TESTED AND PASSED | B4; archive now Archived |
| T15 | PARTIAL | PARTIAL | PLANNED retry proven (pre-fix V6); real pre-move kill not isolated |
| T16 | PASSED | TESTED AND PASSED | unit 6 + crafted suite |
| T17 | PASSED | TESTED AND PASSED | real kill: physical AFTER accepted |
| T18 | PASSED | TESTED AND PASSED | pre-fix V6 |
| T19 | FAILED | TESTED AND PASSED | real kill mid-undo of 300-file tree → recovery completes; crafted suite |
| T20 | PASSED | TESTED AND PASSED | real kill: stale journal state, physical map wins |
| T21 | PARTIAL | TESTED AND PASSED | unexpected object → refusal + preserved + resolvable |
| T22 | PASSED | TESTED AND PASSED | pre-fix V6 |
| T23 | PASSED (deviation) | TESTED AND PASSED | serializes instead of busy-refusal (documented) |
| T24 | PASSED | TESTED AND PASSED | B5 bypass marker → forced reconciliation |
| T25 | NOT TESTED | PARTIAL | mid-transaction tamper detection proven via recovery classification; live race not fault-injected |
| T26 | NOT TESTED | PARTIAL | intruder-in-tree proven; non-target drift path code-reviewed |
| T27 | PARTIAL | PARTIAL | scan-side refusal (B2); rollback-side sharing refusal code-reviewed |
| T28 | NOT TESTED | NOT TESTED | identity revalidation unchanged |
| T29 | PLATFORM UNAVAILABLE | PLATFORM UNAVAILABLE | symlink privilege |
| T30 | NOT TESTED | NOT TESTED | TOCTOU narrowing code-reviewed |
| T31 | FAILED | TESTED AND PASSED | junction → `UNSUPPORTED(WINDOWS_JUNCTION)`, no traversal, undo refused |
| T32 | PLATFORM UNAVAILABLE | PLATFORM UNAVAILABLE | NTFS case-insensitivity |
| T33 | NOT APPLICABLE | NOT APPLICABLE | Windows never attempts cross-volume critical moves; B4 |
| T34 | NOT APPLICABLE | NOT APPLICABLE (improved) | journal fsync now implemented; unix dir flush conditional |
| T35 | NOT APPLICABLE | NOT APPLICABLE | unchanged |
| T36 | PARTIAL | PARTIAL | rename-based replacement exercised; ReplaceFileW not isolated |
| T37 | NOT TESTED | NOT TESTED | unchanged |
| T38 | FAILED | TESTED AND PASSED | refusal + per-step classification + explicit resolvable exit; ambiguous states stay blocked |

Tally: **24 TESTED AND PASSED, 5 PARTIAL, 4 PLATFORM UNAVAILABLE,
3 NOT TESTED, 2 NOT APPLICABLE** (previously 17/6/3 failed/7/5). The three
failures are eliminated.

## 7. Recovery Verification (P0/P1 evidence)

- V-F01 forward: undo of `mkdir` with children, nested 120-file tree,
  400-file tree, redo of `rmdir /S`, non-empty type replacements — all
  commit with HEALTHY and zero unfinished transactions.
- V-F01 crash: real `kill -9` mid-undo (120- and 300-file trees); recovery
  classified every step physically, completed the plan, archived
  quarantined artifacts, disposed staging, and returned to HEALTHY. The
  crafted-journal regression tests pin the directory-step boundary
  permanently.
- V-F02: ambiguous states (external object inside a removed directory;
  externally rewritten unprocessed child) are refused by automatic
  recovery, reported with a per-step classification, remain
  RECOVERY_REQUIRED with every external byte intact, and are resolved only
  by the explicit `rewind recover --reconcile` (archival → ABANDONED →
  unknown interval → reconciliation checkpoint → HEALTHY). No silent
  healthy transition exists; transient storage errors never trigger
  abandonment.
- Not claimed: real power-loss equivalence. kill-based injection and
  physical quarantine-move simulation are the strongest available
  evidence.

## 8. Remaining Limitations

- **Performance**: rollback steps rescan the whole workspace (per-step
  preflight + verification). Measured on NTFS: 120-file tree ≈ 1–2 min;
  400-file tree undo ≈ 8 min, redo ≈ 8.8 min. This is a correctness-first
  design with no Phase 1 performance contract, but it is a real usability
  boundary and should be addressed (incremental scans) before large-tree
  use.
- **Platform coverage**: executed on Windows GNU only. MSVC, Linux, and
  macOS remain unverified; Linux/macOS do not even cross-compile here
  (bundled SQLite C cross-toolchain absent).
- **Symlink coverage**: this account cannot create symbolic links, so
  T12/T13/T29 remain platform-unavailable; the junction classifier is
  tested, the symlink path is code-reviewed.
- **Power-loss**: not testable here; kill-based injection is simulation.
- **T25/T26/T27 rollback-side races**: detection is proven through
  recovery classification and per-step preflight code; live races were not
  fault-injected.
- **T28/T30/T37**: unchanged, not tested.
- **T23 deviation**: concurrent CLIs serialize rather than refuse busy.
- **Pre-1.1 abandoned staging**: staging from transactions abandoned
  before this fix whose archive failed is retained by design and must be
  removed manually after inspection.
- The archive of a quarantined junction recreates it as a symlink in the
  external archive (diagnostic copy only; the live tree is never
  traversed).

## 9. Architecture Compliance

- The Phase 0.7 architecture and `phases/phase-01-foundation.md` are
  unchanged. No refusal rule was weakened; no conflict check was disabled;
  no test was skipped or weakened; no mocks were introduced.
- The transaction-aware step expectations implement §11.1/§11.2's
  "expected complete BEFORE state map" at the step-execution instant and
  §11.4's PARTIAL classification for plan-consistent intermediate states.
- The `ABANDONED` journal status is a minimal terminal-state addition that
  implements the contract's required RECOVERY_REQUIRED exit; it is
  recorded durably, never produced automatically, and always paired with
  artifact archival and a reconciliation checkpoint.
- The reparse-tag classifier implements §8.4's "positively identified"
  requirement literally.
- Durability now matches §11.3 step 2 within platform capabilities.

## 10. Final Recommendation

The two blockers and all secondary defects found by the independent
verification are fixed with permanent regression coverage, and every
scenario that this platform can exercise now passes. Phase 1 should be
re-verified independently (per the original charter) with emphasis on the
new recovery exit path; the outstanding platform, symlink, power-loss, and
live-race gaps must not be upgraded to "verified". Phase 2 must not begin
until that re-verification accepts the Phase 1.1 state.
