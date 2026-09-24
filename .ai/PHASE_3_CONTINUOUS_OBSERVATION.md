# Phase 3 — Continuous Observation & Background Indexer: Implementation Contract

Status: **contract only; no Phase 3 code exists yet.** This document is the
Phase 3 semantic contract required before implementation. It defines what a
watcher may claim, what it may never claim, where its records live, and where
the phase stops.

Phase 1 (through 1.4) is the frozen safety foundation and Phase 2 is the
verified inspection layer. CI run #23 on `4d89a2f` is green on ubuntu-latest,
macos-latest and windows-latest. Nothing in this document may weaken a Phase 1
or Phase 2 invariant; where Phase 3 appears to need one relaxed, the conflict
is to be reported, not resolved silently.

---

## 1. Scope and phase boundary

Phase 3 delivers **optional, advisory, continuous observation**: a background
watcher/indexer for an explicitly initialized workspace that records which
paths *may have changed* between Rewind commands, and — when the watcher
itself is unreliable — makes that uncertainty durable so the existing
reconciliation machinery can close it.

The pipeline is fixed:

```text
WATCHER → ADVISORY EVENTS → EVENT INDEX → POSSIBLE AFFECTED PATHS
        → RECONCILIATION / SCAN → AUTHORITATIVE STATE
```

The watcher is **never authoritative**. It never proves that a physical state
is complete. It never advances a baseline, never creates an operation, never
closes an unknown interval, and never participates in a rollback decision.

Explicitly out of scope: command attribution from filesystem events (§14);
replacing the passive hooks (§13); a privileged daemon or a system-wide
watcher (§8); time-range views and recipes (Phase 4); platform expansion
beyond the three CI platforms (Phase 5); any redesign of Phase 1/2 modules.

---

## 2. What the existing code provides (surveyed, with citations)

The contract is grounded in the code at the Phase 2 head (`0dfa485`).

1. **The writer lease serializes mutations** (`WorkspaceLease`,
   `src/workspace.rs:65-119`): an exclusive `fs2` lock on
   `lock.pid`, opened without truncation, owner metadata written only after
   winning. It covers capture commits, reconciliation, rollback, redo,
   restore, and recovery. A passive hook that cannot get the lease falls
   back to a durable bypass marker (`src/cli.rs:827-833`, `983-1001`); the
   next writer's `enforce_pending_safety_gate`
   (`src/workspace.rs:430-452`) opens an unknown interval and sets
   `RECONCILIATION_REQUIRED`.
2. **Unknown intervals are the durable uncertainty record**
   (`unknown_intervals`, `src/db.rs:148-157`; `open_unknown`/`close_unknown`
   at `src/db.rs:439-462`). Only a successful `reconcile_locked`
   (`src/workspace.rs:370-414`) closes them, and only after a complete scan.
3. **The store lives outside the workspace** (`Storage::open`,
   `src/workspace.rs:32-51`): `<store>/projects/<id>/`. The scanner skips
   only the root `.rewind` metadata directory (`src/scan.rs:102`).
   Watcher artifacts must not perturb scans — they belong in the store.
4. **The catalog is already multi-connection SQLite (WAL)** — hooks, readers
   and writers each open their own connection — but the *filesystem*
   single-writer rule (ADR-014) protects physical workspace transitions.
   The watcher needs no catalog connection at all (see §6); that is what
   makes its isolation testable.
5. **Path confinement is centralized** in `src/paths.rs`
   (`normalize_relative`, `validate_relative`, `ensure_parent_confinement`).
   Phase 3 must reuse it, not fork it.
6. **`Workspace::observe` (Phase 2)** computes state identity without CAS
   writes (`src/workspace.rs:266-272`); the watcher does not scan at all,
   but its scope definition must match the scanner's coverage (§9).
7. **Determinism and structured values are house rules** (Phase 2 contract
   §7, §10): machine-readable artifacts are typed, sorted, and serialized
   identically for identical inputs.

---

## 3. Non-negotiable constraints

- **C1 — Advisory only.** A watcher event means *this path may have
  changed*. It never means *this path now equals X*. No watcher artifact may
  ever be read as authoritative state by any Phase 1/2 code path.
- **C2 — No catalog writes, no lease, no workspace mutation.** The watcher
  process never opens `metadata.sqlite`, never acquires `WorkspaceLease`,
  and never writes inside the workspace root. Its only durable outputs live
  in `<store>/projects/<id>/watch/`.
- **C3 — Uncertainty is always made durable immediately.** Overflow, adapter
  failure, crash-gap detection, and dirty-set overflow each append a
  degradation record at the moment they are detected. The catalog-level
  unknown interval is opened by the next lease-holding enforcement point
  (single-writer discipline); the degradation marker itself is the
  immediate conservative record and is honored by every writer from that
  moment on.
