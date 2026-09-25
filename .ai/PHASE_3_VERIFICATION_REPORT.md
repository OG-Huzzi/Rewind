# Phase 3 — Continuous Observation: Verification Report

Status: implementation complete against the Phase 3 contract; **CI green on
ubuntu-latest, macos-latest and windows-latest for the pushed commit.**

**Verdict: PHASE 3 VERIFIED.** The verification gate (contract §15) is met
by run #36111890265 on `d69fdca` (ubuntu 1m37s, macOS 1m42s,
windows 5m37s; zero non-success check runs). Local gates on
x86_64-pc-windows-gnu (Rust 1.98.1): fmt/check/clippy (-D warnings) clean,
full suite **119 passed / 0 failed**, all 63 Phase 1/2 tests unchanged.
Three CI rounds were needed; every round's finding is recorded in §9.

---

## 1. Baseline and contract

- **Phase 2 baseline:** `0dfa485`, the verified Phase 2 head (CI run #23 on
  `4d89a2f` green on ubuntu/macOS/windows; sign-off record pending, out of
  scope here). The baseline was reproduced on this machine before any
  change: fmt/check/clippy (-D warnings) clean and **76 passed / 0 failed**.
- **Contract:** `.ai/PHASE_3_CONTINUOUS_OBSERVATION.md`, commit `6d18dab`,
  amended before implementation for the detachment mechanism (§6) with the
  rationale recorded in this report (D2).
- **Phase 3 commits:** `6d18dab` (contract), `77197a3` (watcher subsystem),
  `a330a41` (enforcement + watch CLI), `7d41073` (verification suite), plus
  the commit carrying this report.

---

## 2. What was implemented

| Contract clause | Artefact | Evidence |
| --- | --- | --- |
| §4 platform-neutral event model | `src/watch/model.rs` `FsEvent` | run id + per-run sequence (provenance), observation-time timestamps, `EventKind`/`EventSource`; no command/boundary field anywhere (§14 by construction) |
| §5 adapters | `src/watch/adapter.rs` | `EventAdapter` trait; `NotifyAdapter` over `notify` 8.2 (inotify/FSEvents/ReadDirectoryChangesW); overflow mapped uniformly from the `Rescan` flag (`IN_Q_OVERFLOW`, FSEvents drop flags); deterministic `FakeAdapter` |
| §6 watcher storage | `WatchPaths` | `state.json`/`events.log(.1)`/`dirty.json`/`degraded.json`/`stop.flag` under `<store>/projects/<id>/watch/`, outside the workspace |
| §6 process model | `watch::start/serve/stop` | one user-level per-workspace process; detached spawn with **no handle inheritance** (see D2); heartbeat-derived liveness, no pid queries |
| §7 lifecycle | `src/watch/lifecycle.rs` | `Stopped/Running/Degraded/ReconciliationRequired/Stopping/Failed`; pending degradations dominate; stale heartbeat ⇒ `Failed`, never a fabricated `Running` |
| §8 scope | `run::confine_raw` | canonical-root confinement + `normalize_relative`; `.rewind` excluded (scope == scanner scope); escapes/non-Unicode dropped and counted as anomalies |
| §9 coalescing | serve loop + `DirtyIndex` | cumulative sorted union of affected paths; raw events never erased (appended first); no create/delete reconstruction, no rename pairing |
| §10 crash/restart/offline | `watch::record_start_gap` | any prior run (including graceful stop) ⇒ `WATCHER_GAP` from last durable heartbeat/stop to now, written before new coverage is claimed; first-ever start claims nothing |
| §11 overflow/bounds | loop degradation paths | `OVERFLOW`/`AdapterFailed`/`DirtyCap` records appended immediately; dirty cap 100 000; event log rotated at 8 MiB (one generation); heartbeat ≤ 1 atomic write/2 s |
| §12 enforcement | `workspace.rs` + `watch::enforce_watch_degradations` | pending marker ⇒ unknown interval + `RECONCILIATION_REQUIRED` inside the existing enforcement points; marker removed only after the gate is durable; `rewind reconcile` and `recover --reconcile` consume it so no stale gate survives a successful reconcile |
| §13 hooks untouched | zero changes to `hook pre/post` or bash/zsh integration | all Phase 1 suites green unchanged (48 tests) |
| §16 performance | §7 below | measured, no SLA claimed |

