# Phase 1.3 — Passive Hook Isolation & Final Foundation Report

Scope: the Phase 1.3 charter only — make the passive shell hook genuinely
shell-nonblocking, remove the false 150 ms promise, prove the real user
experience with shell-level tests, and perform the final Phase 1
verification pass. The Phase 0.7 architecture, state/degradation/recovery
semantics, and the rollback engine are unchanged. No Phase 2 functionality
was introduced. All previous verification reports are preserved unchanged.

**Verdict: PHASE 1 CONDITIONALLY VERIFIED** (see §10; the conditions are
the same documented, environment-bound limitations as Phase 1.2 — no
open P0/P1).

## 1. The passive hook problem

The Phase 1.2 implementation bounded bookkeeping with an in-process 150 ms
budget plus a 50 ms scan deadline, but the shell integrations
(`integration/rewind.bash`, `integration/rewind.zsh`) invoked
`rewind hook post` **synchronously** from `PROMPT_COMMAND`/`precmd`. The
budget checks covered only the code path up to `hook_post_locked()`; the
scan, CAS writes, and SQLite persistence inside the locked section were
not individually bounded. The architectural goal — *passive observation
must never impose an unbounded interactive-shell wait* — was therefore
not proven: the shell paid for every millisecond of bookkeeping on every
prompt.

## 2. Root cause

Synchronous invocation of expensive bookkeeping from the shell's prompt
hook. A timer inside the callee cannot make a synchronous call
nonblocking; it can only fail the call faster. The correct fix is
structural, not temporal.

## 3. Shell integration change

Both wrappers now launch the post-hook bookkeeping in the background:

```bash
nohup rewind hook post --exit-code "$status" --session "$id" </dev/null >/dev/null 2>&1 &
disown 2>/dev/null || true   # bash; zsh relies on nohup vs its HUP-on-exit
```

- The shell returns to the prompt immediately after spawning; it never
  waits for scan, CAS, SQLite, recovery, or archive work.
- The spawn is fail-open twice over: `command -v rewind` guards
  availability, and the background child's failure cannot change the
  user's command exit status (it is not waited on).
- The pre-hook stays synchronous and extremely lightweight: marker
  discovery, catalog open, one boundary INSERT. No scan, recovery, CAS,
  or archive work. A failed pre-hook is represented safely by absence —
  no boundary exists, so no observation can be fabricated, and the next
  writer's reconcile-first pre-scan catches live drift.

## 4. Failure/degradation semantics (background work)

With the hook in the background, failure handling is conservative and
unchanged in kind:

- Background hook completes → observation recorded (or BoundaryOnly when
  the workspace is gated) as before.
- Hook cannot get the writer lease → brief bounded retry
  (`HOOK_LEASE_RETRY`, 2 s) so overlapping hooks from rapid typing do not
  spuriously gate; if the lease stays held (e.g., a writer mid-rollback),
  a durable bypass marker is appended and the next writer reconciles.
- Scan cannot finish inside the scan deadline (50 ms, measured from the
  start of the scan — see §6) → durable CAPTURE_FAILED + unknown
  interval + RECONCILIATION_REQUIRED. Never a fabricated observation.
- Any other hook error (CAS, storage, catalog) → durable bypass marker
  (best-effort) before the error propagates; the CLI wrapper prints the
  diagnostic and exits 0.
- Hook process terminated or shell closed mid-bookkeeping → at worst an
  unconsumed boundary; no observation exists, nothing is attributed, and
  the next writer's reconcile-first pre-scan detects any live drift.
  Verified by test (§5).
- Writer recovery stays out of the hook: `recover_locked` appears only in
  writer commands (`run`, `undo`, `redo`, `restore`, `recover`,
  `snapshot`); an unfinished transaction makes the hook record a bypass
  marker and return.

The false 150 ms promise is removed; see §6.

## 5. Tests added (`tests/shell_integration.rs`, 6 tests)

- **`bash_wrapper_returns_control_before_bookkeeping_finishes`** — a stub
  `rewind` whose post-hook sleeps 45 s is put first on PATH; an
  interactive bash (DEBUG trap + PROMPT_COMMAND, the real integration
  file) runs a command and exits. Assertions: the shell exits **while the
  post-hook is still running** (post sentinel absent at shell exit,
  present afterwards). This ordering is impossible for a synchronous
  wrapper at any speed — a threshold-free proof. Also asserts the
  synchronous pre-hook ran and the command's status is unaffected.
- **`zsh_wrapper_returns_control_before_bookkeeping_finishes`** — same
  proof for zsh (`add-zsh-hook` preexec/precmd). Runs on runners that
  ship zsh (macOS CI); availability is probed and absence is reported
  honestly with a skip note (zsh is not part of Git for Windows, so
  windows-latest reports the skip).
- **`wrapper_observation_flow_and_future_strong_operations`** — the real
  binary through the real wrapper: observation recorded (or, under heavy
  load, the conservative degradation lands — §7 of the charter explicitly
  accepts "observation or failure"), never a fabricated STRONG
  operation, HEALTHY afterwards, and a following `rewind run` captures a
  strong operation normally.
- **`busy_catalog_makes_passive_bookkeeping_fail_open`** — an external
  process holds the catalog's SQLite write lock: the pre-hook fails open
  with a diagnostic, the user's command is unaffected, nothing is
  fabricated, and the next writer reconciles the unobserved drift and
  captures.
- **`terminated_background_hook_never_becomes_trusted`** — a real 1500-file
  workspace; the background post-hook process is killed mid-flight; no
  observation may appear; an external mutation on top must be reconciled
  by the next writer, which then captures normally.
