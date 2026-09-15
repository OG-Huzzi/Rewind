# Phase 2 — Dependency-Aware Inspection: Verification Report

Status: implementation complete against the Phase 2 contract; **local gates
green; CI not yet run for any Phase 2 commit.**

**Verdict: PHASE 2 NOT VERIFIED.** Every local requirement is met and every
artifact exists, but the charter's completion standard (§18, §21) requires green
CI on ubuntu-latest, macos-latest and windows-latest **for the exact pushed
commit**, and Phase 2 has not been pushed. Local Windows results alone never
justify "Phase 2 verified".

---

## 1. Baseline and contract

- **Phase 1 baseline:** `aff0dbb`, the Phase 1.4 verification commit. Phase 1 was
  declared verified on CI run #19 (`4b5dddb`: ubuntu, macos and windows all
  success). Nothing in Phase 1 has been weakened since.
- **Contract:** `.ai/PHASE_2_DEPENDENCY_AWARE_INSPECTION.md`, commit `5609fdb`.
  It is normative for this phase and is quoted below where Phase 2 is executed
  against a specific clause.
- **Phase 2 commits:** `7b6288a` (interval accessor), `5e08ea6` (observe path),
  `3b43baf` (dependency graph), `a321d35` (planning), `76d0f0f` (CLI),
  `3898209` (interactive surface), plus the commit carrying this report.

---

## 2. What was implemented

| Contract clause | Artefact | Evidence |
| --- | --- | --- |
| §4.1 nodes are existing durable ids | `src/depgraph.rs` `NodeId` | operation/state/boundary ids only; no new identity, no new table |
| §4.2 three typed evidence kinds | `EvidenceKind::{StateLineage, EffectOverlap, TemporalOrdering}` | `src/depgraph.rs` |
| §4.2 Known vs Advisory | `EvidenceKind::confidence` | only `StateLineage` is `Known` |
| §4.3 unknown ≠ absent | `IncompleteEvidence`, `EdgeUnknown` reporting | `src/depgraph.rs` build step |
| §5 cycles reported, never broken | `detect_cycles` | 4 cycle unit tests incl. self-edge and two independent cycles |
| §6.3 closure on known lineage only | `plan::plan_rollback` closure loop | `one_undo_target_plans_the_whole_newer_suffix_newest_first` |
| §6.4 refusal conditions | `BlockReason` (10 variants), `RollbackConflict` (10 variants) | `src/plan.rs` |
| §7 conflicts are structured data | same | unit-testable without a terminal; rendered by CLI and UI |
| §8 plan first, execute second | `plan::execute` | re-plans, refuses stale plans, delegates to Phase 1 |
| §9 empty effects ≠ no change | `effect_evidence_available` | `a_capture_failed_operation_in_the_closure_blocks_the_plan` |
| §10 determinism | `normalise`, `(created_at, id)` ordering | shuffled-input equality tests on graph and plan |
| §11 minimal additive API | `Catalog::unknown_intervals`, `ScanOptions { ingest }`, `Workspace::observe`, `Cas::hash_file` | no schema change |
| §12 CLI surface | `inspect graph`, `inspect history`, `plan rollback`, `apply rollback`, `ui` | live walkthrough below |
| §13 minimum interactive surface | `src/ui.rs` | pure `parse`/`render`; mutation verbs refused by construction |
| §14 testing obligations | `tests/phase2_dependency.rs` + unit tests | see §5 |
| §15 performance measured | see §6 | measured, no SLA claimed |

---

## 3. Conformance matrix