---

## 3. Conformance matrix

| # | Contract obligation | Status | Evidence |
| --- | --- | --- | --- |
| 1 | Study the repository before writing code | done | the mandated `.ai/` corpus and all `src/` modules read before the contract; citations in contract §2 |
| 2 | Contract before code | done | `6d18dab` |
| 3 | Watcher never authoritative | done | no catalog open, no lease, no workspace writes; isolation test below |
| 4 | Unknown remains unknown | done | only `reconcile_locked` closes intervals; watcher has no path to `HEALTHY` |
| 5 | Overflow ⇒ durable degradation + reconciliation | done | fake-adapter injection test, end to end through enforcement and reconcile |
| 6 | Crash/offline gaps conservative | done | gap tests: crashed, graceful-stop, first-start cases |
| 7 | Passive hooks unchanged | done | hook files untouched; Phase 1 suites green |
| 8 | Scope confined, no second path-security impl | done | reuses `normalize_relative`; `.rewind` rule matches `scan.rs:102` |
| 9 | Single-writer intact | done | enforcement runs inside existing lease-holding paths only; concurrent-writer test |
| 10 | Bounded resources | done | caps + degradation; no scans/hashing/CAS in the loop |
| 11 | Determinism | done | sorted `BTreeSet` serialization test; no map iteration in artifacts |
| 12 | Performance measured | done | §7 |
| 13 | Documentation/state handoff | done | this report + CURRENT_STATE/HANDOFF/TEST_STATUS/TODO/CHANGELOG/README |
| 14 | CI gate | done | §9: run #36111890265 on `d69fdca`, all three platforms |

---

## 4. Phase 1/2 invariants verified unchanged

| Invariant | Enforcement | Evidence |
| --- | --- | --- |
| Writer lease semantics | `WorkspaceLease` untouched | watcher never acquires it; concurrent-writer test passes |
| Unknown intervals | `open_unknown`/`close_unknown` untouched | enforcement only ever *opens*; reconcile closes; test asserts closure only via `reconcile_locked` |
| One mutation engine | `rollback::*` untouched | zero diff on `src/rollback.rs`, `src/plan.rs`, `src/depgraph.rs`, `src/ui.rs`, `src/db.rs`, `src/scan.rs`, `src/cas.rs`, `src/journal.rs` |
| Hook protocol/budgets | `HOOK_SCAN_DEADLINE_MS`, boundary identity untouched | `shell_integration` 8/8, `boundary_correlation` 10/10 green |
| Failed capture model | `record_capture_failure` untouched | `foundation` 13/13, `hardening` 8/8 green |
| Phase 2 read-only planning | `observe` path untouched | `phase2_dependency` 15/15 green |

No Phase 1/2 file's behavior changed except by addition: one call at the end
of `enforce_pending_safety_gate`, `enforce_pending_safety_gate()` calls in
the two reconcile-only CLI paths, and the new `watch` command arm.

---

## 5. Tests

**Full suite: 119 passed / 0 failed** (`cargo test --all-targets`,
x86_64-pc-windows-gnu, with bash on PATH so the shell-integration tests ran
rather than skipped).

| Target | Tests | Note |
| --- | --- | --- |
| lib unit (`depgraph` 8 + `ui` 5 + `watch` 28) | 41 | Phase 3 adds model/adapters/loop/lifecycle/detach units |
| `boundary_correlation` | 10 | Phase 1, unchanged |
| `foundation` | 13 | Phase 1, unchanged |
| `hardening` | 8 | Phase 1, unchanged |
| `phase2_dependency` | 15 | Phase 2, unchanged |
| `rollback_tree` | 9 | Phase 1, unchanged |
| `shell_integration` | 8 | Phase 1, unchanged |
| **`phase3_watcher`** | **15** | new |

What the Phase 3 suite proves, beyond "it runs":

- **Overflow (§16 obligation):** injected via the deterministic fake adapter
  — the degradation record is durable immediately, the dirty set survives,
  the watcher marks itself degraded, the *next writer* opens the unknown
  interval naming the reason, `reconcile` closes it and consumes the marker,
  and a second enforcement is a no-op. The watcher alone never gates
  anything.
