# Phase 1 Independent Verification Report

Verifier: independent hostile verification pass (separate from the
implementation engineer)

Date: 2026-09-12

Verification platform: Windows 11 (build 26200) x64, NTFS, Rust stable
1.98.1, default target `x86_64-pc-windows-gnu` (Git Bash harness; MSVC
linker absent).

Evidence basis: full source review of every file in the repository, all
13 integration tests executed, plus ~30 new adversarial scenarios
executed against real temporary workspaces with the release binary
(`target/release/rewind.exe`), including real process-kill crash
injection, real exclusive-handle sharing violations, real scan-timeout
degradation, real cross-volume store operation, and real concurrency.

No production code was modified. No tests were weakened or deleted. Two
temporary verification artifacts were used (a random-content generator
script and an exclusive-file-lock PowerShell snippet) inside temporary
directories only; nothing was changed in the repository.

---

## 1. Executive Verdict

**FAIL**

There is a P0 finding (V-F01): `undo`/`redo` of any operation that
removes a non-empty directory tree — the most ordinary directory
scenario (undo of a supervised `mkdir` with content, redo of a
supervised `rmdir /S`) — self-conflicts against the transaction's own
earlier steps, then permanently locks the workspace in
RECOVERY_REQUIRED. Recovery classifies the plan-consistent intermediate
state as UNKNOWN ("cannot classify recovery state"), no CLI command can
ever clear the lockout (including `--force`), and doctor's remediation
advice is unreachable. This was reproduced three independent ways,
including a real kill mid-undo of a 120-file creation.

There is a dependent P1 finding (V-F02): RECOVERY_REQUIRED is a
terminal state in the CLI; the contract requires an explicit recovery
path to HEALTHY and none exists.

What *was* verified to work correctly and is worth stating precisely:

- The failed-capture → DEGRADED/RECONCILIATION_REQUIRED →
  reconciliation machinery — the heart of the Phase 0.7 corrections —
  works end-to-end through two distinct real failure modes (scan
  deadline timeout and OS sharing violation), with correct exit code 4,
  correct refusal of undo, untrusted boundary-only attribution while
  gated, and gap closure without fabricated causality.