- **C4 — Unknown remains unknown.** Nothing in the watcher may convert a
  gap, an overflow, or "no events observed" into `KNOWN CLEAN`. Only
  `reconcile_locked` closes uncertainty, exactly as before.
- **C5 — One mutation engine.** Enforcement reuses
  `open_unknown`/`set_workspace`/`reconcile_locked` unchanged. No second
  gate, no second reconciliation path.
- **C6 — Passive hooks are untouched.** `hook pre`/`hook post`, the bash/zsh
  integrations, `HOOK_SCAN_DEADLINE_MS`, and the Phase 1.4 boundary-identity
  protocol keep functioning exactly as verified. The watcher and the hooks
  solve different observation problems and are never joined.
- **C7 — Determinism.** The dirty index is a sorted set; serialized
  artifacts are byte-identical for identical inputs; no HashMap iteration
  order reaches a durable artifact.
- **C8 — Bounded resources.** The watcher performs no scans, no hashing, no
  CAS ingestion in its steady-state loop; its memory and on-disk footprint
  are capped and overflow conservatively (§11).

---

## 4. Event model

Platform-neutral internal event, produced by platform-specific adapters:

```text
FsEvent {
    run_id:      Uuid,   // identity of the watcher process run (provenance)
    sequence:    u64,    // per-run arrival-order counter assigned by the loop
    path:        String, // workspace-relative, normalized ('/' separators)
    kind:        EventKind,  // Create | Modify | Delete | Rename | Other
    from_path:   Option<String>, // Rename source when the platform supplies it
    timestamp:   i64,    // microseconds; OBSERVATION time, not occurrence time
    source:      EventSource, // Inotify | FSEvents | Windows | Fake
}
```

- Every event is advisory. There is no confidence field because there is
  only one confidence: advisory (Phase 2 keeps the Known/Advisory
  distinction in the type; the watcher has nothing that would ever be
  Known).
- `timestamp` records when Rewind saw the event, because platform event
  times are not portable evidence. Documented limitation: arrival order is
  per-run and not guaranteed to match occurrence order.
- `kind` is the adapter's best mapping of a platform notification; the
  authoritative scan, not the event, decides what actually happened.
  `Other` covers access notifications and anything unrecognized.

## 5. Platform adapters

The adapter layer is the `EventAdapter` trait; production adapters use the
`notify` crate, which drives the platform-native mechanism directly:
inotify on Linux, FSEvents on macOS, ReadDirectoryChangesW on Windows.
Rationale: these are the platform mechanisms named by the phase objective;
hand-rolling three FFI watchers would add a second unsafe surface without
changing the semantics this contract cares about. What Phase 3 keeps
explicit, rather than delegating silently:

- **Overflow is a first-class outcome.** Every platform surfaces
  queue/queue-loss conditions (`IN_Q_OVERFLOW`, FSEvents
  `UserDropped`/`MustRescanSubTree`, Windows buffer overflow/rescan). All of
  them map to one internal outcome: `Degradation::Overflow`.
- A deterministic **FakeAdapter** implements the same trait for tests, so
  the state machine is exercised without platform scheduling (§15).
- Adapter failures (watch root errors, OS errors) map to
  `Degradation::AdapterFailed` with the error text; the loop attempts a
  bounded restart, and gives up as `FAILED` — never silently.

## 6. Watcher storage and process model

Smallest safe architecture: **one user-level, per-workspace watcher
process**, no privileges, no daemon manager. Files live under
`<store>/projects/<id>/watch/` (outside the workspace):

```text
state.json      lifecycle + heartbeat (atomic write; pid is diagnostic only)
events.log      raw FsEvent JSONL evidence, append-only, rotated (one .1)
dirty.json      coalesced dirty-path index (atomic write, sorted, capped)
degraded.json   pending degradation records (atomic write; consumed by
                the enforcement point, never by the watcher)
stop.flag       stop request, polled by the loop (portable, no signals)
```

- `start` spawns a detached `rewind watch serve --root <root>` child
  (own process group on POSIX, detached/no-window on Windows);
  `--foreground` runs the loop in-process for terminals and tests.
- `stop` writes `stop.flag`; the loop exits within one batch window and
  writes a final `STOPPED` state. `stop` waits a bounded time and reports
  honestly if the watcher does not stop; it does not kill by pid.
- The serve loop derives its context from the workspace pointer
  (`discover_marker` + `read_pointer` + canonical-root check) and **never
  opens the catalog**. Liveness is derived from heartbeat freshness, not
  pid queries (heartbeat every 2 s; a stale heartbeat means presumed dead).

## 7. Lifecycle

Recorded status: `STARTING`, `RUNNING`, `STOPPING`, `STOPPED`, `FAILED`,
plus `degraded` (running but a degradation occurred this run).

