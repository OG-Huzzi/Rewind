# Current Implementation State

Status: Rollback-performance phase (measurement-driven, ADR-018)
**implemented and CI-verified** — per-step rollback verification observes
only the affected path; 400-file undo ≈ 15.8 min → ≈ 1.25 min (debug) with
identical safety anchors; contract at `.ai/PHASE_ROLLBACK_PERF.md`; CI
run #43 on `6ec5e06` green on ubuntu/macOS/Windows. Phase 5 (platform
expansion) first slice **implemented and
CI-verified** — POSIX named pipes (FIFOs) as first-class objects; contract
at `.ai/PHASE_5_PLATFORM_EXPANSION.md` including the object×platform
capability matrix; final CI green on ubuntu/macOS/Windows (see
TEST_STATUS.md for the run and the FIFO lifecycle evidence). Phase 4 first slice (time-range history view) **implemented and
CI-verified** — commit `1f37ec3` on `main`, CI run 36228897539 green on
ubuntu/macOS/Windows; contract at `.ai/PHASE_4_TIME_AND_ECOSYSTEM.md`
(status corrected there post-implementation; recipes deferral is ADR-016).
Phase 3 (continuous observation) complete and CI-verified
(run #36111890265 on `d69fdca`: ubuntu, macOS and windows all green; see
`.ai/PHASE_3_VERIFICATION_REPORT.md`, **Phase 3 VERIFIED** — full suite
119 passed / 0 failed locally, all 63 Phase 1/2 tests unchanged). Phase 2
(dependency-aware inspection) is implemented and CI-verified (run #23 on
`4d89a2f`: ubuntu, macOS and windows all green) and documented in
`.ai/PHASE_2_VERIFICATION_REPORT.md` (**Phase 2 VERIFIED**).

## Post-Phase-3 audit fixes (branch `fix/phase3-watcher-quoting`)

Two audit defects fixed on top of `796ced6` (commit `c881ba3` plus a
follow-up hardening commit on the same branch):

- **Multi-path notification truncation** (`src/watch/adapter.rs`): a
  notification carrying several paths mapped to several `RawEvent`s but
  `poll()` returned only the first. The adapter now retains the remainder
  in a `VecDeque` capped at `PENDING_CAPACITY` (4096) and delivers queued
  events before polling the channel. A batch larger than the capacity is
  truncated at the cap and reported as `Overflow` once the preserved
  prefix drains — the un-preserved remainder becomes a durable OVERFLOW
  degradation (unknown interval → reconciliation), never silent loss.
- **Windows argument quoting** (`src/watch/detach_windows.rs`): plain
  `"{argument}"` wrapping let a trailing backslash escape the closing
  quote (CRT parsers read `\"` as a literal quote and glue the following
  arguments). `quote()` now implements the MSVCRT rules (2n+1 backslashes
  before an embedded quote, 2n before the closing quote); embedded quotes
  are escaped instead of rejected. A local probe proved the Rust child
  (`std::env::args`) follows the CRT rules and that `CommandLineToArgvW`
  follows different rules, so the CRT rules are the round-trip reference.

Local suite on the branch: 129 passed / 0 failed (49 lib unit incl. the
new capacity tests, 17 `phase3_watcher` incl. a real detached-spawn
end-to-end test; every Phase 1/2 suite unchanged and green). Platform
verification for the branch happens through its PR CI run.

## Rollback performance phase (proposed, measured, delivered)

- Measured bottleneck: per-step full-workspace scans in rollback (2N+2 per
  operation, 82-84% of undo time, quadratic scaling). Contract and
  acceptance criteria in `.ai/PHASE_ROLLBACK_PERF.md`; decision ADR-018.
- Fix: `scan::scan_fingerprint_at` + `Workspace::scan_path` — per-step
  verification observes exactly the affected path via the scanner's shared
  per-entry classification; fingerprints are identical to a full scan by
  construction. Final full-scan state verification, journal durability,
  conflict, quarantine, and confinement semantics untouched.
- Measured result (debug, Windows/NTFS): 100-file undo 57.3 s → 9.35 s
  (6.1×); 400-file undo 950.5 s (15.8 min) → 74.7 s (12.7×); redo similar.
  Harness committed as `examples/rollback_bench.rs`.