- Redo provably does not re-execute the original command (byte-identity
  restore with the command's script deleted).
- The state chain is strictly sequential (S0→S1→S2), typed
  fingerprints are deterministic across workspaces and stores, and
  conflict handling preserves user data with working `--force`
  quarantine behavior.
- CAS corruption refusal, writer serialization, and the bypass-marker
  gate all behave as contracted.

The implementation report's own status ("INCOMPLETE — foundation
exists") is accurate and, if anything, slightly generous: the rollback
engine's directory handling is not merely "pending verification", it is
defective.

Per Section 35 of the verification charter, any P0, wrong core
undo/redo semantics, or unsafe crash recovery mandates FAIL.

---

## 2. Scope Reviewed

Every file in the repository (13 Rust files, 19 Markdown documents,
2 Cargo manifests):

| File | Lines | Reviewed |
| --- | ---: | --- |
| `src/model.rs` | 401 | full |
| `src/paths.rs` | 218 | full |
| `src/cas.rs` | 195 | full |
| `src/scan.rs` | 307 | full |
| `src/db.rs` | 712 | full |
| `src/journal.rs` | 90 | full |
| `src/workspace.rs` | 591 | full |
| `src/rollback.rs` | 1027 | full |
| `src/cli.rs` | 454 | full |
| `src/error.rs`, `src/lib.rs`, `src/main.rs` | 83 | full |
| `tests/foundation.rs` | 440 | full |
| `Cargo.toml`, `.cargo/config.toml` | 21 | full |
| `phases/phase-01-foundation.md` | 385 | full |
| `.ai/PHASE_0_7_ARCHITECTURE_FINALIZATION_REPORT.md` (normative) | 949 | full |
| remaining `.ai/*.md` | — | full |

Documents read as authoritative: `phase-01-foundation.md` (contract)
and the Phase 0.7 report (38-scenario matrix, normative failure,
fingerprint, quarantine, durability, platform rules).

---

## 3. Architecture Compliance

Legend: contract requirement → status → evidence.

| Contract Requirement | Implementation Location | Status | Evidence |
| --- | --- | --- | --- |
| Explicit workspace identity, user-selected canonical root | `workspace.rs:init/open_from_pointer`, `paths.rs` | IMPLEMENTED | init/open identity checks; pointer published last on init |
| External store holds catalog/CAS/journal/anchors | `workspace.rs:Storage::open` | IMPLEMENTED | test + CLI runs; store-inside-workspace refused (test 8) |
| Initial full scan → durable S0 → HEALTHY | `workspace.rs:init` | IMPLEMENTED | init on 2500 files; DEGRADED until S0 |
| Typed fingerprints incl. ABSENT | `model.rs:Fingerprint`, `scan.rs` | IMPLEMENTED | tests 1/11; CLI verify of ABSENT, file, dir, symlink-kind |
| Deterministic directory manifests (canonical children) | `scan.rs:visit_directory` | IMPLEMENTED | identical manifests across stores produce identical state IDs (observed) |
| Symlinks recorded literally, never followed | `scan.rs:132-150` | PARTIALLY VERIFIED | unit test capability-skips on this account (no SeCreateSymbolicLink); code review correct; junction misclassification found (V-F03) |
| Unsupported/unclassified reparse → UNSUPPORTED_OBJECT | `scan.rs:is_unclassified_reparse` | CONTRADICTED (V-F03) | junction on NTFS classified `Symlink`, not `Unsupported` |
| Strong capture under writer lease, pre-scan → exec → post-scan → Sx→Sy | `workspace.rs:run_command` | IMPLEMENTED | V1 chain, e2e ops 1-10 |
| Capture failure after exec → CAPTURE_FAILED + UNKNOWN_INTERVAL, exit 4 | `workspace.rs:record_capture_failure` | IMPLEMENTED | V14d: real sharing violation, exit 4, gated |
| Stale baseline never used as pre-state | `run_command` drift check + `ensure_healthy_locked` | IMPLEMENTED | V3/T05: post-reconcile pre-state == reconciled checkpoint, not old baseline |
| Passive hooks: boundary marker, bounded scan, low-confidence observation, baseline refresh | `cli.rs:hook_pre/hook_post` | IMPLEMENTED | e2e op #9; baseline refreshed; observation not undoable (UNAVAILABLE) |
| Passive failure → capture-gap record; untrusted boundaries while gated | `cli.rs:hook_post_locked` | IMPLEMENTED | V3 (timeout), V4 (sharing violation); BOUNDARY_ONLY/UNTRUSTED recorded |
| Hook fail-open for shell, bypass marker when writer busy | `cli.rs:append_bypass_marker`, `workspace.rs:enforce_pending_safety_gate` | IMPLEMENTED | V7(b): bypass during run → doctor forces gate → reconcile clears |
| `rewind run` reconciles first when gated; refuses on failure | `workspace.rs:run_command`, `reconcile_locked` | IMPLEMENTED | V4/T06: refusal with command not executed |
| Reconciliation: full scan, no causality, closes gap as Sx | `reconcile_locked` | IMPLEMENTED | V3/T03: open_unknown closed; no fabricated operation rows |
| Undo: pre-rollback anchor, preflight, conflict gate, quarantine, journal | `rollback.rs:undo/execute_transition/apply_step` | PARTIALLY IMPLEMENTED | file ops + restore-of-tree + empty-dir cases correct; non-empty-directory removals broken (V-F01) |
| Redo applies recorded post-state, never re-executes | `rollback.rs:redo` | IMPLEMENTED | V2: byte-identical restore with generator deleted |
| Same-filesystem transaction-local staging/quarantine | `paths.rs:staging_root`, `rollback.rs` | IMPLEMENTED | staging sibling `.rewind-txn/<txn>`; never crosses volumes for live moves |
| External archival is post-commit only | `rollback.rs:archive_committed_transaction` | PARTIALLY IMPLEMENTED | copy+verify logic present; file archives always fail on Windows (V-F04) |
| Journal lifecycle PLANNED→…→COMMITTED physically meaningful | `rollback.rs`, `journal.rs` | PARTIALLY IMPLEMENTED | states written and used; per-step before/after maps are single-path (root cause of V-F01) |
| Recovery inspects physical state; completes known plans; refuses UNKNOWN | `rollback.rs:recover_locked` | PARTIALLY IMPLEMENTED | recognized partial (quarantined source) completed (unit test 6); plan-consistent dir state misclassified (V-F01); conflict-class lockout unresolvable (V-F02) |
| Quarantine on same filesystem; cross-device never critical | `rollback.rs` + V13 | IMPLEMENTED | cross-volume undo via CAS restore; archive failure non-fatal |
| Path confinement, no `..`/absolute/symlink-parent escape | `paths.rs:validate_relative/ensure_parent_confinement` | IMPLEMENTED | unit test 13; junction/symlink parent components refused; no escape found in testing |
| Case-collision detection | `scan.rs:register_case_collision` | IMPLEMENTED (code) | PLATFORM UNAVAILABLE on NTFS to exercise |
| Single-writer lease | `workspace.rs:WorkspaceLease` (fs2) | IMPLEMENTED | V7(a): writers serialize; hook gets nonblocking |
| Doctor: diagnose, not repair | `cli.rs:doctor` | IMPLEMENTED | V6: integrity, CAS verify, drift, unfinished txns, exit 3 |
| No Phase 2+ features leaking | `cli.rs` surface | IMPLEMENTED | command set is exactly Phase 1 |

---

## 4. Functional Verification (commands and scenarios actually executed)

Build/lint/test baseline (reproduced independently):

```text
cargo fmt --all -- --check                 PASS
cargo clippy --all-targets -- -D warnings  PASS
cargo test --all-targets                   13 passed, 0 failed
cargo build --release                      OK (windows-gnu)
cargo check --target x86_64-unknown-linux-gnu   FAIL (bundled SQLite C cross-toolchain absent)
cargo check --target x86_64-apple-darwin        FAIL (same)
```

Adversarial scenarios executed against real workspaces (all with the
release binary):

| # | Scenario | Result |
| --- | --- | --- |
| V1 | State chain S0→S1→S2 across three runs; op N pre == op N-1 post | PASS |
| V2 | Redo restores captured bytes; generator script deleted first | PASS (decisive) |
| V3 | Real scan-timeout (50 ms hook deadline, 2500 files) → CAPTURE_FAILED, gate, undo refused, boundary-only while gated, reconcile → HEALTHY, next run pre == reconciled Sx | PASS |
| V4 | `rewind run` while reconciliation fails (exclusive handle): refuses, command NOT executed, gate preserved, self-heals after release | PASS |
| V5 | Conflict matrix: content change / deletion / rename / type replacement before undo → Conflict exit 1, data preserved; `--force` quarantines and reaches target in all four | PASS |
| V6 | Corrupt CAS blob → undo refuses before mutation; doctor reports; after repair, pending PLANNED transaction auto-completes on next mutation (contract precondition 7) | PASS (cosmetic message note) |
| V7 | Two concurrent runs serialize (2.23 s for two ~2 s pings); hook during run → bypass marker → doctor forces RECONCILIATION_REQUIRED → next run reconciles | PASS |
| V8c | Real `kill -9` mid-undo of 120-file creation: 12/120 rolled back, 1 unfinished transaction → recovery runs on next mutation but **fails: "cannot classify recovery state at many" → RECOVERY_REQUIRED** | **FAIL (V-F01)** |
| V9 | Plain undo of supervised `mkdir newdir` + child: Conflict "expected DIRECTORY(entries=1), found DIRECTORY(entries=0)" → RECOVERY_REQUIRED; reconcile refuses; force undo fails (recovery first); doctor advice unreachable | **FAIL (V-F01/V-F02)** |
| V10 | Undo of supervised `rmdir /S /Q tree` (restore): PASS; **redo of same op: Conflict → RECOVERY_REQUIRED** | **FAIL (V-F01)** |
| V11b | Escaping-symlink creation unavailable (no symlink privilege); failed mklink captured as no-op STRONG op correctly | PLATFORM UNAVAILABLE |
| V12 | Junction classified `Symlink` (should be UNSUPPORTED_OBJECT); undo of junction creation succeeded by rename, target untouched | DEVIATION (V-F03) |
| V13 | Store on different volume: init/undo-deletion via CAS PASS; archive of quarantined file → os error 5, archive_status Failed | PASS + **FAIL (V-F04)** |
| V14d | Deterministic post-capture failure inside `rewind run` (detached exclusive handle): command ran, capture failed, stderr diagnostic, **exit 4**, gated, undo refused, reconcile → HEALTHY | PASS |
| E2E | Full §32 sequence (create/modify×2/rename/delete/dir/child-file/passive/degrade/dirty/reconcile/trusted op/undo/redo/tamper/conflict/force/doctor/final state) | PASS with V-F01 excluded (empty-dir variant used; non-empty dir breakage proven separately in V8c/V9/V10) |

---

## 5. 38-Scenario Matrix

From `.ai/PHASE_0_7_ARCHITECTURE_FINALIZATION_REPORT.md` §18. No
scenarios were collapsed.

| ID | Classification | Evidence |
| --- | --- | --- |
| T01 post-scan timeout after modify | TESTED AND PASSED | V14d (sharing-violation variant, exit 4) and V3 (timeout variant via hook) |
| T02 passive command during gate | TESTED AND PASSED | V3 BOUNDARY_ONLY/UNTRUSTED |
| T03 reconcile after gap | TESTED AND PASSED | V3: gap closed S0→Sx, HEALTHY |
| T04 several commands while gated | TESTED AND PASSED | V3/V4: coalesced interval, undo refused |
| T05 strong op after reconcile | TESTED AND PASSED | V3 T05: pre == reconciled checkpoint |
| T06 run while degraded, reconcile fails | TESTED AND PASSED | V4: refusal, no execution |
| T07 ABSENT→file creation captured/undoable | TESTED AND PASSED | unit 3 + e2e op 12/15 |
| T08 file deletion captured/undo restored | TESTED AND PASSED | unit 3 + V13 |
| T09 A→B modification captured | TESTED AND PASSED | unit 2, V1 |
| T10 file→directory replacement | PARTIALLY TESTED | empty-dir replacement passes (unit 3); non-empty-dir undo breaks (V-F01) |
| T11 directory→file replacement | PARTIALLY TESTED | same as T10 |
| T12 symlink→file | PLATFORM UNAVAILABLE | no symlink privilege on this account; unit test capability-skips |
| T13 file→symlink | PLATFORM UNAVAILABLE | same |
| T14 cross-volume store undo | TESTED AND PASSED | V13; archive piece FAILED (V-F04) |
| T15 crash before quarantine move | PARTIALLY TESTED | code path reviewed; unit 6 simulates post-quarantine only; real pre-move kill not isolated |
| T16 crash after quarantine move, before install | TESTED AND PASSED (single file) | unit 6 + recovery classification of file steps in V8c |
| T17 crash after install, before archive | TESTED AND PASSED | V8c: already-removed steps marked Durable by physical inspection |
| T18 crash with journal PLANNED | TESTED AND PASSED | V6: PLANNED transaction retried after repair |
| T19 crash during multi-step mutation | TESTED AND FAILED | V8c: recovery reached dir step → "cannot classify" → lockout (V-F01) |
| T20 crash after mutation, before journal update | TESTED AND PASSED (file steps) | V8c: physical AFTER steps accepted as Durable |
| T21 source/destination unexpected objects | PARTIALLY TESTED | classification logic reviewed; conflict-refusal verified in forward path (V5); recovery-side refusal verified (V8c refuses) |
| T22 staging artifact truncation / CAS hash fail | TESTED AND PASSED | V6: materialize verify fails before mutation; transaction left PLANNED |
| T23 second CLI during rollback | TESTED AND PASSED (deviation) | V7: serializes instead of "refuse/busy" — single-writer holds (V-F10) |
| T24 passive hook during writer | TESTED AND PASSED | V7(b): bypass marker → forced reconciliation |
| T25 IDE edits target between preflight and apply | NOT TESTED (races) | code path reviewed (per-step re-scan + conflict) |
| T26 external edit of non-target path during rollback | NOT TESTED | per-step full re-scan implies refusal; unproven |
| T27 Windows exclusive handle on target | PARTIALLY TESTED | scan-side refusal V4; rollback-side refusal code-reviewed only |
| T28 workspace moved to another volume during recovery | NOT TESTED | identity revalidation code-reviewed; pointer root check exercised indirectly |
| T29 symlink points outside during scan | PLATFORM UNAVAILABLE | cannot create symlinks on this account |
| T30 directory swapped for symlink during traversal | NOT TESTED | TOCTOU narrowing reviewed (type recheck in scan) |
| T31 junction on Windows | TESTED AND FAILED | V12: classified Symlink, not UNSUPPORTED_OBJECT (V-F03) |
| T32 case-insensitive collision | PLATFORM UNAVAILABLE | NTFS cannot hold foo/FOO simultaneously |
| T33 EXDEV cross-device rename | NOT APPLICABLE (Windows) | Windows never attempts cross-volume critical moves; V13 confirms local staging |
| T34 Linux directory fsync | NOT APPLICABLE | non-Linux verification platform |
| T35 macOS durability partial | NOT APPLICABLE | non-macOS verification platform |
| T36 ReplaceFileW regular-file replacement | PARTIALLY TESTED | rename-based replacement exercised; ReplaceFileW itself not isolated |
| T37 ACL/xattr change with same bytes | NOT TESTED | not tracked by design; refusal path reviewed |
| T38 unknown physical state after crash | TESTED AND FAILED | V8c/V9/V10: lockout entered correctly but **cannot be exited** (V-F01/V-F02) |

Tally: 17 TESTED AND PASSED, 6 PARTIALLY TESTED, 3 TESTED AND FAILED
(T19, T31, T38), 7 PLATFORM UNAVAILABLE / NOT APPLICABLE, 5 NOT TESTED.

---

## 6. State Model Audit

- Chain continuity: three sequential runs produce op1.post == op2.pre
  == ... (V1). No skipped or duplicated states.
- `INSERT OR IGNORE` state insertion is content-addressed; identical
  manifests across independent workspaces/stores share IDs (observed,
  benign and deterministic).
- Baseline advancement happens exactly once per successful capture
  (`set_workspace(Healthy, post_state)` after operation insert). A
  failure of the trailing `set_workspace` would leave a recorded op
  with stale pointer; next run detects drift and reconciles (analyzed;
  not fault-injected).
- Failed capture: CAPTURE_FAILED row, open UNKNOWN_INTERVAL at the
  pre-state, condition RECONCILIATION_REQUIRED; baseline pointer
  retains the pre-state as historical reference only; all mutation and
  attribution blocked; reconcile closes interval S_last→Sx without
  causality and consumes bypass markers (V3/V4/V14d).
- Pre-run drift: if live scan ≠ baseline before a run, reconciliation
  runs first and the operation's pre-state is the reconciled
  checkpoint (observed in ws8 scenario; prevents S0(A)→S2(C)
  fabrication).
- Reconciliation-failure transition keeps RECONCILIATION_REQUIRED
  rather than the contract's intermediate DEGRADED (V-F11; gate
  preserved either way).

