# Phase 3 Implementation Handoff

Status: Phase 3 (continuous observation) complete and CI-verified
(run #36111890265 on `d69fdca`: ubuntu, macOS and windows all green; final
verdict in `.ai/PHASE_3_VERIFICATION_REPORT.md`). **Phase 3 is VERIFIED.**
All local gates green (fmt, check, clippy -D warnings, full suite 119
passed / 0 failed: 48 Phase 1 and 15 Phase 2 tests unchanged plus 43 new
Phase 3 tests). Phase 2 and Phase 1 handoff content is preserved below
unchanged.

**The crate has three documented minimal FFI sites:** `reparse_tag`
(`src/scan.rs`), `CreateProcessW` (`src/watch/detach_windows.rs`), and
`mkfifo(2)` (`src/rollback.rs` — `std::os::unix::fs::mkfifo` is unstable,
rust-lang/rust#139324; the FFI declares `mode_t` per platform ABI:
`c_ushort` on macOS, `c_uint` on Linux).

The Phase 3 audit fixes were merged into `main` via PR #1 (merge commit
`c4aeef7`; branch deleted). Phase 4's first slice — the read-only
`rewind inspect timeline` time-range view — is merged on `main` (contract
`.ai/PHASE_4_TIME_AND_ECOSYSTEM.md`; invariants in ADR-015; recipes
deferral in ADR-016). Phase 5's first slice — POSIX named pipes as
first-class objects, with the object×platform capability matrix — is
implemented on `main` (contract `.ai/PHASE_5_PLATFORM_EXPANSION.md`;
decision in ADR-017): FIFOs classify from file type alone, restore via
`mkfifo` with a private initial mode plus the recorded authoritative mode,
and never enter CAS. Do not widen the capability matrix without a contract
amendment.

## -3. Phase 3 watcher invariants (do not regress)

- The watcher is **advisory only**, and this is structural, not a promise:
  `src/watch/` never opens the catalog, never acquires `WorkspaceLease`,
  and never writes inside the workspace root. Its only durable outputs are
  `state.json`, `events.log(.1)`, `dirty.json`, `degraded.json`, and
  `stop.flag` under `<store>/projects/<id>/watch/`. No code path may read
  a watcher artifact as authoritative state.
- A watcher event means *"this path may have changed"* — nothing more. The
  `FsEvent` model has no command/boundary/session field by construction
  (§14): filesystem events never prove attribution. The only attribution
  mechanisms remain strong capture and the Phase 1.4 boundary protocol.
- Every watcher failure mode (adapter overflow, adapter failure, dirty-cap,
  restart gap) appends a **durable degradation record immediately**
  (`degraded.json`, atomic write). The catalog-level unknown interval is
  opened by the next lease-holding enforcement point —
  `enforce_watch_degradations` inside `enforce_pending_safety_gate` — and
  the marker is removed only after the interval and the
  RECONCILIATION_REQUIRED gate are durable. The watcher itself never gates
  the workspace and never closes an interval.
- **Any prior watcher run ⇒ unobserved gap.** `watch start` writes a
  `WATCHER_GAP` record spanning the previous run's last durable heartbeat
  (or `stopped_at`) to now — including after a graceful stop — before any
  new coverage is claimed. A first-ever start records nothing (no coverage
  was claimed before). Only `reconcile_locked` closes the interval.
- The serve loop does no scans, no hashing, no CAS work. Memory is bounded
  by the dirty cap (100 000 paths; past it the watcher degrades instead of
  growing), the event log rotates at 8 MiB, and heartbeats are at most one
  atomic write per 2 s. Coalescing appends raw evidence to `events.log`
  first — coalescing must never erase evidence. The adapter's pending
  event queue is likewise bounded (`PENDING_CAPACITY` = 4096): a mapped
  batch larger than the cap is truncated and surfaced as `Overflow` after
  the preserved prefix drains, so capacity exhaustion can only produce a
  degradation, never silent loss or unbounded growth.
- Watcher scope == scanner scope: the recursive tree under the canonical
  root excluding the root `.rewind` directory. Event paths are confined
  with the existing `src/paths.rs` helpers (`normalize_relative`); escapes
  are dropped and counted, never followed or expanded. Do not add a second
  path-security implementation.
- The passive hooks (§13) and the Phase 1.4/1.3 invariants below are
  untouched by the watcher and must stay that way: no hook code may join
  with watcher evidence, and no watcher event may carry a boundary id.