- **Coalescing without evidence loss:** five duplicate modifies collapse to
  one dirty entry while `events.log` retains all five raw events;
  create+delete leaves the path dirty with both endpoint events preserved.
- **Determinism:** identical inputs produce identical, sorted index
  serialization.
- **Bounds:** the dirty cap degrades (`DIRTY_CAP`) instead of growing.
- **Scope:** events outside the canonical root and under `.rewind` are
  dropped; escapes are counted as anomalies.
- **Crash/restart/offline (§14, §15):** a planted 13-minute-dead watcher
  produces a `WATCHER_GAP` record spanning last-heartbeat→now, which gates
  the next writer and closes only via reconcile; a graceful stop leaves
  everything after it unobserved; a first-ever start records nothing.
- **Isolation:** across a serve loop with events, `metadata.sqlite` is
  byte-identical and the workspace tree is fingerprint-identical — the
  watcher wrote nothing anywhere but its own store directory.
- **Real-adapter lifecycle through the CLI:** detached start reaches
  RUNNING, a real file write lands in the dirty index and the event log,
  `rewind reconcile` runs concurrently with the watcher (no lease
  interference), bounded stop lands STOPPED, a second start is refused
  (exit 3), and a dead watcher reports `FAILED` (exit 3), never `RUNNING`.

## 6. Exception-path record

| Scenario | Observed behaviour |
| --- | --- |
| Adapter overflow | degradation `OVERFLOW` + adapter restart; observation resumes; interval stays unknown until reconcile |
| Unrecoverable adapter failure | state `FAILED`, two degradation records (failure + abandoned restart), loop exits `Failed`; no fabricated health |
| Watcher killed / machine sleep | stale heartbeat ⇒ derived `FAILED`; next start writes the gap marker |
| Graceful stop then external changes | restart writes the gap marker; nothing is pretended observed |
| Dirty index cap | `DIRTY_CAP` degradation once; per-path knowledge stops; memory stays bounded |
| Corrupt degradation marker | read as pending (`AdapterFailed`, "unreadable") — uncertainty reaches the gate, never vanishes |
| Corrupt dirty index | treated as empty (advisory only); raw evidence remains in `events.log` |
| Second `watch start` | refused, exit 3, "already running" |
| `watch stop` on a dead watcher | honest exit 3 with last-heartbeat report; no pid kill anywhere |
| `watch stop` timeout | honest exit 3; the flag stays for the loop's next check |
| Event path outside the root / non-Unicode | dropped + anomaly counter (never followed or expanded) |
| Startup failure of the detached child | `start` reports exit 3 after the bounded 5 s wait with the state-file path |

## 7. Performance measurements (contract §16)

Debug build, x86_64-pc-windows-gnu host, NTFS workspace, default knobs
(batch 250 ms), real adapter via the CLI. Recorded as a baseline only; no
latency SLA is claimed.

| Measurement | Result |
| --- | --- |
| Idle CPU (watcher running, no activity, 10 s window) | **15.6 ms CPU total** (≈0.16 % of one core); one channel receive per batch window, one heartbeat atomic write per 2 s |
| Event → dirty index latency (5 single-file creates) | 113 / 149 / 177 / 193 / 208 ms — bounded by the batch window (the event lands in the next drain) |
| Event log growth | 132 090 bytes for 883 raw events (~150 bytes/event) during a 1000-file create burst (the platform coalesced the remainder) |
| Dirty index size | 8441 bytes at 440 dirty paths (~19 bytes/path), rewritten atomically only when the set changes |

The steady-state loop performs no scans, no hashing, and no CAS work; all
claims above are measured, and the resource-bound behaviours (caps,
rotation) are additionally covered by tests.

## 8. Decision ledger