- Suite: 152 passed / 0 failed (3 new equivalence/adversarial tests).
- CI: run #43 on `6ec5e06` green on ubuntu/macos/windows (run #42 on
  `dd80f6b` likewise); identifiers and the local re-verification are
  recorded in TEST_STATUS.md.

## Completed in Phase 5 (first slice)

- POSIX named pipes (FIFOs) are first-class objects: `Fingerprint::NamedPipe`
  (existence + permission mode only — a FIFO's in-flight content is kernel
  state no manifest can hold), scanner classification from file type alone
  (never opened or read), restore through `mkfifo` with a private initial
  mode and the recorded authoritative mode, inside the existing
  journal/quarantine/confinement/verification machinery. A workspace
  containing a FIFO no longer poisons undo/redo on Linux/macOS.
- Scanner refusal descriptors now name the object class (`UNIX_SOCKET`,
  `CHARACTER_DEVICE`, `BLOCK_DEVICE`) instead of a debug string.
- Capability matrix shipped in the Phase 5 contract; Windows object behavior
  unchanged (the scanner can never produce `NamedPipe` there; a foreign
  manifest containing one is refused at materialization).
- Sockets, device nodes, junctions, xattrs/ACLs, ownership: explicitly
  unsupported or deferred (see contract §2).

## Completed in Phase 4 (first slice)

- `rewind inspect timeline --since <RFC3339> [--until <RFC3339>] [--json]`:
  a read-only, evidence-tier-aware view of recorded history in a half-open
  time range (since inclusive, until exclusive). Merges operations,
  snapshots, passive boundaries, unknown intervals, and bounded watcher
  evidence (degradation records + per-kind event summary); unknown intervals
  and watcher degradations force `history_complete=false`; the watcher log's
  coverage bounds are stated, never silently trusted. Deterministic ordering
  `(timestamp, tier, id)` and byte-identical JSON for identical inputs.
- `src/humantime.rs`: dependency-free, exhaustively tested RFC 3339
  parsing/formatting (epoch microseconds; verified against `date -u`).
- `src/timeline.rs`: the pure merge plus the read-only watcher-evidence
  reader; `src/db.rs` gained `snapshots()` and `WorkspaceRow.created_at`.
- Package-specific recipes: evaluated and deferred (ADR-016); range-based
  restore excluded (ADR-015).

## Completed in Phase 3

- The advisory watcher subsystem (see `.ai/PHASE_3_VERIFICATION_REPORT.md`
  for the clause-by-clause map): platform-neutral `FsEvent` model;
  inotify/FSEvents/ReadDirectoryChangesW adapters behind an `EventAdapter`
  trait plus a deterministic fake adapter for tests; a serve loop doing
  batching, bounded coalescing (`dirty.json`), raw-evidence event log with
  rotation, heartbeat lifecycle (`state.json`), and durable degradation
  records (`degraded.json`); `rewind watch start/status/stop`; crash/
  restart and offline gap semantics (any prior run ⇒ an unobserved interval
  until reconciliation); overflow/adapter-failure/dirty-cap degradation;
  enforcement converting pending watcher degradations into unknown
  intervals + RECONCILIATION_REQUIRED inside the existing single-writer
  paths; Windows detached spawn with no handle inheritance (raw
  `CreateProcessW`, `bInheritHandles = FALSE`) after root-causing a real
  pipe-capture hang. The watcher opens no catalog, takes no lease, and
  never writes inside the workspace.

## Completed

- Phase 0 through Phase 0.7 documentation and architecture finalization.
- Phase 1 implementation: Rust package, external store, SQLite catalog,
  typed scanner, BLAKE3 CAS, strong capture, passive boundary commands,
  degradation/reconciliation, snapshots, journaled rollback, redo,
  recovery inspection, and CLI foundation.
- Phase 1 independent verification (V-F01 P0, V-F02 P1, plus secondary
  defects; see `.ai/PHASE_1_INDEPENDENT_VERIFICATION.md` — the failure
  record is preserved as history).