Derived lifecycle (what `rewind watch status` reports; precedence order):

```text
RECONCILIATION_REQUIRED  pending degradation records exist (workspace gate)
FAILED                   state says RUNNING/STARTING but heartbeat is stale
STOPPING                 state says STOPPING and heartbeat is fresh
DEGRADED                 RUNNING and degraded, heartbeat fresh
RUNNING                  RUNNING, not degraded, heartbeat fresh
STOPPED                  no state, or a final STOPPED/STOPPING-dead record
```

`RECONCILIATION_REQUIRED` dominates because it is the workspace-facing
gate; it exists until reconciliation consumes it. A `FAILED` watcher never
fabricates health; the state file keeps the last heartbeat and the restart
gap logic (§10) covers everything since it.

## 8. Watched scope

- Exactly one workspace root, resolved canonically. The watcher never
  watches `/`, `$HOME`, the store, or anything the user did not init.
- Coverage equals scanner coverage: the recursive tree under the root,
  excluding the root `.rewind` metadata directory. Events for `.rewind/...`
  are dropped by the loop, not by the adapter, so the rule is platform-
  independent.
- Every event path is confined: absolute path must be inside the canonical
  root, and the workspace-relative form must pass `normalize_relative`.
  Escapes and non-Unicode names are dropped and counted as anomalies
  (exposed in `state.json`); they are never watched through, followed, or
  expanded. Symlinked directories inside the tree are not special-cased:
  events about them are advisory evidence, and the no-follow scanner
  remains the only authority for what the object actually is.
- Nested workspaces: their trees are inside this workspace's scanner
  coverage, so their events are recorded here as advisory evidence; the
  nested workspace's own scan remains authoritative for it. No exclusion,
  no second watcher coordination — documented, not engineered.
- Network/external filesystems: if the platform adapter fails on them, the
  failure takes the degradation path. No capability is claimed that the
  adapter does not deliver.

## 9. Coalescing

The loop drains the adapter in batches (batch window, default 250 ms) and
coalesces into the dirty index:

- The dirty index is the **union of affected workspace-relative paths** —
  the only safe summary of duplicated, reordered, coalesced evidence.
  `MODIFY a.txt` × 4 and `MODIFY a.txt` × 1 are the same dirty entry.
- Raw events are **never deleted** by coalescing: every event is appended
  to `events.log` first (evidence preservation), the index is derived.
- No create/delete reconstruction, no rename pairing, no ordering claims
  across runs. A create followed by a delete inside one window leaves the
  path dirty — "may have changed" — which is exactly what the evidence
  supports. The authoritative diff (`diff_manifests`) does the real pairing
  during reconciliation.
- Cross-batch duplicates also collapse, because the index is cumulative
  until reconciliation or reconciliation-prep reads it. The index is
  **not** auto-cleared by the watcher on reconciliation (it does not know
  when one happened); it is a diagnostic of "what may have changed since
  the last explicit consumer action" and is rotated when consumed (§12).

## 10. Crash, restart, and offline semantics

- The watcher writes a heartbeat at least every 2 s. Death (crash, kill,
  power loss, machine sleep) is indistinguishable from stopping — treated
  the same: **conservatively**.
- On `watch start`, if a previous watcher run for this workspace exists in
  `state.json` (any status, including graceful `STOPPED`), the interval
  from the last durable heartbeat (or `stopped_at`) to now is an
  **unobserved gap**: a degradation record
  (`reason = WATCHER_GAP`, with from/to timestamps) is written before the
  new run claims coverage. Example: healthy at 10:00, died at 10:17,
  restarted at 10:30 → the 10:17→10:30 span is potentially unknown until
  reconciliation closes it.
- First-ever start (no prior state file) writes **no** gap marker: no
  coverage was claimed before, so nothing is broken. `coverage_started_at`
  records the moment observation begins; nothing earlier is claimed
  observed.
- Offline semantics (files changed while stopped): the same rule — the
  watcher does not pretend it saw those changes. The gap marker (restart
  case) or the existing baseline-drift rules (first-start case) route the
  uncertainty to reconciliation. `watch status` reports coverage explicitly
  and recommends `rewind reconcile` whenever uncertainty is pending.

## 11. Overflow and resource bounds

- Adapter overflow → drain the queue, append a degradation record
  (`OVERFLOW`, timestamp, workspace, run), keep the dirty set, attempt one
  adapter restart to resume observation. The record is only ever consumed
  by reconciliation (via §12). The watcher does not mark anything healthy.
- Dirty-set cap (default 100 000 paths): at the cap the watcher stops
  adding and appends a degradation record (`DIRTY_CAP`). Memory stays
  bounded; honesty is preserved by making the interval unknown.