| # | Contract obligation | Status | Evidence |
| --- | --- | --- | --- |
| 1 | Study the repository before writing code | done | three read-only surveys; findings cited in the contract §2 |
| 2 | Establish the Phase 2 contract before coding | done | `5609fdb` |
| 3 | Do not pretend causality exists | done | typed evidence; advisory edges never a step |
| 4 | Design the graph around the existing state model | done | nodes are existing ids; no parallel table |
| 5 | Multi-select rollback is safety-first | done | closure + refusal before any mutation |
| 6 | Cycles handled explicitly | done | detection, reporting, refusal when in closure |
| 7 | Unknown intervals are hard boundaries | done | open interval refuses unconditionally |
| 8 | Conflicts become structured data | done | typed enums, `--json` output |
| 9 | Plan first, execute second | done | separate `plan` and `apply` commands |
| 10 | Do not break single-writer semantics | done | execution goes through Phase 1 `undo`/`redo` |
| 11 | Small, real domain API | done | `depgraph` + `plan` modules |
| 12 | CLI first, TUI second | done | CLI shipped before the interactive surface |
| 13 | TUI scope is presentation only | done | `ui.rs` cannot decide or execute |
| 14 | Testing requirements | done | see §5 |
| 15 | Determinism requirement | done | see §5, `normalise` + explicit tie-breakers |
| 16 | Performance: measure, do not promise | done | see §6 |
| 17 | Documentation / state handoff | done | this report + `CURRENT_STATE`, `HANDOFF`, `CHANGELOG` |
| 18 | CI / verification gate | **blocked on push** | see §13 |
| 19 | Root-cause discipline | done | two test defects and one planner defect found and fixed at the root, §8 |
| 20 | Commit discipline | done | seven commits, one purpose each |
| 21 | Final report with a verdict | done | this document |

---

## 4. Phase 1 invariants verified unchanged

| Invariant | Enforcement | Phase 2 test |
| --- | --- | --- |
| Post-hook stays backgrounded; shell never waits | `integration/rewind.bash`, `rewind.zsh` | `shell_integration` suite unchanged and green |
| `HOOK_BUDGET_MS` stays deleted; `HOOK_SCAN_DEADLINE_MS = 50` bounds only the scan | `src/cli.rs` | `boundary_correlation` 10/10 |
| Boundary identity explicit and immutable | `src/db.rs` trigger `passive_boundary_consume_once` | `boundary_correlation` |
| Failed capture never becomes trusted | `record_capture_failure` → `ReconciliationRequired` + unknown interval | `foundation::failed_capture_...` |
| Unknown intervals stay unknown | `unknown_intervals` unchanged; Phase 2 only reads | planner refuses, never closes an interval |
| Writer lease untouched | `WorkspaceLease` | `planning_never_touches_the_workspace_or_the_cas` |
| One mutation engine | `rollback::execute_transition` private; `plan::execute` calls `undo`/`redo` | `an_approved_plan_executes_through_the_phase_1_engine` |
| Strong capture still ingests into the CAS | `ScanOptions::ingest` default `true` | `observe_never_ingests_objects_into_the_cas` asserts scan still ingests |

No Phase 1 file's behaviour changed except by addition: `ScanOptions` gained a
field whose default preserves every existing call site (there was exactly one,
`src/workspace.rs:260`), and `Catalog` gained one read-only `SELECT`.

---

## 5. Tests

**Full local suite: 76 passed, 0 failed** (`cargo test --all-targets
--all-features`, x86_64-pc-windows-gnu).

| Target | Tests | Note |
| --- | --- | --- |
| lib unit (`depgraph` 8 + `ui` 5) | 13 | cycles, determinism, confidence, UI parsing |
| `boundary_correlation` | 10 | Phase 1, unchanged |
| `foundation` | 13 | Phase 1, unchanged |
| `hardening` | 8 | Phase 1, unchanged |
| **`phase2_dependency`** | **15** | new |
| `rollback_tree` | 9 | Phase 1, unchanged |
| `shell_integration` | 8 | Phase 1, unchanged |

Phase 2 added 25 tests (13 unit + 12 here, then 3 more exception-path tests).
What they prove, beyond "it runs":

- planning performs **zero** mutation: the workspace tree and the CAS object set
  are compared before and after.
- the closure is the whole newer suffix, newest first, each step carrying
  `Selected` or `RequiredBy`.