## 7. Observation Audit

- Passive pre-hook records boundary only; post-hook: recovery →
  finish boundary → gate enforcement → scan with a fixed 50 ms deadline
  → on success a PASSIVE_OBSERVATION (LOW_CONFIDENCE, UNAVAILABLE —
  never undoable) whose post-state refreshes the trusted checkpoint; on
  timeout/permission failure → record_capture_failure (V3, e2e #9).
- While gated, post-hook records BOUNDARY_ONLY/UNTRUSTED without
  scanning (V3 T02). No fabricated Sx→Sy edges anywhere in history.
- Hook lease contention → nonblocking acquire → durable bypass marker
  (file + DB) → gate enforced by next writer/doctor (V7b). The hook
  itself never blocks the shell (all errors swallowed with a diagnostic,
  exit 0).
- `rewind run`: lease → recovery → healthy gate → pre-scan (failure →
  refuse, exit 1, command never started — V4) → direct process execution
  (no shell) → post-scan (failure → CAPTURE_FAILED, exit 4, command
  result preserved — V14d) → operation + baseline advance.
- Gap in passive wiring: no shell integration scripts ship with the
  repo and the default session id (`shell-<pid>`) cannot match between
  pre/post hook processes, so passive observation is only reachable
  with hand-managed `REWIND_SESSION_ID` (V-F09).

## 8. Storage/CAS Audit

- BLAKE3 content addressing; blob path validated as 64 hex chars (no
  traversal); temp `.part` files cleaned on startup (unit 10); blobs
  written via create_new temp + sync + readonly + rename; existing-blob
  re-verification on re-ingest.
- Corruption: verify() re-hashes; undo refuses via anchor verification
  before any mutation (V6); capture of an unchanged file fails when its
  blob is corrupt (scan re-verifies), which degrades safely.
- No hardlinks anywhere in CAS or rollback (grep + review).
- `materialize` stages into the destination's parent as `.name.uuid.stage`
  then renames; file installed content is synced (sync_all), Windows
  directory sync is correctly best-effort per the platform matrix.
- Gaps: journal/pointer/atomic_write payloads are never fsync'd (V-F06);
  archive of quarantined files fails on Windows (V-F04); staging
  directories never cleaned (V-F07).

## 9. Undo/Redo Audit

- File creation/deletion/modification undo+redo: correct (unit 3, V13,
  e2e). Restore of a deleted non-empty directory tree: correct (V10).
- Directory-child changes preserve the parent directory (unit 5).
- Redo never re-executes: proven with a deleted non-idempotent
  generator (V2).
- **Broken**: any transition that removes a non-empty directory tree
  (undo-of-creation, redo-of-deletion, undo of replacements involving
  non-empty dirs) — V-F01. Empty-directory variants pass, which is why
  the shipped test suite (empty directories only) is green.
- Multi-level undo/redo status machine (latest_undoable/latest_redoable)
  reviewed; passive observations and failed captures are excluded by
  reversibility/status filters; post-observation undo of the last
  strong op correctly conflicts until live state matches (observed).

## 10. Conflict/Safety Audit

- Early conflict gate (current scan ≠ op post-state) refuses with
  exit 1 before any journal exists; all four tamper classes verified;
  user data untouched (V5).
- `--force` proceeds with the live state as the anchor's before-map;
  divergent bytes are renamed to transaction-local quarantine first and
  archived (attempted) post-commit — dirty state preserved (V5, e2e 18).
- Per-step preflight re-scans the whole workspace immediately before
  each step; divergence → journal RECOVERY_REQUIRED, no overwrite
  (code-verified; race not fault-injected).
- CAS-verification gate before anchor creation (V6).

## 11. Journal/Crash-Recovery Audit

- Journal is external JSON per transaction; `unfinished()` returns all
  non-COMMITTED; catalog mirrors it; recovery classifies each step by
  physical inspection (scan) of live + backup + staging artifacts.
- Verified behaviors: PLANNED retry after CAS repair (V6/T18); physical
  AFTER accepted after crash (V8c/T17/T20 file steps); verified partial
  (quarantined source + staged desired) completes (unit 6/T16).
- **Failed behaviors**: the directory-removal step of a tree transaction
  is classified UNKNOWN because its recorded `before` fingerprint
  (directory with children) was invalidated by the transaction's own
  earlier child steps — plan-consistent intermediate state treated as
  unclassifiable (V8c, V9, V10 → V-F01). Consequence: permanent
  RECOVERY_REQUIRED (V-F02).
- Real power-loss testing was not performed (no VM/firmware
  harness here); `kill -9` during apply is the strongest simulation
  used, and it is NOT equivalent to power loss — journal fsync gaps
  (V-F06) mean power-loss behavior is strictly weaker than what kill -9
  shows.

## 12. Quarantine Audit

- Live quarantine is transaction-local on the workspace's filesystem
  (`.rewind-txn/<txn>/quarantine`); installs come from CAS via
  same-filesystem staging. No cross-volume rename is ever attempted for
  critical movement (V13 cross-volume store exercised the copy+verify
  archive path instead).
- Post-commit archive: copy + hash/type verify + (broken) sync — file
  artifacts always fail with os error 5 on Windows (V-F04); committed
  results unaffected (contract-conformant non-failure), quarantine
  bytes remain in staging.
- Crash during archive: archive_status Pending/Failed is retried by
  later recover_locked calls (observed); workspace result unaffected.

## 13. Concurrency Audit

- fs2 exclusive lock on `<store>/projects/<id>/lock.pid`; CLI writers
  block; passive hook uses try_lock and falls back to bypass marker.
  Lock lifetime is the file handle — released by the OS on death; no
  stale-lock state can persist (OS-enforced; reviewed).
- Verified: two concurrent `rewind run` serialize; both captured
  correctly; hook during run records bypass; doctor then forces
  reconciliation; next run reconciles first (V7).
- Deviation: T23 expects refusal/busy reporting; implementation
  serializes (V-F10) — safety-preserving.
- Cosmetic: lease open uses `.truncate(true)` before acquiring, so a
  losing contender can truncate the owner-info file while the real lock
  remains held (no safety impact).

## 14. Security Audit

- No `unsafe` blocks exist in the entire crate. No `unwrap`/`expect`/
  `panic!`/`todo!`/`unimplemented!` in production code (grep + review).
  The CLI never spawns a shell (`Command::new` direct exec; stdin/out
  inherited).
- Path confinement: absolute paths, `..`, drive/UNC prefixes rejected;
  every parent component re-checked as a non-symlink real directory at
  use time; case collisions fail the scan; CAS ids validated. No
  escape found in testing.
- TOCTOU: confinement checks and renames are not atomic together; the
  contract explicitly accepts a losing race against external writers at
  the final pre-step verification; observed behavior consistent.
- Temp artifacts use UUID names (not predictable per-target);
  CAS blobs are set readonly.
- Junction misclassification (V-F03) is the one security-relevant
  deviation: recorded and renamed without traversal (safe in the
  exercised paths), but a junction restore would attempt to create a
  symlink of a guessed flavor instead of refusing as the matrix
  requires.
- `publish_bytes` joins caller-provided names onto the CAS root;
  current callers pass fixed names (reviewed); latent, not exploitable
  today.

## 15. Cross-Platform Audit

- **Windows MSVC**: NOT VERIFIED — no MSVC linker installed; not even a
  compile check possible.
- **Linux**: NOT COMPILED — bundled SQLite C cross-build lacks a
  cross-toolchain. Code reviewed only.
- **macOS**: NOT COMPILED — same. Code reviewed only.
- Executed and verified: `x86_64-pc-windows-gnu` only. The shipped test
  suite hard-codes `cmd /C` command vectors and would fail on POSIX as
  written; symlink tests silently capability-skip on unprivileged
  accounts (so the suite's "symlink" pass is a skip here).
- These categories are not equivalent and only the first (code
  reviewed) applies beyond Windows GNU.

## 16. Test Coverage Gaps

- 13 real-filesystem tests exist and pass, but they only exercise
  empty directories; every non-empty-directory removal path is
  untested (this is exactly where V-F01 lives).
- The CLI parser is never exercised by tests (library API only) — which
  is how V-F05 (debug panic on `rewind run`) shipped unnoticed.
- No test injects a real post-scan failure inside `rewind run` (the
  shipped "failed capture" test calls `record_capture_failure`
  directly); the exit-4 path was verified manually here.
- No tests for: second-CLI contention, bypass marker flow, cross-volume
  store, case collision (unavailable on NTFS), junction classification,
  recovery of multi-step tree transactions, redo non-re-execution
  byte-identity, session-managed passive hooks.
- Crash tests are simulations (hand-built journals), not real kill
  points; only V8c here performed a real mid-transaction kill.

## 17. Findings

### V-F01 — P0 — Rollback self-conflict and unrecoverable lockout on non-empty directory removal

- ID: V-F01
- Severity: P0 (unrecoverable journal/workspace lockout from ordinary
  use; core undo/redo incorrect; crash recovery cannot complete a known
  plan)
- Component: `src/rollback.rs`
- Location: `build_journal` (src/rollback.rs:314-417), `apply_step`
  conflict check (src/rollback.rs:475-484), `recover_locked`
  classification (src/rollback.rs:769-833)
- Reproduction (three):
  1. `rewind init`; `rewind run -- cmd /C "mkdir newdir & echo x>
     newdir\child.txt"`; `rewind undo` →
     `conflict at newdir: expected DIRECTORY(entries=1), found
     DIRECTORY(entries=0)`; condition RECOVERY_REQUIRED; every
     subsequent mutation (including `undo --force`, `reconcile`) fails
     with "cannot classify recovery state at newdir".
  2. Undo deletion of a tree (succeeds), then `rewind redo` → same
     self-conflict on the tree step.
  3. `kill -9` mid-undo of a supervised 120-file creation; next `rewind
     run` → recovery removes remaining files, then fails: "cannot
     classify recovery state at many" → RECOVERY_REQUIRED with 1
     unfinished transaction; all further commands refused.
- Expected: per Phase 0.7 §11.2/§11.4 and T16/T19/T20, the
  transaction's own earlier steps legitimately change a parent
  directory; the directory-removal step's effective before-state is the
  emptied directory, the plan is unambiguous, and recovery completes it
  (or, at minimum, a documented CLI recovery action resolves the
  lockout).
- Actual: the step's recorded `before` is the original
  Directory(children) fingerprint; `apply_step` and `recover_locked`
  compare the live emptied directory against it, classify CONFLICT/
  UNKNOWN, and write RECOVERY_REQUIRED permanently. No CLI path exits
  RECOVERY_REQUIRED (V-F02).
- Root cause: single-path `before`/`after` maps per
  `JournalStep` cannot represent intra-transaction ordering effects on
  parent directories; removals are ordered deepest-first, so child
  removals change the directory before its own step runs; recovery has
  no plan-consistent PARTIAL rule for "directory present but all
  planned children removed".
- Impact: any user who undoes/redoes a directory-creating or
  directory-deleting operation with content bricks the workspace until
  manual journal + SQLite surgery; data is preserved in quarantine but
  unreachable through the tool.
- Recommended fix: coalesce each removed/created directory subtree
  into a single directory-tree step whose before/after maps include
  every descendant (with the directory's intermediate state derived
  from step order), or emit the parent-directory step's before-map as
  the post-child-steps state; additionally give recovery a
  plan-consistent PARTIAL classification for "live directory ⊆ planned
  removals" and add an explicit `rewind recover`/`rewind abandon`
  resolution path (see V-F02).

### V-F02 — P1 — RECOVERY_REQUIRED is a terminal state with no CLI resolution

- Component: `src/cli.rs`, `src/rollback.rs:recover_locked`
- Location: `Command::Reconcile` (src/cli.rs:112-119),
  `recover_locked` (src/rollback.rs:725)
- Reproduction: after V-F01, run `rewind reconcile` (fails inside
  `recover_locked`), `rewind undo --force` (fails — recovery precedes
  force), `rewind doctor` (prints "action: run rewind reconcile", which
  cannot succeed). Deleting the journal file alone makes it worse
  (catalog row still references it → RecoveryRequired with baseline
  nulled).
- Expected: contract §State machine — "RECOVERY_REQUIRED — explicit
  recovery classification succeeds → HEALTHY"; T38 expects a diagnostic
  plus an actionable path.
- Actual: no command performs resolution; the advice doctor prints is
  unreachable.
- Root cause: `reconcile` (and every mutating command) call
  `recover_locked` first and treat its failure as fatal; there is no
  abandon/force-resolve/diagnose-and-clear surface.
- Impact: any RECOVERY_REQUIRED entry (including from V-F01) is
  permanent from the CLI.
- Recommended fix: add an explicit recovery/resolution command
  (e.g. classify-only mode, forced-abandon-to-reconciliation) and make
  doctor's advice match the actual available commands.

### V-F03 — P2 — Windows junctions classified as SYMLINK instead of UNSUPPORTED_OBJECT

- Component: `src/scan.rs`
- Location: `is_unclassified_reparse` (src/scan.rs:177-187)
- Reproduction: `mklink /J junc sub` under `rewind run`; captured
  effect kind is `Symlink` with the literal target, not
  `UNSUPPORTED_OBJECT` (V12).
- Expected: Phase 0.7 §8.4/T31 — junctions are UNSUPPORTED_OBJECT, no
  traversal or replacement.
- Actual: Rust std reports junctions as `is_symlink() == true`, so the
  reparse guard never fires; the junction is recorded as a symlink.
  Rollback of junction creation happened to be safe (namespace rename,
  target untouched), but a junction *restore* would create a real
  symlink with a guessed dir/file flavor instead of refusing.
- Impact: support-matrix violation; wrong-object restore where symlink
  privilege exists.
- Recommended fix: positively identify the reparse tag
  (IO_REPARSE_TAG_MOUNT_POINT etc.) via metadata attributes before
  trusting `is_symlink()` on Windows.

### V-F04 — P2 — Post-commit archive of quarantined files always fails on Windows

- Component: `src/rollback.rs`
- Location: `copy_artifact` (src/rollback.rs:967-972)
- Reproduction: any undo that quarantines a file (e.g. undo of A→B)
  with store on C: or D: → journal `archive_status: "Failed"`,
  `archive_error: "I/O error: Access is denied. (os error 5)"`;
  destination file fully copied. Directory-only backups archive fine.
- Expected: contract §10.2 — copy, hash verification, flush, publish;
  success or an honest pending/failed record (the record IS honest).
- Actual: `File::sync_all` on a handle opened read-only calls
  FlushFileBuffers, which requires write access on Windows → every
  file archive fails; `pending_archives` retries and re-fails on every
  later `recover_locked`.
- Impact: the archival capability never works for files; quarantined
  bytes survive only in never-cleaned staging (compounding V-F07).
- Recommended fix: open the copied destination with write access
  (or fsync via the write handle during copy) before sync_all.

### V-F05 — P2 — `rewind run` panics in debug builds (invalid clap argument configuration)

- Component: `src/cli.rs`
- Location: `Command::Run` arg definition (src/cli.rs:33-36)
- Reproduction: any debug build: `rewind run --help` →
  `panicked at clap_builder ...: 'Arg::trailing_var_arg' and 'Arg::last'
  cannot be used together` (exit 101). Release builds compile the
  assert out and parse correctly.
- Expected: the flagship command parses in every build profile.
- Actual: debug builds — the profile developers and `cargo test`/`cargo
  run` use — panic; tests never exercise the parser so it was missed.
- Impact: tooling/UX defect; masks the documented command surface in
  debug usage.
- Recommended fix: use `trailing_var_arg = true` alone (values are
  captured correctly in release with it; verify `--` handling), and add
  a CLI-level test.

### V-F06 — P2 — Journal and pointer writes are not flushed to disk

- Component: `src/paths.rs`, `src/journal.rs`, `src/workspace.rs`
- Location: `atomic_write` (src/paths.rs:80-116) — no `sync_all` on the
  temp file and no containing-directory flush; used for the external
  recovery journal, workspace pointer, and boundary log truncation.
- Expected: contract §11.3 step 2 — journal file and containing
  directory flushed as supported.
- Actual: CAS objects and installed files are synced; journal writes
  are rename-atomic but not durable-requested. `kill -9` tests cannot
  distinguish this; real power loss can lose the authoritative physical
  journal record.
- Impact: crash-durability weaker than the documented protocol on all
  platforms.
- Recommended fix: sync the temp payload (and parent directory where
  the platform accepts it) before rename inside `atomic_write`.

### V-F07 — P3 — Transaction-local staging is never cleaned up

- Location: `execute_transition` (no post-commit/abort removal of
  `.rewind-txn/<txn>`); contract §10.1 calls staging "disposable".
- Evidence: 21 transaction directories (quarantined user bytes,
  installed copies) accumulated beside the verification workspaces.
- Impact: unbounded disk growth and indefinite retention of quarantined
  (potentially sensitive) content next to the workspace; T15's
  "clean staging" option absent.
- Recommended fix: remove local staging after Archived, or on
  startup for transactions that are COMMITTED and Archived.

### V-F08 — P3 — List/diff do not surface the unknown interval as a first-class boundary

- Evidence: `rewind list` shows CAPTURE_FAILED/BOUNDARY_ONLY rows but
  never the UNKNOWN_INTERVAL or the reconciliation checkpoint;
  `open_unknown` appears only in `status`; `diff` is per-operation only
  and never reports the gap as unknown. Contract §Unknown interval
  semantics requires list to display the gap as a first-class boundary
  and diff to report it unknown.

### V-F09 — P3 — Passive hook session correlation is broken out of the box

- Evidence: default session id is `shell-<pid>` of the hook process;
  pre and post run in different processes, so `pending_boundary` never
  matches unless the user exports `REWIND_SESSION_ID` or passes
  `--session` themselves. No shell integration scripts exist in the
  repository. Passive observation — a Phase 1 capability — is
  unreachable without hand-built integration.

### V-F10 — P3 — Second CLI serializes instead of refusing busy (T23 deviation)

- Evidence: two concurrent `rewind run` both succeed sequentially
  (blocking lease). Single-writer safety holds; the expected
  refuse/busy behavior differs.

### V-F11 — P3 — Reconciliation-failure transition keeps RECONCILIATION_REQUIRED

- Evidence: `reconcile_locked` scan failure calls
  `mark_reconciliation_required` (condition stays RECONCILIATION_
  REQUIRED); contract shows failure → DEGRADED (gate preserved either
  way). Cosmetic state-machine naming deviation.

### V-F12 — P3 — Fixed 50 ms passive scan deadline degrades real workspaces

- Evidence: 2500-file workspace guarantees a hook timeout on every
  passive command (observed 0.155 s hook runtime); the failure mode is
  safe but makes passive mode generate constant reconciliation churn.
  A sizing heuristic or configuration is needed before passive mode is
  usable beyond toy workspaces.

### V-F13 — P3 — Windows symlink flavor guessed from file extension

- Location: `create_symlink` (src/rollback.rs:672-688) picks
  `symlink_file` vs `symlink_dir` from whether the target string has an
  extension. Mis-typed restores possible; platform-scoped per the
  matrix but undocumented in code.

### V-F14 — P3 — Doctor diagnostics are incomplete

- Evidence: doctor verifies CAS artifacts only for the baseline state,
  not for anchors/snapshots/operation-referenced artifacts; does not
  report orphaned staging directories; its RECOVERY_REQUIRED advice is
  unreachable (V-F02).

## 18. Known Limitations (genuine, per architecture)

- No power-loss testing was possible; kill-based crash injection is
  simulation, not power-loss equivalence (contract-compliant honesty).
- Symlink creation/restore untestable on this account
  (SeCreateSymbolicLink absent); T12/T13/T29 depend on it.
- NTFS cannot host case-collision scenarios (T32).
- Linux/macOS could not even be cross-compiled (bundled SQLite C
  cross-toolchain absent).
- ACID across filesystem and SQLite is explicitly out of scope by
  design; observed recovery gaps (V-F01/V-F02) are implementation
  defects, not accepted design limits.
- Performance was measured only for sanity: init of 2500 files ≈ 7.2 s;
  passive hook bounded at 50 ms; undo of a 120-file creation ≈ 38 s
  (dominated by full-workspace rescans per step, O(steps × files));
  no benchmark targets exist to compare against.

## 19. Final Verdict

**PHASE 1 NOT VERIFIED**

Basis: one P0 (V-F01) and one P1 (V-F02) exist; core undo/redo
semantics are wrong for the most common directory scenario; crash
recovery cannot complete a known plan and the resulting lockout is
permanent from the CLI. The capture/observation/degradation/
reconciliation architecture is correctly implemented and independently
verified, and no unsafe data loss was observed in any scenario — the
rollback engine, not the trust model, is what fails. Phase 1 must not
be declared complete, and Phase 2 must not begin, until V-F01/V-F02 are
fixed and re-verified together with the pending matrix rows
(T12/T13/T25/T26/T28/T29/T30/T37) and a native MSVC/Linux/macOS run.