- **`post_hook_fails_open_when_workspace_disappears`** — the workspace is
  deleted between pre and post; the hook fails open with a diagnostic and
  exits 0.
- **`unwritable_cas_degrades_to_capture_gap`** (POSIX) — CAS made
  unwritable; the scan fails into CAPTURE_FAILED + unknown interval +
  RECONCILIATION_REQUIRED; reconciliation restores HEALTHY with the live
  bytes intact.

The Phase 1.2 hardening test that asserted "< 5 s" was reframed
(`passive_scan_deadline_degrades_to_capture_gap`): it now verifies the
degradation semantics only, with no wall-clock claim, because the
shell-facing guarantee is a property of the integration (proved by the
stub tests above), not of the hook process.

Suite total: **36 integration tests** (13 foundation + 9 rollback_tree +
8 hardening + 6 shell_integration), all green locally and in CI.

## 6. The false 150 ms guarantee — removed

`HOOK_BUDGET_MS` (150 ms "total budget") is deleted from `src/cli.rs`;
the audit confirms no reference remains. The documented guarantees are
now exactly:

- **Shell-facing**: the integration does not synchronously wait for
  Rewind bookkeeping (proved structurally by the stub ordering tests).
- **Bookkeeping scan budget**: the scan is bounded by
  `HOOK_SCAN_DEADLINE_MS` = 50 ms, now measured from the moment the scan
  begins (workspace open and lease work no longer consume it — a defect
  found by this phase's testing: under load, a slow catalog open could
  exhaust a process-start-anchored deadline and produce spurious
  CAPTURE_FAILED records).
- **Background work**: may take longer; it cannot block the shell and
  fails into the existing degradation model. `HOOK_LEASE_RETRY` (2 s) is
  a background-only retry before the bypass fallback — not a latency SLA.

No new performance SLA was invented.

## 7. Local gate results (Rust 1.98.1, x86_64-pc-windows-gnu, NTFS)

- `cargo fmt --all -- --check`: PASS
- `cargo check --all-targets --all-features`: PASS
- `cargo clippy --all-targets --all-features -- -D warnings`: PASS
- `cargo test --all-targets --all-features`: 36 passed, 0 failed
  (full suite run repeatedly; the suite is stable under its own parallel
  load after the §6 deadline fix).

## 8. CI results (GitHub Actions, OG-Huzzi/Rewind)

Final run for commit badf17d (Phase 1.3 code complete): **ubuntu-latest,
macos-latest, windows-latest (MSVC) — all success** (fmt, check, clippy
-D warnings, full 36-test suite per platform). Zsh ran on macOS; zsh's
absence on windows-latest is reported by the test itself via an honest
skip note.

CI again did real verification work on the way there (same pattern as
Phase 1.2 — POSIX behavior is invisible from the Windows host):

- 2afbb7b: windows ✓; ubuntu/macos ✗ — two genuine POSIX-only test
  defects surfaced: (1) the zsh integration registered its hooks through
  `command -v add-zsh-hook` *before* autoloading it, but an autoloadable
  function is invisible to `command -v`, so the hooks silently never
  registered on macOS (the test's missing pre-sentinel caught it); (2)
  the unwritable-CAS test locked the wrong directory (`project_root/cas`
  instead of the store-root CAS).
- c71642e: windows ✓; ubuntu/macos ✗ — the CAS test locked `cas` itself,
  but blob staging happens in `cas/tmp`, whose own permissions still
  allowed writes; the scan legitimately succeeded.
- badf17d: all three platforms green.

## 9. 38-scenario matrix — affected scenarios re-verified

Re-verified by execution this phase (not carried over automatically):
T01 (scan-deadline degradation: hardening + shell tests under real load),
T02 (gated passive boundary → BoundaryOnly), T03 (gap → reconcile →
HEALTHY), T05 (writer reconcile-first pre-state == checkpoint;
busy-catalog test), T23 (second writer serializes; overlapping hooks
serialize on the lease with bounded retry), T24 (hook during writer:
deferred + bypass; writer mid-rollback still gates after retry), T38
(unknown physical state → RECOVERY_REQUIRED → reconcile exit path).
T25/T26/T27/T30 keep their honest PARTIAL statuses (race/share scenarios
not live-fault-injected). Unavailable scenarios (T12/T13/T29 symlink
privilege on some Windows accounts — TESTED AND PASSED on POSIX CI;
T32 NTFS case-collision) keep their honest statuses.

Tally stands at **26 TESTED AND PASSED, 6 PARTIAL, 1 PLATFORM
UNAVAILABLE, 2 NOT TESTED, 3 NOT APPLICABLE — zero FAILED**.

## 10. Final recommendation

Phase 1 is complete to the limit of what this environment can prove:
shell-facing passive observation is genuinely nonblocking (structural +
behavioral proof), background failures preserve the conservative safety
model, all correctness suites are green, CI is green on all three
platforms including MSVC, the README is valid UTF-8, the architecture is
unchanged, and no Phase 2 functionality was introduced. No P0/P1 is open.

Remaining conditions (documented, unchanged from Phase 1.2): power-loss
durability is not assertable in CI (T34/T35); five race/sharing scenarios
remain PARTIAL (T25/T26/T27/T30/T36) because live fault injection is not
available; T28/T37 remain untested scope; T32 needs a case-colliding NTFS
environment; Windows symlink creation is capability-gated (graceful skip
asserted); rollback performance is measured, not redesigned (Phase 2
owns the redesign).

**PHASE 1 CONDITIONALLY VERIFIED.** Phase 2 may begin when the project
owner accepts these documented conditions; the foundation itself is
honest about every interval it did not observe.