- an open unknown interval refuses the plan and names the interval.
- a refused plan executes nothing; an approved plan executes through the Phase 1
  engine and returns the workspace to `HEALTHY`.
- a stale plan is refused after history moves on.
- an already-undone operation is not an eligible target.
- a capture-failed operation inside the closure blocks the plan, and its empty
  effect list is reported as missing evidence.
- duplicate targets collapse to one plan.
- an empty history builds an empty graph, and no targets is an error.
- graph and plan JSON are byte-identical for shuffled versus ordered input.

---

## 6. Exception-path record

| Scenario | Observed behaviour |
| --- | --- |
| Failure — target not eligible | refused, `TargetNotEligible { status, reversibility, direction }`, exit 3 |
| Failure — unknown operation | exit 1, `not found: operation 99` |
| Failure — live state unreadable | `LiveStateUnavailable` conflict and block reason; the mismatch question is left **unanswered**, not answered negatively |
| Failure — workspace not healthy / unfinished transaction | `WorkspaceNotHealthy`, `RecoveryRequired` block reasons |
| Empty — no operations | empty graph, no edges, no cycles; `plan` with no targets is `InvalidCommand` |
| Empty — empty UI line | renders help |
| Empty — operation with no effects (`BOUNDARY_ONLY`/`CAPTURE_FAILED`) | reported as missing evidence, never as "no dependency" |
| Partial — operation with `UNAVAILABLE` reversibility in the closure | `NotReversible` block; exercised via a real `CAPTURE_FAILED` operation |
| Partial — `PARTIALLY_REVERSIBLE` | **not exercised: no producer of that value exists anywhere in `src/`** (a Phase 1 gap, out of Phase 2 scope); the planner handles the value if one ever appears |
| Duplicate — same target repeated | collapses to one target; plan identical to the single-target plan |
| Duplicate — same operation as undo and redo | `ConflictingTargets` |
| Retry — planning repeated | identical output (byte-identical JSON); planning is pure |
| Timeout | not applicable: planning has no deadline or background work. The only bounded operation in the project is the hook scan deadline, untouched by Phase 2 |
| Mixed directions | refused before the live-state scan, with two structured conflicts |

No scenario degraded silently or lost state.

---

## 7. Performance measurements

Measured on `x86_64-pc-windows-gnu`, **debug build**, workspace with **40
recorded operations** and 40 files:

| Operation | Run 1 | Run 2 | Run 3 |
| --- | --- | --- | --- |
| `rewind inspect graph` | 343 ms | 239 ms | 189 ms |
| `rewind plan rollback --undo 1` | 737 ms | 725 ms | 674 ms |

The plan for that workspace contains **40 steps** and was `EXECUTABLE`. Planning
is dominated by the read-only live-state observation (hashing the workspace),
not by graph construction. No latency SLA is claimed, because the repository has
no justified contract for one. These numbers are a debug build; they are
recorded as a baseline, not as a guarantee.

---

## 8. Decision ledger

Every decision that interprets or narrows the contract, with the evidence that
forced it.

