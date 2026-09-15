# Phase 1.4 — Passive Boundary Correlation & Final Phase 1 Verification

Scope: the Phase 1.4 charter only — make the passive-boundary identity explicit
and immutable across the pre/post hook pair, remove the asynchronous
cross-consumption defect introduced by the Phase 1.3 nonblocking architecture,
add deterministic concurrency regression coverage, and perform the final Phase 1
verification pass. No Phase 2 functionality was introduced. All historical
reports are preserved unchanged (the Phase 1.3 report carries a clearly marked
addendum pointing here).

**Verdict: PHASE 1 NOT VERIFIED.** Boundary correlation is explicit and
deterministic and the full local gate is green, but CI run #17 (commit
`b69817d`) failed on macOS and Windows. The Windows failures are addressed in
the working tree; the macOS failure is not, and its nature is not yet
determined. See §11 and §13. (An earlier revision of this file claimed a green
CI on all three platforms and "PHASE 1 VERIFIED"; that claim was not supported
by the recorded run and is corrected here — see §15.)

## 1. The defect

Phase 1.3 correctly made post-hook bookkeeping nonblocking
(`nohup rewind hook post ... &`), but left the correlation between a
background post-hook and its boundary implicit. The post-hook asked the
catalog:

```sql
SELECT ... FROM passive_boundaries
WHERE workspace_id=? AND session_id=? AND consumed=0
ORDER BY started_at DESC LIMIT 1
```

That lookup is only sound when at most one boundary of a session can be
unconsumed at a time. Asynchronous hooks break that assumption: several
background posts can coexist, so the newest unconsumed boundary is not
necessarily *this* post's boundary. The observed failure shape was:

```text
pre(command A) -> create boundary A
pre(command B) -> create boundary B
post(A)        -> consumes boundary B   (newest unconsumed)
post(B)        -> consumes boundary A   (newest remaining)
```

Both hooks "succeed", so nothing looks broken, but command, exit code, cwd,
observation payload, effects, and the refreshed trusted checkpoint are all
attributed to the wrong shell command. Because a complete passive scan may
refresh the trusted current checkpoint (Phase 1 contract), this is a
correctness defect, not merely metadata quality. It is a P1 blocker: it can
pair one command's exit status with another command's filesystem delta.

A second, latent defect was found while auditing the same path: the
integration computed the session id inside a command substitution
(`--session "$(_rewind_session_id)"`), so the assignment only ever happened in
the subshell. The parent shell never kept the id, which meant the recorded
session id drifted with the wall clock. Unreachable-then, live-now: with the
recency lookup gone, a session id is provenance only — but it is now correct.

## 2. Root cause

The boundary identity was stored (a UUID primary key existed) but never
*used* as identity. The post-hook had no way to name its boundary, so it
recovered one heuristically from (workspace, session, unconsumed, newest).
Any heuristic of that shape is unsound under concurrency, because the
information needed to disambiguate (which boundary belongs to this post) is
simply absent from the call.

## 3. Exact architectural fix

The boundary id becomes an explicit, immutable token travelling from the
pre-hook to the post-hook through the shell:

```text
pre(command A)   -> creates boundary A, prints id A on stdout
shell            -> captures id A for exactly this command
post(A, exit 17) -> claims exactly A, exactly once
pre(command B)   -> creates boundary B, prints id B
shell            -> captures id B
post(B, exit 23) -> claims exactly B, exactly once
```

No newest-boundary lookup, no oldest-boundary lookup, no timestamp heuristic,
no best-effort matching, and no session-only correlation exist anywhere in the
codebase after this phase (`pending_boundary` is deleted).

Ordering of the two hook phases is unchanged from Phase 1.3:

- the pre-hook stays synchronous and lightweight: marker discovery, catalog
  open, one boundary INSERT, plus printing the id;
- the post-hook stays asynchronous (`nohup ... &`, detached stdio), and the
  interactive shell never waits for it;
- the hook bounds are unchanged: `HOOK_BUDGET_MS` stays deleted, no wall-time
  SLA exists, `HOOK_SCAN_DEADLINE_MS` (50 ms, measured from the start of the
  scan) remains the only in-process bound, the 2 s lease retry stays
  background-only, and `recover_locked` still never runs in the hook path.

### 3.1 Claim-before-work

`rewind hook post --boundary <id> --exit-code <status>` now does exactly this,
in this order:

1. resolve the workspace (failure → diagnostic, exit 0, nothing touched);
2. fetch that one boundary, scoped to the workspace
   (`workspace_id`, `id`) — `None` means unknown id or another workspace →
   diagnostic, exit 0, **no side effects at all**;