- The detached spawn on Windows is a raw `CreateProcessW` with
  `bInheritHandles = FALSE` (`src/watch/detach_windows.rs` — the crate's
  second documented `unsafe` site, alongside `reparse_tag` in
  `src/scan.rs`). Rationale: std always spawns with `bInheritHandles =
  TRUE` and offers no restriction API (rust-lang/rust#73281), so a daemon
  spawned via `std::process::Command` would inherit the caller's stdout
  pipe and hang every pipe-captured invocation forever. Do not replace it
  with a `Command` spawn.
- Lifecycle derivation (`src/watch/lifecycle.rs`): pending degradations
  dominate as RECONCILIATION_REQUIRED; a stale heartbeat means `FAILED`,
  never a fabricated `RUNNING`; liveness is heartbeat-derived (no pid
  queries). `watch status` is read-only; `watch stop` is bounded and
  signal-free (no pid kill).

---

# Phase 1.4 Implementation Handoff

Status: Phase 1.4 (passive boundary identity) complete and CI-verified
(run #19 on `4b5dddb`: ubuntu, macOS and windows all green); final verdict in
`.ai/PHASE_1_4_BOUNDARY_CORRELATION_REPORT.md`. **Phase 1 is VERIFIED.**
Phase 2 (dependency-aware inspection) is implemented and CI-verified (run #23 on
`4d89a2f`: ubuntu, macOS and windows all green): see
`.ai/PHASE_2_VERIFICATION_REPORT.md` for its contract, evidence and verdict.
Phase 1.3 and earlier handoff content is preserved below unchanged.

## -2. Phase 1.4 boundary-identity invariants (do not regress)

- Every passive post-hook is correlated to exactly one immutable boundary ID.
  A post-hook never discovers or guesses its boundary by recency, timestamp,
  command text, cwd, or session ordering. `pending_boundary` is deleted and
  must never return; do not add any "newest/oldest unconsumed" lookup.
- `rewind hook pre` prints the boundary id as its single stdout line;
  `rewind hook post` REQUIRES `--boundary` and claims exactly that boundary
  exactly once (guarded `UPDATE ... consumed = 0`). Unknown, duplicate, or
  foreign ids fail open with no side effects.
- The bash/zsh integrations hold the id in a shell-local variable for exactly
  one command and pass it to the background post-hook. Never let the id leak
  across commands.
- Out-of-order background completion advances the trusted checkpoint in lease
  order only; an older background observation finishing later must never
  regress newer trusted state.
- `tests/boundary_correlation.rs` owns the identity proof (older-before-newer
  ordering, genuine three-way overlap via the lease barrier,
  duplicate/unknown/cross-workspace cases). Do not replace it with
  timing-dependent tests.
- The Phase 1.3 invariants below remain in force (async post-hook, deleted
  `HOOK_BUDGET_MS`, 50 ms scan deadline from scan start, background-only
  lease retry, no `recover_locked` in the hook path).

---

# Phase 1.3 Implementation Handoff

Status: Phase 1.3 (passive hook isolation) complete and CI-verified;
final verdict in `.ai/PHASE_1_3_FINAL_FOUNDATION_REPORT.md`. Phase 1.2
and Phase 1.1 handoff content is preserved below unchanged.

## -1. Phase 1.3 nonblocking-hook invariants (do not regress)

- The shell integrations launch `rewind hook post` in the background
  (nohup, detached, stdio detached). NEVER make the post-hook synchronous
  again; NEVER let a hook subprocess failure change the command's exit
  status.
- The pre-hook stays synchronous and lightweight (marker discovery +
  catalog open + one INSERT). Do not add scans, recovery, CAS, or
  archive work to it.
- `HOOK_BUDGET_MS` is gone and must stay gone: no constant may promise a
  total wall time for the hook process. The only bound is
  `HOOK_SCAN_DEADLINE_MS` (50 ms), measured from the start of the scan.
- `recover_locked` must never run in a hook path; unfinished
  transactions make the hook record a bypass marker and return.
- Any hook error records a durable bypass marker before propagating;
  incomplete background work becomes uncertainty (bypass /
  CAPTURE_FAILED / unknown interval), never a fabricated operation.
- `tests/shell_integration.rs` owns the nonblocking proof (stub 45 s
  post-hook ordering test). Do not replace it with a wall-clock
  threshold.

---

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