| # | Decision | Rationale | Evidence |
| --- | --- | --- | --- |
| D1 | Commit the contract before any code | charter §2, §20 | `5609fdb` |
| D2 | No schema change; intervals exposed with a read-only `SELECT` | the crate has no migration mechanism, so a new column or table would silently not apply to existing catalogs (contract §2.2.1) | `7b6288a` |
| D3 | `ScanOptions.ingest` defaults to **true** with a hand-written `Default` | a derived `Default` would have made `false` the default and silently disabled CAS ingestion for any future `Default::default()` caller, weakening strong capture | `5e08ea6` |
| D4 | `Cas::hash_file` mirrors `put_file`'s streaming BLAKE3 exactly | `observe` and `scan` must agree on `state_id`, otherwise planning would compare against a different identity than capture produced | `observe_never_ingests_objects_into_the_cas` |
| D5 | Only state lineage is `Known` | it is entailed by a shared content hash; effect overlap and temporal order are correlations and could reorder a closure incorrectly | contract §4.2 |
| D6 | Advisory edges never enter the closure | safety-first: a heuristic must not cause a mutation | contract §4.2 |
| D7 | **An open unknown interval blocks unconditionally** (changed after a failing test) | the first rule blocked only when the closure reached into the interval's time window. The test proved that lets an unreconciled gap through when the gap opened **after** the operations. If the trusted baseline does not cover the present, every rollback is undecidable — which is the same conclusion Phase 1's healthy gate reaches | `an_open_unknown_interval_refuses_the_plan` |
| D8 | `reaches` split into `reaches_known` / `reaches_any` | one method conflated the closure relation with context reachability; a test showed a known-only query traversing an advisory edge | `reachability_respects_confidence` |
| D9 | Lineage does not synthesise self-edges | an operation whose pre- and post-state are identical is a no-change capture, not a self-dependency. A self-edge supplied directly is still detected as a cycle | contract §5; `detects_a_self_edge` |
| D10 | Execution moved behind a separate `apply` command | keeps planning and mutation apart in the CLI, not only in the library, so no plan can be produced and executed in one unexamined step | contract §8 |
| D11 | The interactive surface renders its own compact text rather than sharing the CLI's formatters | avoids destabilising just-verified CLI output; the *decision* still comes from the same domain API, which is what §13 requires | `src/ui.rs` |
| D12 | Mixed directions are refused in one plan | Phase 2 executes one direction per plan; combining them would need two transitions whose ordering is not specified by the contract | `ConflictingTargets` |
| D13 | The `PARTIALLY_REVERSIBLE` case is documented as unexercised, not tested | no producer of that value exists in `src/`; fabricating one would test a fiction | §6 |
| D14 | The `Fathom Information Design` presentation style applies to the *delivery report*, not to the crate source | the charter forbids scope creep into the repository; presentation style has no bearing on the code | this report §13 |

Two test defects and one planner defect were found and fixed at the root rather
than worked around: the test that wrote its script files inside the workspace
(breaking the state chain it was checking), the test that assumed `init` ingests
objects from an empty directory, and D7 above.

---

## 9. Known limitations

- **CI has not run.** No Phase 2 commit has been pushed. This is the only
  blocker to the phase verdict.
- `PARTIALLY_REVERSIBLE` and `TrackingConfidence::Unsupported` remain
  producerless in `src/`; the planner handles their values but no test can
  create one honestly.
- The closure is computed on lineage order only. An advisory-only relation is
  reported but never acted on, so a genuinely causal relation that Phase 1 did
  not record as state lineage will not widen a closure. This is deliberate: the
  alternative is acting on a heuristic.
- `inspect graph` is unbounded: a large history produces a large JSON document.
  The contract defers bounding to a later decision (§17).
- An operation whose `pre_state_id`/`post_state_id` is dangling is reported as
  incomplete evidence; the planner's `MissingHistoricalState` covers the closure
  case, and `DependencyUnknown` covers the graph case.
- Planning measures ~0.7 s for 40 operations in a debug build, dominated by the
  live-state observation. No optimization was attempted (contract §15).
- SQLite WAL sidecar files may be created in the store when a catalog is opened.
  The read-only claim is therefore asserted over the workspace tree and the CAS
  object set, and stated plainly rather than overclaimed.

---

## 10. Risk register