3. claim it atomically (`consumed = 0 -> 1`, recording `exit_code`/`ended_at`).
   `false` means another post already accounted for it → diagnostic, exit 0,
   no side effects. The claim is the ownership transfer: precisely one post
   can ever act on an interval;
4. only then take the writer lease, refuse to run deferred recovery, enforce
   the safety gate, and either record a passive observation, a `BoundaryOnly`
   gate record, or a durable bypass marker / `CAPTURE_FAILED` gap.

Because the claim comes first, every outcome path accounts for the claimed
boundary exactly once — success, gate, deferred recovery, terminated process
after the claim, and CAS/scan failure. Because it is a guarded single-statement
`UPDATE`, it is atomic even when several background hooks race for the same id.

### 3.2 Scan-deadline start made exact (found during CI verification)

The Phase 1.3 contract says the 50 ms scan deadline "starts when the scan is
about to run". The implementation actually started it *before* the deferred-
recovery check, the safety-gate read, and the baseline read — so under load
those catalog/file operations silently consumed part of the scan's budget.
Phase 1.4 moves the deadline start to immediately before `workspace.scan(..)`
inside `hook_post_locked`, making the code match the documented contract
exactly. `HOOK_BUDGET_MS` stays deleted, no total wall-time SLA exists, and a
scan that still cannot finish records the same durable capture gap as before.

## 4. Database and API changes

The ambiguous API was replaced rather than patched at the caller
(`src/db.rs`):

| Before | After |
| --- | --- |
| `pending_boundary(workspace, session)` -> newest unconsumed | **deleted** |
| `boundary(id)` | `boundary(workspace_id, id) -> Option<BoundaryRow>` |
| `finish_boundary(id, exit_code)` (unconditional UPDATE) | `finish_boundary(workspace_id, id, exit_code) -> bool` with `AND consumed=0`, reporting whether this caller won the claim |
| — | `boundaries(workspace_id)` (creation order, for diagnostics/tests) |
| `BoundaryRow { id, session_id, command, cwd, started_at }` | adds `ended_at`, `exit_code`, `consumed` |

Invariants are now enforced in more than one place:

- **Workspace scoping** is a SQL predicate on both fetch and claim; a boundary
  id minted by another workspace simply does not exist here.
- **Single accounting** is a guarded `UPDATE` plus SQLite's write
  serialization: two racing processes cannot both see `consumed = 0`.
- **Immutability** is database-enforced for every catalog, including ones
  created before this phase, by a trigger installed with
  `CREATE TRIGGER IF NOT EXISTS` in `Catalog::initialize`, which aborts any
  UPDATE to an already-accounted-for boundary. A test exercises this from raw
  SQL, bypassing the typed API.
- A `CHECK (consumed IN (0, 1))` constraint is added to new catalogs.

The `id` remains a v4 UUID: unique, unguessable, and independent of filesystem
timestamps, command text, cwd, process scheduling, and session ordering.

## 5. Shell integration changes

The fix touches `integration/rewind.bash` and `integration/rewind.zsh`, and
both shells keep every Phase 1.3 property (async post-hook, detached stdio,
no exit-status interference, Bash on Windows/POSIX, Zsh where installed).

- The pre-hook prints the new boundary id on stdout. The shell captures it
  via command substitution into `_REWIND_BOUNDARY_ID` for exactly the command
  that is about to run. Diagnostics remain on stderr (`REWIND_HOOK_VERBOSE`
  unchanged), so they can never contaminate the captured id.
- The post-hook invocation passes `--boundary <id>` along with `--exit-code`.
- The post-hook arguments never include `--session` anymore (identity no
  longer comes from the session).
- The shell consumes (clears) the stored id the moment the post-hook is
  spawned, so it can never leak into a later command.
- `_REWIND_LAST_COMMAND` is gone; the boundary id itself is the pending-post
  marker. The stable session id is now materialized once in the shell at
  source time, so the recorded provenance is stable and shared by the whole
  session (it used to be recomputed in a subshell, where the assignment could
  not persist).

The pre-hook stays synchronous and lightweight. `PROMPT_COMMAND`/`precmd`
invocation structure is unchanged, including appending to (not replacing) a
user's existing `PROMPT_COMMAND`.

## 6. Concurrency model (how pre/post correlation now works)

The boundary id is immutable across the pre/post pair:

1. The shell's pre-command hook calls `rewind hook pre`, which creates the
   durable boundary row and returns its unique id on stdout.