- Event log cap (default 8 MiB): before an append that would exceed it,
  `events.log` rotates to `events.log.1` (one generation). Rotation is
  evidence-preserving within the documented cap; the catalog remains the
  durable history.
- Heartbeat/state writes: at most one atomic write per 2 s in steady state;
  dirty.json only when the set changed. No database writes exist to run
  away.

## 12. Enforcement: how uncertainty reaches the authoritative layer

`enforce_pending_safety_gate` (`src/workspace.rs:430-452`) is extended: in
addition to the pending bypass markers it already checks, it checks
`degraded.json`. When pending degradation records exist it:

1. opens an unknown interval (unless one is already open) with a reason
   summarizing the pending records,
2. sets `RECONCILIATION_REQUIRED`,
3. and only then removes the marker (crash-safe: a crash between 1 and 3
   repeats 1 idempotently; the marker is removed last).

This runs exclusively in existing lease-holding or diagnostic enforcement
points — the same places bypass markers are honored today — plus the two
reconciliation entry points that do not call it today (`rewind reconcile`
and `rewind recover --reconcile`), which must consume the marker so a
successful reconciliation does not leave a stale gate behind. The watcher
itself never performs step 1-3.

## 13. Relationship to the passive hooks

The hooks stay as verified. `rewind hook pre/post` keep their protocol,
budgets, and fail-open model; the bash/zsh integrations are unchanged. The
watcher records between-command evidence and degradation gaps; the hooks
record command boundaries. No code path joins them: watcher events carry no
boundary id and boundaries carry no watcher evidence.

## 14. Event attribution

A filesystem event cannot prove which command caused a change, and Phase 3
makes no such claim anywhere: the `FsEvent` model has no command, boundary,
or session field by construction. The only attribution mechanisms in this
repository remain the Phase 1 strong capture and the Phase 1.4
boundary-identity protocol. Watcher evidence answers exactly one question —
*what may have changed since the last consumer action* — and the contract
forbids widening it.

## 15. Testing obligations

- **Deterministic state-machine tests (FakeAdapter):** coalescing
  (duplicate modify, create+delete, cross-batch), overflow injection
  (degradation record written, dirty set kept, state degraded, enforcement
  opens the interval, reconcile closes it), adapter failure, dirty-cap
  behavior, path confinement (escape dropped, `.rewind` dropped, anomalies
  counted), event-log evidence preservation, byte-identical serialized
  index for identical inputs.
- **Crash/restart tests:** stale heartbeat state → `start` writes a
  `WATCHER_GAP` record spanning last-heartbeat→now, before any new
  coverage is claimed; graceful stop → same rule; first-ever start → no
  gap marker.
- **Isolation tests:** the serve loop opens no catalog (metadata.sqlite
  byte-identical before/after), acquires no lease (a concurrently held
  lease does not affect it and vice versa), and writes nothing inside the
  workspace root.
- **Real-filesystem lifecycle tests:** `start`/`status`/`stop` with the
  real adapter — lifecycle transitions, heartbeat, stop within the bounded
  window; one real event round-trip with generous timeouts and honest
  skip-if-no-events handling (platform schedulers vary; assertions on
  lifecycle, not on timing).
- **Enforcement integration:** a pending marker gates `run`/`undo`/`doctor`
  (RECONCILIATION_REQUIRED + unknown interval naming the watcher reason)
  and is consumed by successful reconciliation.
- **Regression:** every Phase 1/2 suite unchanged and green (76 tests).

Verification gate: `cargo fmt --all -- --check`, `cargo check --all-targets
--all-features`, `cargo clippy --all-targets --all-features -- -D warnings`,
`cargo test --all-targets --all-features`, and green CI on ubuntu-latest,
macos-latest and windows-latest for the exact pushed commit. Local Windows
results alone never justify "Phase 3 verified".

## 16. Performance

Measured, not promised. Record: idle CPU (one recv per batch window), the
event→dirty latency for a small burst, dirty-index write size at a few
thousand paths, and the memory-bound behavior at the cap. No latency SLA is
claimed; no optimization work in scope unless a measurement shows the
watcher is unusable.

## 17. What would make Phase 3 fail

- Any watcher artifact treated as authoritative state by any code path.
- A baseline advance, operation, or interval close performed from watcher
  evidence.
- Overflow or a crash gap silently swallowed (no durable degradation
  record).
- A watcher crash leaving the workspace apparently HEALTHY *because of* the
  watcher (it must remain healthy-or-not exactly as Phase 1 decides).
- A second path-security implementation instead of `src/paths.rs`.
- Catalog writes or lease acquisition from the watcher process.
- A regression in any Phase 1/2 suite, or a change to the hook protocol.
- Unbounded memory, unbounded disk growth, or per-event scans/hashing.