| Risk | Owner | Severity | Trigger | Mitigation |
| --- | --- | --- | --- | --- |
| CI fails on macOS or Windows for Phase 2 | repo owner | high | first push of a Phase 2 commit | every gate step is platform-neutral; `observe`/`plan` touch no platform-specific API; if it fails, diagnose at the root as in Phase 1.4 |
| A heuristic is later promoted to a fact | future contributor | high | someone treats `EffectOverlap` as a dependency | confidence is in the type, not a field; `reaches_known` is the only closure relation; §4.2 states the rule |
| An unknown interval is treated as empty by a later feature | future contributor | high | new planner code ignores `unknown_intervals` | the accessor exists and the refusal is tested; D7 records why it is unconditional |
| Planning cost grows with history | repo owner | medium | many operations, unbounded `inspect graph` | measured baseline recorded; bounding deferred by contract §17 |
| The interactive surface grows into a second decision path | future contributor | medium | UI starts calling mutation APIs | `ui.rs` has no mutation import and refuses the verbs by construction; a test asserts it |
| Push remains unavailable from the agent shell | repo owner | low | every push needs a human terminal | documented in §12 |

---

## 11. Rollback and recovery runbook

**Returning the repository to the last approved phase state (Phase 1, `aff0dbb`):**

1. Confirm the working tree is clean: `git status --short` (expected: empty).
2. Identify the Phase 2 commits: `git log --oneline aff0dbb..HEAD` (expected: the
   eight commits listed in §1).
3. Revert them as one unit, preserving history:
   `git revert --no-commit aff0dbb..HEAD && git commit -m "revert Phase 2"`.
   Phase 2 is additive, so a revert removes `src/depgraph.rs`, `src/plan.rs`,
   `src/ui.rs`, `tests/phase2_dependency.rs` and the added CLI variants without
   touching Phase 1 logic.
4. Verify the revert: `cargo fmt --all -- --check`, `cargo check --all-targets
   --all-features`, `cargo clippy --all-targets --all-features -- -D warnings`,
   `cargo test --all-targets --all-features` → expect **48 passed, 0 failed**
   (the Phase 1 count).
5. If CI is required for the revert, push and confirm run green on all three
   platforms.

**Trigger authority:** the repository owner. **Expected duration:** the revert
itself took one second in a scratch clone; verification dominates (the full
suite runs in a few minutes). **Exercised: yes, end-to-end** on 2026-09-15 in a
scratch clone of the repository at `2fd0bf8`: `git revert --no-commit
aff0dbb..HEAD`, committed as one revert commit (`c4a4830` in that clone). The
revert removed 3,529 lines across 15 files (`src/depgraph.rs`, `src/plan.rs`,
`src/ui.rs`, `tests/phase2_dependency.rs` and the CLI/API additions), and the
full suite on the reverted tree returned exactly the Phase 1 count: **48
passed, 0 failed**. Phase 2 is additive by construction; this is the executed
proof.

**Recovery of the workspace under rollback (product-level):** `rewind recover`
for a completable unfinished transaction, `rewind recover --reconcile` only when
classification fails — unchanged from Phase 1 and still the only documented exit.

---

## 12. Handoff pack

- **Repository:** `D:\The RewindUndo Project` → `github.com/OG-Huzzi/Rewind`,
  branch `main`.
- **Build:** `cargo build` (Rust stable; `rusqlite` is bundled, so no system
  SQLite is required). **Test:** `cargo test --all-targets --all-features`
  (expect 76 passed). **Lint:** `cargo clippy --all-targets --all-features -- -D
  warnings`.
- **Manual prerequisites:** none beyond a Rust toolchain. `bash` is needed for
  the shell-integration tests (present via Git for Windows on this host); `zsh`
  is exercised only on macOS/Linux CI.
- **Environments:** local Windows only for this phase. CI is GitHub Actions on
  ubuntu-latest, macos-latest, windows-latest (MSVC) — unchanged workflow.
- **Credentials:** none required to build, test or run. The agent shell **cannot
  push** (Git Credential Manager cannot use its store from this process); pushes
  are performed by the repository owner.
- **Monitoring:** none. There is no daemon, service or watcher in Phase 2.
- **Run instructions:** `rewind inspect graph`, `rewind inspect history`,
  `rewind plan rollback --undo <id>... [--json]`, `rewind apply rollback --undo
  <id>...`, `rewind ui`. All planning and inspection commands are read-only.
- **Accountable owner after Phase 2:** the repository owner. No step in this
  phase relies on undocumented personal knowledge.