2. The shell stores that id in a shell-local variable scoped to exactly the
   command whose post-hook will later complete (cleared when consumed).
3. The post-hook runs asynchronously with `nohup ... &`, receives the id as
   `--boundary`, queries exactly that boundary, and claims it atomically:
   `UPDATE ... WHERE id=? AND workspace_id=? AND consumed=0`.
4. If the id does not exist, is already consumed, belongs to another
   workspace, or cannot be safely verified, the post-hook fails open: it
   guesses nothing, consumes nothing else, fabricates nothing, and preserves
   the conservative reconciliation semantics.

No newest/oldest/timestamp/session heuristic exists anywhere. The database API
requires the identity explicitly (§4): `boundary(workspace_id, id)` and
`finish_boundary(workspace_id, id, exit_code)`, with `pending_boundary` deleted.

Out-of-order background completion: checkpoint progression still happens only
from scans taken under the writer lease, in lease order, so an older background
observation that finishes later can never overwrite newer trusted state with an
older one (§9, §13).

## 7. Tests

46 integration tests total (13 foundation + 9 rollback_tree + 8 hardening +
6 carried-over shell-integration + 10 new boundary-correlation). The new and
changed coverage:

- `tests/boundary_correlation.rs` (10 deterministic tests, no stubbing of the
  binary, no timing dependence):
  - older/never-consume-newer ordering (`A` post first while `B` is pending)
    with distinctive command + exit metadata, each boundary asserted consumed
    exactly once with its own identity;
  - genuine three-way overlap via three real `hook post` child processes
    synchronized behind a held writer lease (order-independent assertions);
  - unknown id, duplicate post, and cross-workspace id: exit 0, diagnostics,
    no fabrication, no unrelated consumption, exit-code records unchanged;
  - corrupted-state semantics preserved: gated workspaces yield one
    `BoundaryOnly` per boundary rather than the retired ambiguous post;
  - post-during-writer activity leaves the bypass marker, reconciles, and
    completes the pending boundary afterwards without cross-consumption;
  - second-order race audit scenarios: two sessions in one workspace, no
    double attribution, per-boundary `consumed`/`exit_code` accounting.

## 8. Concurrency tests

The deterministic regression core (A7 of the charter) is
`older_boundary_post_never_consumes_a_newer_boundary`: boundaries A then B
are created with distinctive metadata (`COMMAND_A`/17, `COMMAND_B`/23), then
`post(A)` runs while B is still pending — the exact order the retired lookup
got wrong — followed by `post(B)`. The catalog must show A consumed with
`COMMAND_A`/17 and B consumed with `COMMAND_B`/23, each exactly once, with no
third boundary fabricated and none left unexpectedly pending.

Overlap is covered by `overlapping_background_posts_keep_identity`: three real
post processes are spawned behind a held writer lease and then released, which
forces genuine overlap without any timing-dependent assertion (the assertions
are order-independent).

Failure-model cases are covered as A9 requires: unknown id, duplicated post,
overlapped A/B, post-after-pre-failure (consumed exactly once + conservative
gap), and rapid shell commands.

## 9. Safety reasoning

- Failed capture never becomes trusted: every degraded path (unknown/gated
  hook, bypass marker, scan-deadline miss, CAS failure) still records only
  `BoundaryOnly`, `CaptureFailed`, bypass markers, or open unknown intervals —
  never a fabricated operation, never a trusted-baseline advance from
  incomplete work.
- Incomplete work never becomes a fabricated operation: unknown or duplicate
  ids exit 0 with no catalog writes at all.
- Unknown intervals remain unknown; degraded/reconciliation states remain
  conservative; bypass markers remain durable where required; writer
  reconcile-first semantics remain intact.
- Baseline-state progression is deterministic and non-regressive: checkpoint
  updates still happen only from scans taken under the writer lease, so
  out-of-order background completion cannot overwrite newer trusted state with
  an older observation. (See A13 of the charter and the chain-order tests.)

## 10. Test results

Local gate (Rust 1.98.1, x86_64-pc-windows-gnu, NTFS; Git Bash present, zsh
absent):

- `cargo fmt --all -- --check`: PASS
- `cargo check --all-targets --all-features`: PASS
- `cargo clippy --all-targets --all-features -- -D warnings`: PASS
- `cargo test --all-targets --all-features`: **48 passed, 0 failed**
  (10 boundary-correlation [new] + 13 foundation + 9 rollback_tree +
  8 hardening + 8 shell-integration; the 2 zsh tests exercise the real
  `rewind.zsh` on runners that ship zsh and report an honest skip on
  Windows, where this machine reports the skip for both)
