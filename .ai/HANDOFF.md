# Phase 1.2 Implementation Handoff

Status: Phase 1.2 (final foundation hardening) complete and pushed;
cross-platform CI verification recorded in
`.ai/PHASE_1_2_HARDENING_REPORT.md`. Phase 1.1 handoff content is
preserved below unchanged.

## 0. Phase 1.2 hardening invariants (do not regress)

- The passive hook is bounded and fail-open: 50 ms scan deadline, 150 ms
  total budget, durable bypass markers on overrun or unavailable lease,
  exit 0 for the shell always. Deferred transaction recovery is *writer*
  work; a hook that finds an unfinished transaction records a marker and
  returns. Never let the hook pay unbounded recovery cost.
- The writer lease opens `lock.pid` without truncation and writes owner
  metadata only after winning the exclusive lock. A contender that loses
  the race must not destroy the holder's record.
- Every rollback step re-verifies path confinement *after* its mutation.
  The accepted residual race is the interval between the final
  pre-mutation check and the mutation itself; anything wider must be
  detected and refused.
- Archives are marked Archived only after recursive verification (type,
  size, BLAKE3 content, symlink targets, directory shape). Local
  quarantine staging is disposed only after that verification. A
  shallow pass must never authorize deleting the only surviving copy.
- Windows symlink restoration uses the recorded target kind (reparse
  data at scan time). Unknown kinds refuse restoration — never guess,
  and never infer from a filename extension.
- State identity digests the schema-versioned envelope
  (`STATE_SCHEMA_VERSION = 2`). Persisted v1 manifests deserialize with
  `target_kind = unknown`. Version bumps must make state ids differ;
  they never invalidate old rows silently.
- Startup repair of committed metadata is idempotent and
  journal-authoritative; it never mutates the filesystem.
- A failed recovery sets RECOVERY_REQUIRED (anchored at the unfinished
  transaction) before the error propagates. No code path may leave a
  failed recovery looking HEALTHY.

## 0.1 Cross-platform verification facts

- CI (`.github/workflows/ci.yml`): ubuntu-latest, macos-latest,
  windows-latest (MSVC), each running fmt --check, check --all-targets,
  clippy -D warnings, cargo test. Failing output is surfaced as
  annotations because raw job logs need admin auth on this repo.
- POSIX-only clippy findings are invisible on a Windows host; the two
  found during Phase 1.2 (`unneeded return` under `#[cfg(unix)]` in
  `scan::metadata_fingerprint` and `tests/common`'s symlink helper) are
  the reason CI must stay authoritative for non-host platforms.
- `tests/common/mod.rs` owns platform portability (cmd/.cmd vs sh/.sh,
  CRLF vs LF). Never inline shell commands in tests.

---

# Phase 1.1 Implementation Handoff (preserved)

Status: Phase 1.1 corrective phase complete; implementation under
re-verification

## 1. Authority

Read these in order:

1. .ai/PHASE_0_7_ARCHITECTURE_FINALIZATION_REPORT.md
2. phases/phase-01-foundation.md
3. .ai/PHASE_1_INDEPENDENT_VERIFICATION.md
4. .ai/PHASE_1_1_REPAIR_REPORT.md
5. .ai/DECISIONS.md
6. .ai/ARCHITECTURE_PROPOSAL.md

If an earlier document disagrees, the Phase 0.7 report and Phase 1 contract
control. The independent verification report documents the defects that
Phase 1.1 repaired; do not treat the pre-repair implementation report as
evidence of correctness.

## 2. Non-negotiable design

- A failed capture invalidates current trust even if the baseline pointer is
  numerically unchanged.
- DEGRADED becomes RECONCILIATION_REQUIRED after the gap marker is durable.
- Passive commands during the gate are untrusted boundaries, not operations.
- Rewind run reconciles before execution and refuses when reconciliation fails.
- UNKNOWN_INTERVAL is never converted into a guessed operation.
- ABSENT is a first-class path state.
- Directories use canonical child manifests; symlinks are literal leaves.
- Unsupported objects and metadata are explicit.
- Critical rollback movement is same-filesystem local staging/quarantine.
- External archival is post-commit and may be cross-device.
- Journal recovery verifies source and destination state maps.
- SQLite does not commit the filesystem.
- Linux, macOS, and Windows semantics are documented separately.
- Guarantees are conditional and include refusal/unknown outcomes.

## 3. Implementation status

The following foundation order has been implemented in the current package
and is being verified against the contract:

1. capability and support policy;
2. workspace identity and state condition storage;
3. typed manifest and CAS artifact protocol;
4. reconciliation and strong capture;
5. external journal and same-filesystem staging;
6. rollback step execution and physical recovery;
7. passive hook boundary integration;
8. inspection and diagnostics;
9. adversarial platform tests;
10. Phase 1.1 repairs (transaction-aware step expectations, `rewind
    recover`/`recover --reconcile`, reparse-tag classification, archive
    flush, durable journal writes, staging cleanup, bash/zsh integration).

## 3.1 Transaction-aware rollback expectations (V-F01 fix)

A rollback step's recorded before-map describes the state the filesystem
must be in when that step executes, not the pre-transaction snapshot.
Because removals always run before installations and deeper paths run
before shallower removals, a directory's execution-time expectation is the
directory restricted to the children that survive in the target state. Any
object outside that expectation still trips the conflict gate. Do not
revert to snapshot-based per-step comparison, and do not skip conflict
checks for in-flight transactions.

## 3.2 Recovery exit path (V-F02 fix)

`rewind recover` classifies and completes unfinished transactions when the
physical state matches a known plan. When a step matches neither the
expected before nor after state, the workspace stays RECOVERY_REQUIRED and
`rewind recover` prints a per-step physical classification.
`rewind recover --reconcile` is the only documented exit: it archives every
existing quarantine artifact, marks the journal ABANDONED (terminal),
records an unknown interval from the transaction anchor, and reconciles the
live state into a new trusted checkpoint. Never weaken this into a silent
healthy transition.

## 4. Stop conditions

Stop and expose a refusal when:

- the root identity or volume changes unexpectedly;
- a required path is unknown, unsupported, or unreadable;
- the workspace is degraded and reconciliation cannot complete;
- source/destination states do not match a known journal map;
- a platform lacks the required same-filesystem operation;
- a writer lease or external lock makes the mutation unsafe;
- CAS or journal durability cannot be established.

Do not add a fallback that turns one of these conditions into a success
message.
