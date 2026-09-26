# Phase Contract — Rollback Performance for Large Workspaces

Status: **IMPLEMENTED** (commits recorded in CURRENT_STATE/TEST_STATUS; the
sections below are the contract the implementation follows). This phase was
proposed, measured, and approved on measurement evidence — not roadmap-
mandated. If the measurements had not supported optimization, the deliverable
would have been this document plus a recommendation instead.

Baseline: `main` at `aae28f4` (Phase 5, CI green on all three platforms).

## 1. Measured bottleneck (the only one this phase addresses)

Methodology: `examples/rollback_bench.rs` — a deterministic, reproducible
harness that seeds N files, initializes a real workspace with an external
store, captures one real `run_command` that modifies every file, then times
`undo` and `redo` end-to-end plus one bare full scan, all on a debug build
(the profile of the recorded historical baseline). Environment: Windows 11
(NTFS), rustc 1.98.1, debug profile; the harness prints every timing it
reports and cleans its scratch directory.

Measured baseline (debug, this machine):

| Files | Steps | One bare full scan | UNDO | scan-share estimate |
|---|---|---|---|---|
| 25 | 25 | 58.6 ms | 3.69 s | 52 scans ≈ 3.0 s (83%) |
| 100 | 100 | 232.2 ms | 57.26 s | 202 scans ≈ 46.9 s (82%) |

Scaling matches O(steps × files): 4× the files cost 15.5× the undo time.
The historical record ("undo of 400 modified files ≈ 15.7 minutes, debug,
per-step full-workspace rescan dominates", `.ai/TEST_STATUS.md`) is the same
signature at larger N. A 400-file confirmation run was executed with the
same harness and is recorded in TEST_STATUS alongside these numbers.

Root cause (audit, `src/rollback.rs`): `apply_step` performs **two full
workspace scans per step** (pre-mutation conflict check and post-mutation
verification) plus one entry scan and one final scan per rollback — `2N + 2`
full scans, each walking every file and hashing/CAS-ingesting all content.
The per-step scans' results are consumed **only** as
`manifest.get(path)` — the single affected path's fingerprint. Everything
else the scans compute is discarded.

## 2. Scope

**In scope:** replacing the two per-step full scans with a fingerprint scan
of exactly the affected path (`scan::scan_fingerprint_at`), sharing the
scanner's per-entry classification so the produced `Fingerprint` is
byte-identical to what the full scan yields for that path.

**Non-goals:** journal write batching or durability changes; CAS layout
changes; watcher behavior; scan algorithm changes for reconcile/capture;
parallelism; release-profile tuning; any public API change beyond the new
internal scan function; touching recovery paths (`recover_locked` keeps its
full scans — recovery correctness outranks speed).

## 3. Correctness invariants and safety boundaries (unchanged)

- Per-step pre-conflict detection and post-verification still compare the
  **same fingerprints** they compared before: `actual == desired`,
  `actual == expected_before`, and `compatible_after(actual_after, desired)`
  receive exactly the fingerprint the full scan would have produced for that
  path.
- Global deviation detection is and remains the **final full scan** whose
  `state_id` must equal the target state; any external interference
  anywhere — including mid-operation — still lands in `RecoveryRequired`
  exactly as before. (The old per-step full scans never inspected unrelated
  paths' values; unrelated-path scan errors now surface at the authoritative
  final scan instead of aborting earlier — same abort class, later timing.)
- Journal durability is untouched: the same writes at the same points,
  including the per-step `Durable` transitions under
  `PRAGMA synchronous = FULL`. The remaining ~18% non-scan cost is mostly
  journal durability and is explicitly left alone.
- Quarantine, confinement checks, backup matching, anchor creation, and the
  planner are untouched. Path-scoped scans confine to the workspace root
  with the same `normalize_relative` machinery and refuse escapes.
- Deterministic state identity: fingerprints are produced by the same
  classification and hashing code as the full scan (shared per-entry
  function), so `state_id` values are unchanged bit-for-bit.

## 4. Measurable objective

On the reference machine and methodology above (debug build):

- Undo of 100 modified files: **57.3 s → ≤ 12 s** (≥ 4.8× faster).
- Redo improves by the same order (48.0 s baseline).
- The 400-file debug case collapses proportionally (historically ≈ 15.7 min).
- The optimization must show zero behavioral change: the entire existing
  suite (including conflict, recovery, quarantine, and Phase 5 tests) passes
  unchanged, and the final-state verification still refuses a mismatched
  workspace (adversarial test below).

## 5. Adversarial acceptance criteria

- **AC1 Scaling:** the benchmark's undo time grows with **touched work**
  (steps), not with total file count: at fixed step count, adding untouched
  files adds only the two full scans' cost.
- **AC2 Conflict semantics:** a workspace where a target path changed
  externally after capture still refuses undo with the same `Conflict`
  error and untouched filesystem (existing tests must keep passing).
- **AC3 Final verification still authoritative:** a post-capture external
  modification to an *untouched* path still forces `RecoveryRequired` at the
  final full scan (the per-step scans never guarded this; the final scan
  does).
- **AC4 Interrupted operation:** an interrupted rollback (existing recovery
  tests) still classifies and completes identically.
- **AC5 Idempotence:** re-running undo on an already-rolled-back workspace
  still short-circuits per step (`actual == desired`) with the same result.
- **AC6 Determinism:** state ids produced by capture and reconcile are
  unchanged (existing suite asserts this transitively).
- **AC7 Full suite:** every existing test passes unmodified on all three
  CI platforms; the benchmark harness is committed as the regression
  benchmark.

## 6. Explicit exclusions

No new dependencies; no parallel scanning; no change to `ScanOptions`
semantics; no caching of fingerprints across steps (each step re-observes
its path from the live filesystem — nothing is assumed unchanged); no
watcher involvement; no CLI changes.