- The full suite was run repeatedly, including the concurrency-sensitive
  tests and the shell integration tests; every repeated run was green.
- Final local gate before commit (all on `x86_64-pc-windows-gnu`, full
  `--all-targets --all-features`, bash present via Git for Windows, zsh
  honestly skipped): the four commands above, plus a dedicated 6x loop of
  `tests/boundary_correlation.rs` (60/60 green), a 4-thread high-contention
  run, and per-test repeats of the shell integration tests.
- Degraded-branch check: with `HOOK_SCAN_DEADLINE_MS` temporarily set to 0
  (forcing every healthy-workspace post-hook into the documented conservative
  degradation), all 10 correlation tests still pass — proving the tests assert
  identity in *both* branches rather than assuming the observation branch
  always lands.
- Teeth check: with the fix temporarily replaced by a simulation of the
  retired newest-pending lookup, `older_boundary_post_never_consumes_a_newer_
  boundary` FAILS (confirming the test discriminates the defect); with the
  fix restored, it passes.
- Independent re-run by AutoCoder on 2026-09-15, on the working tree that
  carries the §3.2 change (x86_64-pc-windows-gnu): `fmt`/`check`/`clippy` PASS,
  full suite **48 passed / 0 failed**, `tests/boundary_correlation.rs` 10/10 on
  six consecutive runs plus one `--test-threads=4` run, and
  `tests/shell_integration.rs` 8/8 on two runs. These are Windows-only results;
  they do not substitute for the macOS job.

## 11. CI results

CI runs the unchanged workflow (fmt, check, clippy -D warnings, full test
suite) on ubuntu-latest, macos-latest, and windows-latest (MSVC).

The Phase 1.4 commit `b69817d` was pushed to `OG-Huzzi/Rewind` and ran as
**run #17**. Its result, read directly from the GitHub Actions API (not from
local logs):

| Job | Result | Failing step | Failing tests |
| --- | --- | --- | --- |
| ubuntu-latest (stable) | success | — | — |
| macos-latest (stable) | **failure** | Tests (exit 101) | `bash_rapid_commands_keep_command_identity` — `tests/shell_integration.rs:1044`, "a missing observation must leave a durable conservative trace" |
| windows-latest (stable) | **failure** | Tests (exit 101) | `multiple_sessions_keep_their_own_observation_provenance` (`tests/boundary_correlation.rs:583`, `CaptureFailed` vs `PassiveObservation`) and `observation_chain_advances_the_checkpoint_in_completion_order` (`tests/boundary_correlation.rs:831`, baseline did not advance because the capture degraded) |

No boundary was cross-consumed in run #17: the *identity* assertions (one
boundary consumed exactly once per post, its own command/exit code/cwd) held on
both failing platforms. The failures concern which representation the run
produces, not identity.

- **Windows — addressed in the working tree.** The two observation-path tests
  now drive both branches through `post_on_healthy`: identity assertions are
  unconditional, and when the bounded scan degrades the durable conservative
  trace (capture gap, unknown interval, reconciliation gate, `rewind reconcile`
  restoring HEALTHY) is asserted instead of assuming an observation. The gated
  (`BoundaryOnly`) scenarios remain fully deterministic and carry the
  exact-identity proof.
- **macOS — NOT addressed.** `tests/shell_integration.rs` is not modified by the
  working-tree fix. That test already implements the both-branch pattern, so the
  failure is not simply "the test assumed an observation": a boundary was
  claimed and consumed while fewer than three observations landed and without
  `ReconciliationRequired`, a pending bypass marker, or an open unknown
  interval. The nature of this failure is **not determined** — candidate causes
  include a genuine gap in the degraded path on macOS, a not-yet-handled
  representation, or an unmodelled scan outcome. It cannot be reproduced on this
  Windows machine, and no macOS environment is available here.

**CI has never been green for Phase 1.4.** The corrected (Windows-fix) revision
has not been pushed, so no CI run exists for it.

## 12. Remaining environment limitations

Only genuine platform limitations remain; nothing is hidden:

- The recorded boundary `cwd` is the workspace root (resolved by marker
  discovery), not the shell's literal `$PWD` at command time. Command and exit
  association is exact; cwd granularity is unchanged from Phase 1.
- A compound command line (`A; B`, `A && B`) in bash produces one DEBUG-trap
  boundary per simple command while a single post-hook completes only the
  last one; the earlier boundaries stay unconsumed and inert (never read,
  never attributed). Zsh records the whole line as one boundary.
