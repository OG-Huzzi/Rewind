# Phase 5 Close-Out / CI-Stabilization Contract

Status: contract for this phase. Root causes below were established by
reading the failing tests and the hook implementation, by a probe of the
shell integration's prompt mechanics, and by the recorded CI failures
(runs #52–#55 and the `b188e06`/`343ae03` record runs) before this
contract was written.

Baseline: `main` at `343ae03` (Phase 5 slice 4 CI-verified; run #55 green
on ubuntu/macOS/Windows).

## 1. Scope

This phase closes Phase 5. It contains exactly three kinds of work:

1. **CI stabilization** — deterministic rework of the two hook-timing test
   suites that flaked during Phase 5 (evidence in §2), plus failure
   diagnostics for the third observed flake. No assertion may be weakened,
   removed, or made conditional on load; every identity and safety
   assertion stays exactly as it is.
2. **Phase 5 signoff** — a verification report stating the final
   object×platform capability state (supported / refused / deferred, with
   the recorded blockers) and the per-slice CI evidence.
3. **Stale-document reconciliation** — the remaining Phase 1 TODO items
   that CI has long since superseded, and the roadmap position.

Explicitly out of scope: new capability slices (including POSIX xattrs —
still blocked on a probe host), scanner sharing-violation retries (no
reproduced failure; the third flake gets diagnostics only), interactive
UI, and any journal-durability performance work (ADR-018 deliberately
untouched).

## 2. Root causes (evidence, not speculation)

- **`boundary_correlation::overlapping_background_posts_keep_identity`**
  (failed once on windows-latest, run #55; green on unchanged re-run):
  the post-hook records `hook_post`'s claim (`finish_boundary`) *before*
  waiting for the lease, and falls back to a durable bypass marker when
  `HOOK_LEASE_RETRY` (2 s) expires. In the failed run, hook C's boundary
  was claimed (the `row.consumed` assertions precede the operation
  lookup and passed) and no operation existed: hook C starved — hooks A
  and B serialized their fsync-heavy gated bookkeeping (SQLite
  `synchronous = FULL`) ahead of it on a slow runner, exhausting C's
  2 s budget. Exit 0 + bypass marker + no operation is exactly the
  observed panic (`no operation for COMMAND_C` at the operation lookup,
  before the bypass assertion). The test races a production timing
  constant; the fallback itself is correct and separately tested
  (`post_during_writer_activity_gates_durably_and_leaves_others_pending`).
- **`shell_integration::bash_rapid_commands_keep_command_identity`**
  (failed once on ubuntu-latest on a doc-only commit; green on unchanged
  re-run): the background post-hook is spawned by `_rewind_precmd`
  (PROMPT_COMMAND), which fires when the interactive shell returns to the
  prompt. The three fed commands and the harness's trailing `exit 0` are
  written to the shell's stdin as one buffered block, and a probe of
  buffered interactive bash showed **zero** PROMPT_COMMAND cycles for
  fully buffered input: whether the cycle that spawns the *last* real
  command's post-hook happens before `exit` is consumed is
  environment-dependent. In the failed run, c.txt's boundary was never
  claimed (`consumed == false` after the full 75 s poll) — its post-hook
  was never spawned, while a.txt and b.txt (whose spawn cycles precede a
  *subsequent command*) were claimed in every recorded run. The last
  real command's spawn is the only one without a guaranteed following
  command.
- **`rollback_tree::undo_of_nested_tree_with_many_files`** (failed once
  under full local parallel load; green 3/3 isolated and in every CI
  run): `capture_tree_creation` asserts `outcome.captured`, whose `false`
  collapses two very different situations — a capture-failure (post-scan
  error, e.g. a transient Windows file lock; the outcome carries
  `capture_error`) and an unchanged pre/post state. The assert message
  loses the cause. No scanner change is made without a reproduced,
  diagnosed failure; this phase adds the diagnostics only.

## 3. Changes

### 3.1 `REWIND_HOOK_LEASE_RETRY_MS` (the only production-code change)

`HOOK_LEASE_RETRY` becomes overridable per process via
`REWIND_HOOK_LEASE_RETRY_MS` (milliseconds). Default unchanged (2000 ms);
values outside a documented sane range fall back to the default; the
fallback behavior itself is untouched — when the budget is genuinely
exhausted, the durable bypass marker is still the outcome, and its
dedicated test still runs against the default. Rationale: the constant is
a starvation-avoidance tuning knob for environments with slower disks,
not a wall-time promise (the Phase 1.3 rule that no constant promises the
hook's total wall time is untouched); the watcher subsystem already has
the same env-tuning pattern. The overlap test sets a generous value so
its identity assertions no longer depend on disk speed.

### 3.2 Deterministic overlap barrier

`overlapping_background_posts_keep_identity` replaces its fixed 500 ms
sleep with a claim barrier: hold the writer lease, poll the catalog until
all three spawned hooks have claimed their boundaries (`finish_boundary`
runs *before* the lease wait, so a claimed boundary proves the hook is
real, running, and blocked at the lease), then drop the lease. Overlap
becomes a verified fact instead of a timing assumption; the assertions
are unchanged.

### 3.3 Guaranteed post-hook spawn in the rapid-commands test

The rapid test feeds a trailing sentinel command after the last real
command, so the last real command's post-hook spawn gets the same
guaranteed prompt cycle every other command's spawn has (the sentinel's
own boundary joins the tolerated extras, alongside the harness's `exit`
line — both carry only their own command text and can never be mistaken
for an expected identity). The shell environment sets
`REWIND_HOOK_LEASE_RETRY_MS` so in-flight posts under heavy parallel load
keep waiting instead of bypassing.

### 3.4 Diagnostics for the capture flag

`capture_tree_creation`'s assert message gains `capture_error` and the
command's exit code, so a recurrence is diagnosable from the CI log
alone.

## 4. Acceptance criteria

- **AC1:** the overlap test passes 3 consecutive local runs with the
  claim barrier in place (it was already green locally; the bar is that
  the barrier holds under repetition).
- **AC2:** the rapid test passes 3 consecutive local runs (bash; zsh
  where available — it is not, locally).
- **AC3:** the full suite passes twice consecutively locally, and fmt /
  check / clippy `-D warnings` stay green.
- **AC4:** `.ai/PHASE_5_VERIFICATION_REPORT.md` exists and states, per
  slice: the delivered capability, its contract/ADR, its tests, and its
  CI runs — plus the final capability matrix state (supported / refused /
  deferred, with blockers) and what was deliberately left deferred.
- **AC5:** the stale Phase 1 TODO items are reconciled against what CI
  actually covers; ROADMAP/CURRENT_STATE/HANDOFF/TEST_STATUS/AGENT_LOG/
  CHANGELOG reflect the close-out.
- **AC6:** CI is green on ubuntu/macOS/windows for the phase commit; any
  timing flake is handled by the recorded re-run protocol and reported
  honestly, never silently.