- Phase 1.1 repairs: transaction-aware rollback expectations (V-F01),
  `rewind recover` / `recover --reconcile` with ABANDONED journal state
  (V-F02), reparse-tag junction classification (V-F03), archive flush
  (V-F04), CLI argument fix (V-F05), fsync durability (V-F06), staging
  cleanup (V-F07), bash/zsh integration (V-F09).
- Phase 1.2 hardening (charter fixes #1–#6, #18, #20): bounded fail-open
  passive hook; lock owner-metadata protection; post-mutation TOCTOU
  confinement verification; recursive archive verification; Windows symlink
  restoration from recorded reparse target kinds; schema-versioned state
  identity; idempotent startup repair; failed recovery lands
  RECOVERY_REQUIRED.
- Phase 1.3 passive-hook isolation: bash/zsh integrations launch the
  post-hook in the background (shell never waits for bookkeeping); the
  false 150 ms budget is removed (scan deadline starts at the scan; a 2 s
  lease retry avoids spurious gates); every hook failure lands in the
  conservative bypass/CAPTURE_FAILED model.
- Phase 1.4 passive boundary identity: the retired "newest unconsumed
  boundary of the session" post-hook lookup is deleted. The pre-hook
  prints the boundary's immutable id, the shell holds it for exactly one
  command, and the post-hook claims exactly that id exactly once
  (guarded `UPDATE` + a `passive_boundary_consume_once` DB trigger).
  Unknown, duplicate, and cross-workspace ids fail open with no side
  effects. Ordering: out-of-order background completion advances the
  trusted checkpoint in lease order only, never regressing it.
- Test suite: 48 real-filesystem integration tests (13 foundation +
  9 rollback_tree + 8 hardening + 10 boundary-correlation [new] +
  8 shell-integration), all portable across Windows/POSIX.
- CI: GitHub Actions (ubuntu-latest, macos-latest, windows-latest MSVC)
  running fmt/check/clippy(-D warnings)/test per platform.

## Verification status

- Local gates all green on `x86_64-pc-windows-gnu` (Rust 1.98.1): fmt, check,
  clippy (-D warnings), and the full 48-test suite run repeatedly.
- Phase 1.4 CI is green for `4b5dddb` (run #19: ubuntu, macOS, windows/MSVC).
  The two preceding revisions had failed on macOS, and `b69817d` also on
  windows; see the Phase 1.4 report section 11 for the per-platform results.
- Phase 2 local gates are green on `x86_64-pc-windows-gnu`: fmt, check, clippy
  (-D warnings) and the full suite, 76 passed / 0 failed (Phase 1's 48 unchanged
  plus 25 new Phase 2 tests). CI run #23 on `4d89a2f` is green on ubuntu, macOS
  and windows. Note: `bash` must be on `PATH` for the shell-integration tests to
  execute rather than skip (Git for Windows: `C:\Program Files\Git\bin`).
- zsh coverage runs where zsh is available (macOS/Linux CI) and reports an
  honest skip on windows-latest.

## Completed

- Phase 0 through Phase 0.7 documentation and architecture finalization.
- Phase 1 implementation: Rust package, external store, SQLite catalog,
  typed scanner, BLAKE3 CAS, strong capture, passive boundary commands,
  degradation/reconciliation, snapshots, journaled rollback, redo,
  recovery inspection, and CLI foundation.
- Phase 1 independent verification (V-F01 P0, V-F02 P1, plus secondary
  defects; see `.ai/PHASE_1_INDEPENDENT_VERIFICATION.md` — the failure
  record is preserved as history).
- Phase 1.1 repairs: transaction-aware rollback expectations (V-F01),
  `rewind recover` / `recover --reconcile` with ABANDONED journal state
  (V-F02), reparse-tag junction classification (V-F03), archive flush
  (V-F04), CLI argument fix (V-F05), fsync durability (V-F06), staging
  cleanup (V-F07), bash/zsh integration (V-F09).
- Phase 1.2 hardening (charter fixes #1–#6, #18, #20):
  - bounded fail-open passive hook (50 ms scan deadline / 150 ms total
    budget; durable bypass markers; deferred recovery is writer work);
  - lock file opened without truncate; owner metadata written only after
    winning the exclusive lock;
  - post-mutation TOCTOU confinement verification on every rollback step;
  - recursive archive verification before any Archived marking or
    quarantine disposal;
  - Windows symlink restoration from recorded reparse target kinds,
    never filename extensions; unknown kinds refuse restoration;
  - schema-versioned state identity (STATE_SCHEMA_VERSION = 2) with v1
    manifests deserializing at `target_kind = unknown`;
  - idempotent journal-authoritative startup repair of committed
    metadata; failed recovery always lands RECOVERY_REQUIRED.
- Phase 1.3 passive-hook isolation: bash/zsh integrations launch the
  post-hook in the background (shell never waits for bookkeeping); the
  false 150 ms budget is removed (scan deadline starts at the scan; a 2 s
  lease retry avoids spurious gates); every hook failure lands in the
  conservative bypass/CAPTURE_FAILED model.
- Phase 1.4 passive boundary identity: the retired "newest unconsumed
  boundary of the session" post-hook lookup is deleted. The pre-hook
  prints the boundary's immutable id, the shell holds it for exactly one
  command, and the post-hook claims exactly that id exactly once
  (guarded `UPDATE` + a `passive_boundary_consume_once` DB trigger).
  Unknown, duplicate, and cross-workspace ids fail open with no side
  effects. Ordering: out-of-order background completion advances the
  trusted checkpoint in lease order only, never regressing it.
- Test suite: 48 real-filesystem integration tests (13 foundation +
  9 rollback_tree + 8 hardening + 10 boundary-correlation [new] +
  8 shell-integration), all portable across Windows/POSIX.
- Repo hygiene: `.gitignore`; 5195 tracked `target/` artifacts untracked;
  machine-local `.cargo/config.toml` untracked.
- CI: GitHub Actions (ubuntu-latest, macos-latest, windows-latest MSVC)
  running fmt/check/clippy(-D warnings)/test; failures surface as
  annotations (logs otherwise require admin auth on this repo).
- Measured (not redesigned): 400-file snapshot ≈ 2.5 s; 400-file
  restore/undo ≈ 15.7 min (debug, per-step rescan dominates). Phase 2
  baseline.

## Verification status

- Local gates all green on `x86_64-pc-windows-gnu` (Rust 1.98.1): fmt,
  check, clippy (-D warnings), and the full 48-test suite run repeatedly.
- Phase 1.3 CI (ubuntu, macos, windows/MSVC) was green for the Phase 1.3
  commit; the Phase 1.4 commit is verified locally and CI is observed on
  push (see the Phase 1.4 report §11 for the final per-platform results).
- zsh coverage runs where zsh is available (macOS/Linux CI) and reports an
  honest skip on windows-latest.
- ubuntu/macos: one POSIX-only clippy warning (`unneeded return` in
  `scan::metadata_fingerprint`, not visible to the Windows-host clippy)
  was surfaced via annotations and fixed in Phase 1.2; final per-platform
  results are in the Phase 1.2 report.

## Not started

- Range-based restore (excluded by the Phase 4 contract, ADR-015).
- Package-specific recipes (evaluated and deferred, ADR-016).
- Phase 5 platform expansion.

## Known limitations

- Rollback no longer rescans the whole workspace per step (ADR-018): 400-file
  undo ≈ 1.25 min (debug, NTFS), down from ≈ 15.8 min. The remaining cost is
  journal durability (`synchronous = FULL`) plus the mutations themselves —
  deliberately untouched. Correctness is still prioritized; no performance
  guarantee exists.
- True power-loss durability cannot be tested; kill-based crash
  injection and physical quarantine-move simulation are the strongest
  available evidence.
- Windows symlink creation is capability-gated where the account lacks
  SeCreateSymbolicLink (tests skip gracefully; runners typically grant
  it).
- Pre-existing staging directories left by transactions abandoned before
  Phase 1.1 whose archive failed are retained on disk by design (data
  safety) and must be removed manually after inspection.
- The recorded boundary `cwd` is the workspace root (marker discovery), not
  the shell's literal `$PWD`; command/exit association is exact. A bash
  compound line (`A; B`) mints one boundary per simple command while a single
  post-hook completes the last; Rewind owns the bash `DEBUG` trap. See the
  Phase 1.4 report §12 for the full list; none of these is a defect.