- Rewind owns the bash `DEBUG` trap: it does not compose with another
  `DEBUG`-trap user (e.g. `bash-preexec`), and `_REWIND_BOUNDARY_ID` is
  intentionally not exported.
- True power-loss durability cannot be tested; kill-based crash injection is
  the strongest available evidence (carried over from Phase 1.3).
- Windows symlink creation stays capability-gated (honest skips); large
  rollbacks are measured, not redesigned (Phase 2 owns the redesign).

## 13. Verdict

The Phase 1.4 charter's own completion standard (§18) requires: explicit
deterministic boundary correlation; no cross-consumption through the supported
APIs; overlapping/reversed/duplicate/nonexistent/foreign boundary behaviour
tested; rapid bash/zsh command flows tested; exact command/exit-code/cwd
provenance; no baseline regression from a late background observation; full
local tests pass; CI green on Ubuntu/macOS/Windows; no P0/P1 open; no Phase 2
functionality introduced; `.ai` documentation updated.

| Requirement | State |
| --- | --- |
| Explicit, deterministic boundary correlation | met |
| No cross-consumption through the supported APIs | met (database-enforced) |
| Overlap / reversed order / duplicate / unknown / foreign tested | met (10 deterministic tests) |
| Rapid bash command flow tested | met locally; **failed on macOS CI** |
| Rapid zsh command flow tested | locally skipped (no zsh on this host) |
| Exact command / exit-code provenance | met |
| No baseline regression from late background completion | met |
| Full local tests pass | met (48/48, repeated) |
| CI green on Ubuntu/macOS/Windows | **NOT met** (run #17: ubuntu pass, macOS fail, windows fail) |
| No P0/P1 remains | **NOT established** — unresolved macOS CI failure |
| No Phase 2 functionality | met |
| `.ai` state/handoff documentation updated | met |

**PHASE 1 NOT VERIFIED.**

Remaining blocker, stated precisely: CI run #17 failed on
`macos-latest` (`bash_rapid_commands_keep_command_identity`,
`tests/shell_integration.rs:1044`) and on `windows-latest`
(`multiple_sessions_keep_their_own_observation_provenance`,
`observation_chain_advances_the_checkpoint_in_completion_order`). The Windows
failures are addressed in the working tree; the macOS failure is not, and its
nature is not yet determined. Until the corrected revision runs green on both
macOS and Windows, the charter forbids declaring Phase 1 verified.

## 14. Documentation updates in this phase

- Created `.ai/PHASE_1_4_BOUNDARY_CORRELATION_REPORT.md` (this file).
- Appended a clearly marked addendum to
  `.ai/PHASE_1_3_FINAL_FOUNDATION_REPORT.md` (history preserved; the addendum
  records the P1 defect and points here).
- Updated `.ai/HANDOFF.md`, `.ai/CURRENT_STATE.md`, `.ai/CHANGELOG.md`.
- `tests/shell_integration.rs`: the stub `rewind` now prints boundary ids;
  the nonblocking tests also assert the wrapper passes the pre-hook's id to
  the post-hook; the CAS test asserts exact id accounting; plus a new
  rapid-command identity test per shell (three quick commands with distinct
  exit codes, asserting the catalog shows A→A, B→B, C→C with correct exit
  codes and command metadata).
- `tests/hardening.rs` + remaining hook-adjacent tests updated to the
  explicit-id interface.

New invariant (normative for all future phases):

> Every passive post-hook is correlated to exactly one immutable boundary ID.
> A post-hook never discovers or guesses its boundary by recency, timestamp,
> command text, or session ordering. Out-of-order background completion
> advances the trusted checkpoint in lease order only; an older background
> observation finishing later can never regress newer trusted state.

The API change makes the old race structurally impossible: overlapping
background hooks serialize on the writer lease only for bookkeeping, while
consumption is per-id, so two hooks can never take each other's boundary.

## 15. Report repair note

This revision of the file repairs an interrupted write. The previous revision
contained a duplicated `## 11. CI results` section, four sentences truncated
mid-clause, out-of-order sections (4, 5, 3.1 placed after 14), and a CI claim
("ubuntu-latest, macos-latest, windows-latest — all success") together with a
"PHASE 1 VERIFIED" verdict that the actual GitHub Actions run for `b69817d`
does not support. Existing content was reordered and completed, not rewritten;
the CI section and verdict now record the run as observed. The §10 local-gate
results were additionally re-run and confirmed on the current working tree.
