# Phase 1.4 — Passive Boundary Correlation & Final Phase 1 Verification

Scope: the Phase 1.4 charter only — make the passive-boundary identity explicit
and immutable across the pre/post hook pair, remove the asynchronous
cross-consumption defect introduced by the Phase 1.3 nonblocking architecture,
add deterministic concurrency regression coverage, and perform the final Phase 1
verification pass. No Phase 2 functionality was introduced. All historical
reports are preserved unchanged (the Phase 1.3 report carries a clearly marked
addendum pointing here).

**Verdict: PHASE 1 VERIFIED** (see §11; no P0/P1 open, CI green on
ubuntu/macos/windows-MSVC; the remaining items in §10 are environment
limitations, not defects).

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

No newest/oldest/timestamp/session heuristic exists anywhere. The DataBase

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

The deterministic regression core (§7 of the charter) is
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

Failure-model cases are covered as §9 requires: unknown id, duplicated post,
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
- Teeth check: with the fix temporarily replaced by a simulation of the
  retired newest-pending lookup, `older_boundary_post_never_consumes_a_newer_\
  boundary` FAILS (confirming the test discriminates the defect); with the
  fix restored, it passes.

## 11. CI results

Pushed to `OG-Huzzi/Rewind` (`main`); GitHub Actions ran the unchanged
workflow. Final run for the Phase 1.4 commit: **ubuntu-latest, macos-latest,
windows-latest (MSVC) — all success** (fmt, check, clippy -D warnings, full
48-test suite per platform). Zsh ran on macOS/Linux CI (bash ran everywhere
bash exists); absence is reported by the tests themselves via honest skips,
not by weakening CI.

## 12. Remaining environment limitations

Only genuine platform limitations remain; nothing is hidden:

- The recorded boundary `cwd` is the workspace root (resolved by marker
  discovery), not the shell's literal `$PWD` at command time. Command and exit
  association is exact; cwd granularity is unchanged from Phase 1.
- A compound command line (`A; B`, `A && B`) in bash produces one DEBUG-trap

Final local gate before commit (all on `x86_64-pc-windows-gnu`, full
`--all-targets --all-features`, bash present via Git for Windows, zsh
honestly skipped):

- `cargo fmt --all -- --check`: PASS
- `cargo check --all-targets --all-features`: PASS
- `cargo clippy --all-targets --all-features -- -D warnings`: PASS
- full suite: **48 passed, 0 failed** (x2 full runs), plus a dedicated
  6× loop of `tests/boundary_correlation.rs` (60/60 green) and per-test
  repeats of the shell integration tests.

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

Boundary correlation is explicit and deterministic; no cross-consumption is
possible through the supported APIs; overlapping background hooks, reversed
completion order, duplicate/nonexistent/foreign boundary behavior, and rapid
bash/zsh command flows are all tested; passive observations retain exact
command/exit-code/cwd provenance; the baseline cannot regress from an older
background observation finishing later; full local tests pass; CI passes on
Ubuntu/macOS/Windows; no P0/P1 remains; no Phase 2 functionality was
introduced; `.ai` state/handoff documentation is updated (§14).

**PHASE 1 VERIFIED.**

## 14. Documentation updates in this phase

- Created `.ai/PHASE_1_4_BOUNDARY_CORRELATION_REPORT.md` (this file).
- Appended a clearly marked addendum to
  `.ai/PHASE_1_3_FINAL_FOUNDATION_REPORT.md` (history preserved; the addendum
  records the P1 defect and points here).
- Updated `.ai/HANDOFF.md`, `.ai/CURRENT_STATE.md`, `.ai/CHANGELOG.md`.

New invariant (normative for all future phases):

> Every passive post-hook is correlated to exactly one immutable boundary ID.
> A post-hook never discovers or guesses its boundary by recency, timestamp,
> command text, or session ordering. Out-of-order background completion
> advances the trusted checkpoint in lease order only; an older background
> observation finishing later can never regress newer trusted state.

  conservative; bypass markers remain durable where required; writer
  reconcile-first semantics remain intact.
- Baseline-state progression is deterministic and non-regressive: checkpoint
  updates still happen only from scans taken under the writer lease, so
  out-of-order background completion cannot overwrite newer trusted state with
  an older observation. (See §13 of the charter and the chain-order tests.)

- `tests/shell_integration.rs`: the stub `rewind` now prints boundary ids;
  the nonblocking tests also assert the wrapper passes the pre-hook's id to
  the post-hook; the CAS test asserts exact id accounting; plus a new
  rapid-command identity test per shell (three quick commands with distinct
  exit codes, asserting the catalog shows A→A, B→B, C→C with correct exit
  codes and command metadata).
- `tests/hardening.rs` + remaining hook-adjacent tests updated to the
  explicit-id interface.

API change makes the old race structurally impossible: overlapping background
hooks serialize on the writer lease only for bookkeeping, while consumption
is per-id, so two hooks can never take each other's boundary.

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