| # | Decision | Rationale | Evidence |
| --- | --- | --- | --- |
| D1 | `notify` 8.2 as the adapter substrate | it drives the platform-native mechanisms named by the phase objective (inotify/FSEvents/ReadDirectoryChangesW); hand-rolling three FFI watchers would add unsafe surface without changing the semantics the contract keeps explicit (overflow, rename shapes) | contract §5 |
| D2 | **Detached spawn via raw `CreateProcessW` with `bInheritHandles = FALSE` on Windows** | discovered by root-causing a real hang: std always spawns with `bInheritHandles = TRUE` and offers no restriction API (rust-lang/rust#73281, #161158), so a daemon spawned through `std::process::Command` inherits the caller's stdout/stderr pipe handles and any pipe-captured invocation (`$(rewind watch start)`, `Command::output()`) blocks forever on an EOF that never arrives. Verified end-to-end: reproduction hung, fix returns in milliseconds. POSIX needs no equivalent (null stdio `dup2` replaces fds 0/1/2); the residual POSIX >2-fd limitation is documented. A `windows-sys` dependency was added and then removed in favour of the crate's existing `extern "system"` precedent (`reparse_tag` in `src/scan.rs`) — the only `unsafe` in `src/watch/` | reproduction transcript in this session; contract §6 amendment |
| D3 | Degradation markers, not catalog writes, from the watcher | the single-writer discipline (ADR-014) is preserved exactly: the watcher never opens the catalog; the interval is opened by the next lease-holding enforcement point, which makes the immediate-record vs catalog-record split honest (the marker IS durable the moment degradation is detected) | `enforce_watch_degradations`; isolation test |
| D4 | Heartbeat-derived liveness, no pid queries | portable (no FFI for `kill`/`OpenProcess`), conservative (a suspended machine reports `Failed`, and restart gaps gate regardless), and testable; the pid remains in `state.json` for human diagnostics | `lifecycle::heartbeat_is_fresh` |
| D5 | First-ever start records no gap | no coverage was claimed before the first run, so nothing is broken; the existing baseline-drift rules already route pre-watcher changes to reconciliation. Restart after ANY prior run always records the gap — including after a graceful stop, because stopping does not observe what happens afterwards | gap tests |
| D6 | Dirty index cumulative across runs, cleared only by consumers | a restart must not erase evidence; the authoritative scan is what makes "may have changed" meaningless, so `rewind reconcile`/`recover --reconcile` clear it (best-effort, advisory) | `clear_dirty_index`; merge test |
| D7 | Rename handling: `Both`→`Rename{path,from_path}`, `From`→`Delete`, `To`→`Create`; both endpoints dirty | records exactly what the platform said without inventing pairing; the authoritative `diff_manifests` does the real rename pairing during reconciliation | `confine_raw` tests |
| D8 | Raw events appended before coalescing; rotation keeps one generation | coalescing must never erase evidence (§9); the cap is documented and the catalog remains the durable history | event-log tests |
| D9 | `exit 3` for refused/degraded/failed watch states | matches the repository's blocked/needs-operator-action convention (Phase 2 contract §12) | `lifecycle::exit_code` |
| D10 | Enforcement added to the two reconcile-only CLI paths | `reconcile_locked` consumes bypass markers but did not call the enforcement point; without this, a marker consumed *after* a successful reconcile would leave a stale gate that re-gates the workspace spuriously | `Command::Reconcile`, `Command::Recover{reconcile}` |

Two test-side defects and one product defect were found and fixed at the
root rather than worked around: the hang (D2), synthetic event paths built
from the non-canonical tempdir root (correctly confined away by the watcher
— the tests were wrong, the product was right), and a hand-computed civil-
date test constant.

## 9. CI record for Phase 3

- **Run #36110312682** (commit `0e62dae`, first pushed Phase 3 head):
  ubuntu and macOS **failed at Compile**; windows failed at Tests.
  The POSIX compile failure was the Phase 1.2 trap verbatim: the detached
  spawn's `process_group(0)` call needs the
  `std::os::unix::process::CommandExt` trait in scope, which a
  Windows-host compiler never sees. Fixed with a scoped import and a
  comment citing that precedent (`eed94e5`).
- **Run #36111212151** (`eed94e5`): ubuntu and macOS **success**; windows
  failed at Tests — `overflow_records_a_degradation_keeps_evidence_and_
  never_gates_by_itself` expected the post-overflow event in the dirty
  index but saw only the pre-overflow one. Root cause: a race in the
  **test**, not the product — three fake-adapter tests requested the stop
  as soon as the FIRST observable appeared (the degradation record or the
  first dirty entry), legitimately racing the loop and cutting the
  remaining scripted events off. Locally the loop always won; on a loaded
  runner the test won. The behavior is correct per contract (the
  degradation record precedes the remaining events; stop is honored
  immediately). The tests now wait for the strongest condition (all
  scripted events on the log / both endpoints in the index) before
  stopping (`d69fdca`). Windows robustness was additionally hardened in
  the same commit: the detached-child startup deadline rose from 5 s to
  15 s (fresh-binary first execution under real-time AV on runners) and
  the four CLI/daemon-spawning tests serialize on a shared mutex.
- **Run #36111890265** (`d69fdca`): ubuntu-latest 1m37s, macos-latest
  1m42s, windows-latest 5m37s — **all success, zero non-success check
  runs on the commit**. This is the CI evidence the verdict rests on.

---

## Verdict

**Local gates (Windows host, x86_64-pc-windows-gnu, Rust 1.98.1):**

- `cargo fmt --all -- --check` — PASS
- `cargo check --all-targets` — PASS
- `cargo clippy --all-targets -- -D warnings` — PASS
- `cargo test --all-targets` — **119 passed / 0 failed** (48 Phase 1 + 15
  Phase 2 unchanged; 43 new Phase 3), plus the phase3 suite re-run five
  consecutive times green after the race fix.

**CI: run #36111890265 on `d69fdca` passed on ubuntu-latest, macos-latest
and windows-latest with zero non-success check runs.** This is the
evidence the verdict rests on.

> **PHASE 3 VERIFIED.** The watcher is advisory-only by construction,
> every overflow/crash/offline obligation is covered by a test that
> actually exercises it (fake-adapter injection plus real-filesystem
> lifecycle), no Phase 1/2 invariant or suite regressed, and both defects
> surfaced during verification were root-caused and fixed with evidence.

Known limitations (all documented in the contract): no command attribution
from events, ever; the event log is capped evidence, not durable history;
POSIX descriptors above 2 are not closed in the detached child; a suspended
machine's watcher reports `FAILED` (conservative); liveness is
heartbeat-derived; the dirty index is advisory and cleared by consumer
actions, not by the watcher.

## 10. Risk register

| Risk | Owner | Severity | Trigger | Mitigation |
| --- | --- | --- | --- | --- |
| CI fails on macOS/Linux for Phase 3 | repo owner | medium | first push | adapter and loop are platform-neutral; the only platform code is the Windows spawn and the POSIX `process_group`; the POSIX fd>2 limitation is documented, not load-bearing |
| A watcher artifact is later trusted as state | future contributor | high | new code reads `dirty.json` as truth | contract §17 names it a failure; the isolation test pins the boundary; all artifacts live outside the catalog |
| Enforcement forgets a new entry point | future contributor | medium | a new mutation command | the gate lives in `enforce_pending_safety_gate`, which every writer path already calls; reconcile-only paths are named in the contract §12 |
| Daemon leak on test failure | repo owner | low | an assertion fires before `watch stop` | watchers hold no lease and self-terminate when their workspace disappears (adapter errors → `FAILED` exit); `stop` is idempotent |
| std gains handle-restriction (rust#73281) | n/a | info | future toolchain | the raw spawn can then be replaced by std; the module is isolated in `detach_windows.rs` |

## 11. Handoff pack

- **Build:** `cargo build` (Rust stable; `rusqlite` bundled; new dependency:
  `notify 8.2` only).
- **Test:** `cargo test --all-targets` (expect 119 passed; bash on PATH for
  the shell-integration suite).
- **New commands:** `rewind watch start [--foreground] [--batch-ms N]`,
  `rewind watch status [--json]`, `rewind watch stop`; internal:
  `rewind watch serve --root <root>` (hidden).
- **Knobs:** `REWIND_WATCH_BATCH_MS` (50..5000, default 250),
  `REWIND_WATCH_DIRTY_CAP` (default 100 000), `REWIND_WATCH_LOG_BYTES`
  (default 8 MiB); read once at serve start and recorded in `state.json`.
- **Credentials/monitoring:** none; the watcher writes no telemetry and
  opens no network resources.
- **Rollback of the phase:** revert commits `77197a3..HEAD` as one unit;
  Phase 3 is additive (one call site in `workspace.rs`, one command arm in
  `cli.rs`, one module) — the Phase 2 report §11 runbook applies unchanged.