---

## 13. Verdict

**PHASE 2 NOT VERIFIED.**

Local gates: `cargo fmt --all -- --check` PASS, `cargo check --all-targets
--all-features` PASS, `cargo clippy --all-targets --all-features -- -D warnings`
PASS, `cargo test --all-targets --all-features` **76 passed / 0 failed**. Live
walkthroughs of every new command, including the refusal and error paths, were
performed on scratch workspaces.

The single remaining gap is CI. The charter requires the final verification to
be against the exact pushed commit on all three platforms, and explicitly
forbids declaring Phase 2 verified on local Windows results. Until the Phase 2
commits are pushed and green, the verdict stays as written.

No Phase 3, 4 or 5 functionality was introduced. No Phase 1 invariant was
weakened.

---

## 14. Review

The review was attempted by an independent agent twice; both attempts failed on
infrastructure (the first lost its execution context, the second failed its
model request), so no independent review artefact exists. That requirement is
recorded here as **UNMET**, not as passed.

In place of it, the four load-bearing claims were re-checked directly against the
source, with the lines quoted. This is AutoCoder's own verification and is
therefore **not** an independent review; it is recorded as such.

| Claim | Evidence | Result |
| --- | --- | --- |
| Only state lineage produces `Known` | `src/depgraph.rs:87-88` | holds |
| `reaches_known` cannot traverse an advisory edge | `src/depgraph.rs:431` | holds |
| The closure comes from operation order, not from advisory edges | `src/plan.rs:217` (`sort_by_key(|o| (o.created_at, o.id))`), `src/plan.rs:327` | holds |
| An open unknown interval blocks unconditionally | `src/plan.rs:387` (`filter(|interval| interval.is_open)`) | holds |
| `execute` re-plans and refuses a stale plan before mutating | `src/plan.rs:567-568`, then `src/plan.rs:578` | holds |
| `observe` hashes without writing to the CAS; `Default` still ingests | `src/scan.rs:148-151`, `src/scan.rs:35`, `src/scan.rs:45`, `src/workspace.rs:270` | holds |
| `src/ui.rs` contains no mutating call | zero matches for `plan::execute`, `rollback::`, `.scan(`, `insert_`, `set_workspace`, `open_unknown`, `run_command`; `src/ui.rs:98` refuses the mutating verbs | holds |

**Outstanding for a genuine independent review:** a second person (or a working
review agent) should re-read `.ai/PHASE_2_DEPENDENCY_AWARE_INSPECTION.md` and
README this package, and record a pass/fail per contract obligation with an
evidence link.
---

## 15. CI record for Phase 2

**Run #21** (commit `6014a1b`, the first pushed Phase 2 head): **ubuntu-latest
failed; macos-latest passed; windows-latest passed.**

The failure was a defect in the Phase 2 *tests*, not in the product. Three call
sites in `tests/phase2_dependency.rs` passed POSIX script bodies without a
`#!/bin/sh` shebang. `common::shell_script` writes the POSIX body verbatim and
marks it executable, so on Linux `execve` returned `Exec format error` (os error
8) for every test that ran a supervised command. macOS tolerated it and Windows
used the `.cmd` path, which is why only one platform failed.

Failing tests included `a_plan_is_deterministic`,
`a_capture_failed_operation_in_the_closure_blocks_the_plan` and
`a_refused_plan_executes_nothing`, plus their siblings in the same suite.

**Fix:** the three call sites now pass `#!/bin/sh\n…\n`, matching the convention
already used in `tests/foundation.rs` and `tests/rollback_tree.rs`. Test-only:
no product code changed, and the local gate stayed green before and after. The
POSIX path cannot be exercised on Windows at all — which is precisely why CI is
the authority for non-host platforms, and why this class of defect must be caught
there rather than argued away locally.

**Run #19** (`4b5dddb`) and **run #20** (`aff0dbb`) were both green on all three
platforms; these are the Phase 1.4 verification and its documentation commit.