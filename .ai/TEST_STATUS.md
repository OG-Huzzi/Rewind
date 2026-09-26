# Phase 1 Test Status

Status after the rollback-performance phase. Newer records are at the top;
the Phase 5, Phase 4, Phase 3, and Phase 1.3 records are preserved below.

## Rollback performance phase (path-scoped step verification)

Contract: `.ai/PHASE_ROLLBACK_PERF.md` (measured bottleneck, scope,
invariants, objective, adversarial criteria); decision: ADR-018.

Methodology: `examples/rollback_bench.rs` — deterministic harness seeding N
files, one real captured operation modifying every file, then end-to-end
undo/redo timings plus one bare full scan. Environment: Windows 11, NTFS,
rustc 1.98.1, **debug build** (the historical baseline's profile). The
400-file baseline independently reproduced the historical Phase 2 record
(950.5 s vs the recorded ≈ 15.7 min).

Before → after (same machine, same harness):

| Files | UNDO before | UNDO after | speedup | REDO before | REDO after |
|---|---|---|---|---|---|
| 25 | 3.69 s | 2.05 s | 1.8× | 4.23 s | 2.09 s |
| 100 | 57.26 s | 9.35 s | 6.1× | 48.02 s | 13.24 s |
| 400 | 950.51 s (≈15.8 min) | 74.73 s (≈1.25 min) | 12.7× | 1012.88 s | 70.99 s |

The measured bottleneck: `apply_step` performed two full workspace scans per
step (2N+2 per rollback) while consuming only the affected path's fingerprint
— 82-84% of undo time at every scale. The fix (`scan::scan_fingerprint_at`,
shared per-entry classification with the full scanner) makes the fingerprints
identical by construction; global correctness stays anchored on the final
full-scan state comparison, which is unchanged. The remaining ~75 s at 400
files is journal durability (`synchronous = FULL`) plus actual mutations —
explicitly out of scope.

Scenario and payload variants (same harness and environment, post-optimization
code, debug build; the harness grew `--scenario modify|create|delete|mixed`
and scenario-aware final-state assertions, and every variant asserts exactly N
rollback steps and the correct final content after redo):

| Variant (N=100) | capture | UNDO | REDO |
|---|---|---|---|
| modify, payload 200 B | 0.93 s | 9.35 s | 13.24 s |
| modify, payload 10 240 B | 1.81 s | 7.68 s | 7.88 s |
| create 100 files | 1.21 s | 4.11 s | 3.69 s |
| delete 100 files | 0.88 s | 2.84 s | 4.45 s |
| mixed (34 deleted / 33 modified / 33 created) | 1.10 s | 4.55 s | 4.40 s |

Reading: undo time tracks the *touched* work (steps) rather than total
payload — a 51× payload increase leaves undo at the same order (9.35 s →
7.68 s, within run-to-run noise on this machine), and create/delete/mixed
land in the same seconds band as modify. Pre-optimization equivalents were
not re-measured per scenario (the dominant cost — 2N+2 full scans — is
scenario-independent and was measured directly in the modify table above);
the harness's historical-model line reconstructs it from one bare scan.

Suite after the change (Windows, debug): **152 passed / 0 failed**
(`cargo test --all-targets`), including 3 new
`tests/rollback_path_scan.rs` tests proving fingerprint equivalence across
every object type (files, nested/deep directories, absent paths, and — on
POSIX — symlinks, FIFOs, sockets), live re-observation, plan-time conflict
semantics, and unchanged `RecoveryRequired` behavior. The `rollback_tree`
suite itself dropped from ≈ 45 s to ≈ 10 s as a side effect. POSIX-specific
equivalence runs ride the ubuntu/macOS CI jobs.

CI verified for the phase commits: run #42 on `dd80f6b` and run #43 on
`6ec5e06` (the phase-final tree, 2026-09-26), each completed / success on
ubuntu-latest, macos-latest, and windows-latest, with Format, Compile (all
targets), Clippy (-D warnings), and Tests green per job — the ubuntu and
macOS Tests steps therefore executed the POSIX-only `rollback_path_scan`
equivalence cases (symlinks, FIFOs, sockets) and the POSIX-gated Phase 5
tests. Re-verified locally 2026-09-26 (Windows, rustc 1.98.1): fmt, clippy
`-D warnings`, `cargo test --all-targets` (152 passed / 0 failed), and
`cargo test --doc` all green.

The 149-test record below describes `aae28f4` before this phase.

---

## Phase 5 (platform expansion — POSIX named pipes)

Local suite on Windows (x86_64-pc-windows-gnu, Rust 1.98.1): **149 passed /
0 failed** (`cargo test --all-targets`; fmt, clippy
`--all-targets --all-features -- -D warnings`, `cargo test --doc` green).
The new `tests/phase5_platform.rs` suite runs 1 model-level test
(backward-compatible serde) on every platform and 4 POSIX-gated tests — FIFO
scan classification, FIFO create/undo/redo with recorded mode, FIFO→file
replacement reversibility, and the UNIX_SOCKET refusal — which the local
Windows host **cannot execute**; they are verified by the Linux and macOS CI
jobs (this is the same pattern as Phase 1.1: local cross-checks are exactly
what CI covers). Windows behavior for all pre-existing object types is
unchanged and covered by the unchanged Windows CI job.

Deliberate-failure checks and CI-repair cycles (real defects the POSIX CI
caught that the Windows host cannot even compile):
1. `std::os::unix::fs::mkfifo` is unstable (rust-lang/rust#139324) — lib and
   tests failed to compile on Linux/macOS; fixed by declaring `mkfifo(2)`
   directly (crate FFI site #3, `mode_t` per platform ABI) and creating test
   FIFOs via the `mkfifo` utility.
2. `undo` takes `Option<i64>` (test passed it double-wrapped) and an unused
   unix-only binding — compile errors invisible locally.
3. **A real product hang:** `sync_target` opened any non-symlink target
   read-only to fsync it; `open(2)` on a FIFO blocks until a writer appears,
   so redo/undo that recreate a FIFO hung the POSIX runners until the run
   was cancelled (~6 h). Fixed by never opening a FIFO: its directory-entry
   durability is the parent-directory fsync that was already there.

**CI verified (commit `4d5f242`, run of 2026-09-26): ubuntu-latest,
macos-latest, and windows-latest all completed / success.** The ubuntu
runner's log shows the full `phase5_platform` suite executing: 5 passed /
0 failed — `posix::a_fifo_scans_as_a_supported_named_pipe`,
`posix::fifo_creation_is_captured_and_reversible`,
`posix::replacing_a_fifo_with_a_file_is_reversible` (the FIFO create → undo
→ redo lifecycle with recorded-mode preservation, verified on real Linux),
`posix::unix_sockets_stay_unsupported_and_refuse_undo`, and the
cross-platform serde compatibility test. Windows ran the same suite with the
POSIX module compiled out (1 passed), its object behavior unchanged.

The 148-test record below describes `1f37ec3` before the Phase 5 work.

---

## Phase 4 first slice (time-range history view)

Local suite (x86_64-pc-windows-gnu, Rust 1.98.1): **148 passed / 0 failed**
(`cargo test --all-targets`; fmt, clippy `--all-targets --all-features -- -D
warnings`, and `cargo test --doc` green): 61 lib unit tests (5 `humantime`
incl. adversarial timestamp rejection and non-ASCII panic-safety, 7
`timeline` incl. boundary/tiling/uncertainty/determinism, plus the 49
prior), and a new **7-test `phase4_timeline`** integration suite driving the
real CLI: empty history, half-open boundaries over a real capture, byte-identical
JSON across invocations, uncertainty exposure with byte-identical catalog +
workspace across the read-only view, watcher summary with rotation coverage
and unparseable-line accounting, invalid ranges mutating nothing, and human
output tier honesty.

Deliberate failure checks: the read-only byte-identity assertion was
verified to catch a WAL-checkpoint window (test now checkpoints before
snapshotting); exit-code semantics for malformed bounds were driven from
observed failures to the contract's exit 2.

The 129-test record below describes `c4aeef7` before the Phase 4 work.

---

## Post-Phase-3 audit-fix branch (`fix/phase3-watcher-quoting`)

Local suite on the branch (x86_64-pc-windows-gnu, Rust 1.98.1): **129
passed / 0 failed** (`cargo test --all-targets`, run twice; fmt, clippy
`--all-targets --all-features -- -D warnings`, and `cargo test --doc` all
green): 49 lib unit tests (the 47 from `c881ba3` plus 2 new capacity
tests), 10 boundary_correlation, 13 foundation, 8 hardening,
15 phase2_dependency, **17 `phase3_watcher`** (16 plus a real
detached-spawn end-to-end test), 9 rollback_tree, 8 shell_integration.

New on the branch, beyond `c881ba3`:

- Adapter queue capacity: `poll_reports_overflow_when_a_batch_exceeds_the_pending_capacity`
  (a batch over `PENDING_CAPACITY` delivers its preserved prefix in order,
  exactly once, then reports `Overflow` once, then idles) and
  `serve_loop_records_a_degradation_when_capacity_is_exhausted`
  (production channel → serve loop → durable OVERFLOW record + preserved
  prefix in the dirty index). The exhaustion tests were verified to fail
  with the capacity check disabled.
- Windows detached spawn end-to-end: `detached_spawn_delivers_the_intended_root_to_a_real_child`
  spawns the real binary through the production detached path with a
  `--root` containing a space and a trailing backslash; the child must
  discover the workspace, report a fresh heartbeat, and stop cleanly.
  Verified to fail against the pre-fix `format!("\"{argument}\"")`
  quoting. Note: this test cannot distinguish a *dropped* trailing
  separator (invisible to discovery) — that variant is pinned by the
  byte-exact serialization table, the CRT round-trip decoder, and the
  real-child clap echo test.
- Unicode quoting cases in the serialization table, the CRT round-trip,
  and the real-child clap echo test.

The 119-test record below describes `796ced6` before the fix branch.

---

**Full suite after Phase 3: 119 passed / 0 failed** (`cargo test
--all-targets`, x86_64-pc-windows-gnu, Rust 1.98.1, bash on PATH so the
shell-integration tests ran rather than skipped), **and CI run #36111890265
on `d69fdca` is green on ubuntu-latest, macos-latest and windows-latest**
(after two earlier rounds surfaced one POSIX-only compile defect and three
test-side wait races, both root-caused and fixed — see
`.ai/PHASE_3_VERIFICATION_REPORT.md` §9): 41 lib unit tests (13 Phase 2 +
28 Phase 3), 10 boundary_correlation, 13 foundation, 8 hardening,
15 phase2_dependency, 9 rollback_tree, 8 shell_integration — every Phase 1/2
suite unchanged and green — plus the new **15 `phase3_watcher`** tests.

Phase 3 tests prove, beyond "it runs" (details and evidence in
`.ai/PHASE_3_VERIFICATION_REPORT.md` §5-6):

- **Overflow is injected through a deterministic fake adapter** (contract
  §16): the degradation record is durable immediately, the dirty set
  survives, the watcher marks itself degraded, the next writer opens the
  unknown interval naming the reason, reconcile closes it and consumes the
  marker, and the watcher alone never gates anything.
- Coalescing collapses duplicates into the dirty index while the raw event
  log keeps every event (evidence is never erased); create+delete leaves
  the path dirty; the index serialization is sorted and byte-identical for
  identical inputs.
- The dirty cap degrades instead of growing; events outside the canonical
  root and under `.rewind` are dropped and counted as anomalies.
- Crash/restart and offline semantics: a 13-minute-dead watcher produces a
  `WATCHER_GAP` spanning last-heartbeat→now that gates the next writer and
  closes only via reconcile; a graceful stop leaves everything after it
  unobserved; a first-ever start records nothing.
- Isolation: across a serve loop with events, `metadata.sqlite` is
  byte-identical and the workspace tree fingerprint-identical — the
  watcher writes nothing outside its own store directory.
- Real-adapter lifecycle through the CLI: detached start (which must not
  hang pipe-captured invocations — see the verification report's D2),
  RUNNING via fresh heartbeat, a real file write landing in the index and
  event log, a concurrent writer with no lease interference, bounded stop,
  second-start refusal, and a dead watcher reporting FAILED.
- Measured (contract §16, debug build, NTFS): idle CPU 15.6 ms over 10 s;
  event→index latency 113–208 ms at a 250 ms batch; ~150 bytes per raw
  event; ~19 bytes per dirty path.

Known gaps that remain verification work, not passing claims:

- Platform overflow was injected through the fake adapter, not induced on
  real hardware; the real-adapter overflow path is exercised only by
  construction (the `Rescan` flag mapping), matching the contract's
  allowance for deterministic injection.
- Real power-loss durability of watcher artifacts is not claimed (they are
  advisory; the catalog and journals remain the durable records).

---

## Phase 1.3 status (historical)

The suite uses real temporary workspaces, real filesystem mutations, real
subprocess writers, and real crash simulation. No mocked filesystems.

Passed locally (Rust stable 1.98.1, `x86_64-pc-windows-gnu` host,
NTFS): 36 integration tests, 0 failed.

- 13 foundation tests (portable command vectors: `.cmd` via `cmd /C` on
  Windows, executable `.sh` on POSIX; CRLF/LF handled by `common::echoed`).
- 9 rollback_tree tests (V-F01/V-F02 regressions: non-empty/nested tree
  undo and redo, interrupted rollback recovery, external-modification
  refusals, junction classification, archive verification, CLI e2e).
- 6 shell-integration tests (Phase 1.3, `tests/shell_integration.rs`):
  the shell wrapper exits while a stub 45 s post-hook is still running
  (threshold-free ordering proof; bash everywhere, zsh where the runner
  ships it — windows-latest reports the skip honestly); real-wrapper
  observation/degradation flows; busy catalog, terminated hook process,
  missing workspace, and unwritable CAS all fail open and gate the next
  writer without fabrication.
- 8 hardening tests (Phase 1.2 fixes, `tests/hardening.rs`):
  - lock owner metadata survives a failed contender (in-process, and
    against a real cross-process `rewind run` writer);
  - bounded passive hook fails open on a real 1500-file workspace:
    exit 0 in bounded time, CAPTURE_FAILED + unknown interval recorded,
    undo refused, `reconcile` restores HEALTHY with no fabricated
    STRONG operation;
  - the hook defers unfinished-transaction recovery to the writer and
    records a durable bypass marker that gates reconcile-first;
  - committed-metadata crash window repairs idempotently (journal is
    authority, catalog rows converge, second repair is a no-op);
  - recursive archive verification catches nested corruption, missing
    and extra entries, wrong directory shape, type mismatch, and wrong
    symlink targets (POSIX);
  - v1 manifests (no `target_kind`) deserialize with unknown kind, and
    state ids hash the schema-versioned envelope;
  - recovery failure never leaves a false HEALTHY, and
    `recover --reconcile` archives, marks ABANDONED, and checkpoints;
  - symlinked-parent rollback refusal (POSIX): external target never
    followed (real symlink to outside the workspace);
  - symlink target kinds recorded from evidence and restore faithfully
    (POSIX: file, directory, dangling).

Local gates: `cargo fmt --all -- --check`, `cargo check --all-targets`,
`cargo clippy --all-targets -- -D warnings`, `cargo test` — all passing.

CI (GitHub Actions, `OG-Huzzi/Rewind`): ubuntu-latest, macos-latest,
windows-latest (MSVC host) — fmt/check/clippy(-D warnings)/test per
platform. windows-latest (MSVC) passed in full on the first Phase 1.2
run (compile, clippy, and all 30 tests). Initial runs surfaced one
POSIX-only clippy warning (`unneeded return` in `metadata_fingerprint`,
invisible on the Windows host clippy) — fixed; the final per-platform
results are recorded in `.ai/PHASE_1_2_HARDENING_REPORT.md`.

Measured performance (documented per charter; no performance redesign):

- 400-file tree: snapshot ≈ 2.5 s per invocation (scan + CAS);
  restore/undo of 400 modified files ≈ 15.7 minutes in the debug build
  (per-step full-workspace rescan dominates). Recorded as the Phase 2
  baseline; the Phase 1.2 charter forbids performance redesign.

Known gaps that remain verification work, not passing claims:

- Real power-loss durability is not testable in CI; kill-based crash
  injection and physical quarantine-move simulation are the strongest
  available evidence, plus one real `kill -9` mid-undo of a 300-file
  tree that recovered correctly during Phase 1.1 verification.
- Windows symlink creation is capability-skipped when the runner account
  lacks SeCreateSymbolicLink (GitHub-hosted runners grant it; local
  accounts may not). The graceful-skip path is itself asserted.
- The TOCTOU confinement check narrows the symlink-swap window to the
  interval between the final pre-mutation check and the mutation itself
  (accepted residual race under the Phase 0.7 conditional guarantee).

---

## Phase 1.1 status (historical, 2026-09)

Passed: 22 real-filesystem tests.

- 13 foundation tests (Phase 1 suite).
- 9 rollback_tree regression tests: non-empty/nested/large tree undo and
  redo, interrupted recursive rollback recovery, external-modification
  refusals, junction classification, archive verification with staging
  cleanup, CLI parse/e2e coverage.

Gates at that time: fmt/check/clippy(-D warnings) passed; tests 22/22 on
`x86_64-pc-windows-gnu`; Linux/macOS cross-checks could not compile
(bundled SQLite C cross-toolchain absent) and MSVC was unavailable —
which is exactly what Phase 1.2 CI now covers.
