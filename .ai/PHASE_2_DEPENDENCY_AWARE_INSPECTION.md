# Phase 2 — Dependency-Aware Inspection: Implementation Contract

Status: **contract only; no Phase 2 code exists yet.** This document is the
Phase 2 semantic contract required before implementation. It defines exactly
what Phase 2 means in this repository, what evidence a dependency may rest on,
what a plan is allowed to do, and where the phase stops.

Phase 1 (through 1.4) is the frozen safety foundation. CI run #19 on `4b5dddb`
is green on ubuntu-latest, macos-latest and windows-latest, and Phase 1 is
verified. Nothing in this document may weaken a Phase 1 invariant; where Phase 2
appears to need one relaxed, the conflict is to be reported, not resolved
silently.

---

## 1. Scope and phase boundary

Phase 2 delivers **dependency-aware inspection**: a dependency graph over
already-recorded history, safety-first multi-select rollback planning, structured
conflicts, and the smallest interactive surface that exercises the new semantics.

Explicitly out of scope: a background watcher or daemon (Phase 3); time-range
views, package recipes and ecosystem integrations (Phase 4); platform expansion
(Phase 5); any architectural redesign of Phase 1; unrelated clean-up.

The charter's own framing is the acceptance test: build intelligence **on top of**
the foundation without making the foundation less trustworthy.

---

## 2. What Phase 1 actually provides (surveyed, with citations)

The contract is grounded in the code, not the prose. All line references are to
the compiled state of `main` at `aff0dbb`.

### 2.1 Durable identities available for reuse

| Identity | Where | Nature |
| --- | --- | --- |
| Workspace | `workspaces.id` (`src/db.rs:93`) | uuid v4; the store is `<store>/projects/<id>/` (`src/workspace.rs:34`) |
| Operation | `operations.id` (`src/db.rs:109`) | `INTEGER AUTOINCREMENT`; the history ledger |
| Operation effects | `effects` (`src/db.rs:124`) | one row per changed path, typed `pre`/`post` fingerprints |
| State | `states.id` (`src/db.rs:100`) | BLAKE3 hex of the canonical manifest envelope (`src/model.rs:176`) |
| State lineage | `states.parent_id` (`src/db.rs:104`) | nullable, **no FK, no acyclicity check** |
| Baseline pointer | `workspaces.baseline_state_id` (`src/db.rs:97`) | the trusted current checkpoint |
| Snapshot | `snapshots.name` (`src/db.rs:175`) | **globally** unique, not per workspace |
| Boundary | `passive_boundaries.id` (`src/db.rs:143`) | one record per shell command; consume-once, DB-enforced (`src/db.rs:154`) |
| Unknown interval | `unknown_intervals` (`src/db.rs:133`) | open/closed by flag; **no Rust row type and no read accessor exist** |
| Transaction / journal | `transactions.id` (`src/db.rs:167`), `journals/<uuid>.json` (`src/journal.rs:21`) | 8-state lifecycle (`src/model.rs:405`) |
| Bypass marker | `bypass_markers` (`src/db.rs:160`), `boundaries.log` (`src/cli.rs:644`) | forces writer reconciliation |

**Decision:** Phase 2 introduces **no new durable identity**. Every node and every
edge refers to an existing id from this table. No parallel "graph table" of
synthetic operations.

### 2.2 Facts that constrain the design

1. **There is no schema-migration mechanism.** `Catalog::initialize` is one
   idempotent `CREATE TABLE IF NOT EXISTS` batch (`src/db.rs:88-182`); no
   `ALTER TABLE` or `user_version` exists anywhere in `src/`. Consequence: Phase 2
   may add **read accessors and Rust types** freely, but adding a *table* or
   *column* would silently not apply to existing catalogs. Phase 2 therefore adds
   no schema changes (see §11).
2. **`states.id` is content-addressed and globally unique** (`src/db.rs:101`,
   inserted with `INSERT OR IGNORE` at `src/db.rs:259`), and `snapshots.name` is
   globally unique (`src/db.rs:176`) while lookups are workspace-scoped
   (`src/db.rs:644`). Any Phase 2 view keyed on these must tolerate an id that is
   shared across workspaces, and must never assume it identifies a workspace.
3. **`Workspace::scan` is not read-only.** The scanner ingests every regular file
   into the CAS (`src/scan.rs:117` → `Cas::put_file`, `src/cas.rs:42`) before
   returning the manifest. A planner that wants a live comparison must not call it
   (see §11.2).
4. **Opening a workspace mutates the store.** `open_from_current` →
   `open_from_pointer` creates the store directory tree (`src/workspace.rs:35-40`)
   and runs `Catalog::initialize` (`src/workspace.rs:42`). "Zero side effects" in
   this contract therefore means *zero workspace and catalog mutations beyond this
   pre-existing open path*, which is stated explicitly rather than glossed over.
5. **`execute_transition` is private and is the only mutation engine**
   (`src/rollback.rs:176`). `undo`, `redo` and `restore_snapshot` are the only
   public mutation entry points, and all three take the writer lease, run
   `recover_locked`, enforce the pending safety gate, then `require_healthy`
   (`src/rollback.rs:28-31`, `76-78`, `121-124`). Phase 2 must reuse these and must
   not add a second engine.
6. **Only three error variants model a decision-level refusal**:
   `ConditionBlocked`, `RecoveryRequired`, `Conflict` (`src/error.rs:14`, `16`,
   `18`). Everything else is environment/integrity failure. The current CLI has no
   structured handling of them: they reach `main` and become one stderr line plus
   exit code 1 (`src/main.rs:7-10`).
7. **There is no TUI and no TUI dependency.** `Cargo.toml:9-20` lists only
   `blake3`, `clap`, `fs2`, `rusqlite`, `serde`, `serde_json`, `thiserror`, `uuid`
   and the dev-dependency `tempfile`. Nothing in `src/` reads stdin.

---

## 3. Non-negotiable constraints

Derived from §2, these bind every Phase 2 commit:

- **C1 — No new durable identity, no schema change.** No new table, column,
  trigger, or migration; no synthetic operation rows.
- **C2 — The graph is advisory; the catalog is authoritative.** A Phase 2 view may
  never override `WorkspaceCondition`, the unknown-interval model, the writer
  lease, or the journal/recovery state machine.
- **C3 — Planning is read-only and side-effect free** within the meaning of
  §2.2(3-4): it must not call `Workspace::scan` as it exists, must not acquire the
  writer lease, and must not call any `Catalog` mutator (§11.2 lists the permitted
  surface).
- **C4 — Exactly one mutation engine.** An approved plan is executed through the
  existing `undo`/`redo`/`restore_snapshot` paths, which keep the lease,
  recovery-first and healthy-gate ordering unchanged.
- **C5 — Uncertainty is never silently resolved.** An unknown interval, an
  advisory edge, a missing state, or an unsupported object changes the plan's
  decision; none may be treated as "probably irrelevant".
- **C6 — Determinism.** Identical catalog/state/evidence must yield an identical
  graph and an identical plan (see §10).
- **C7 — No Phase 3/4/5 functionality.** No daemon, no watcher, no recipes.

---

## 4. The dependency graph

### 4.1 Nodes

A node is exactly one durable record, tagged by kind:

```text
NodeId::Operation(i64)    // operations.id — the primary node kind
NodeId::State(String)     // states.id — lineage scaffolding, not a rollback target
NodeId::Boundary(String)  // passive_boundaries.id — evidence about who changed what
```

Operations are the only nodes a user may select as a rollback target.
States and boundaries are supporting nodes that exist so that evidence can point
at real ids instead of duplicated metadata.

### 4.2 Edges and what evidence may create one

An edge is directed `from → to` and means **"`to` follows from `from`"** in one of
three precisely-defined senses. The evidence kind is part of the edge; there is no
untyped edge.

| Evidence | Edge means | Derived from | Confidence |
| --- | --- | --- | --- |
| `StateLineage` | `to` began from the state `from` produced: `to.pre_state_id == from.post_state_id` | `operations` columns (`src/db.rs:113-114`) | **Known** |
| `EffectOverlap` | `from` and `to` changed at least one common path | `effects.path` for both operations (`src/db.rs:127`) | **Advisory** |
| `TemporalOrdering` | `from` immediately precedes `to` in the same session with no intervening state change | `operations.created_at` + `SessionId` on the boundary side | **Advisory** |

**Known** means the edge is *entailed by recorded durable identity* — the two
operations literally share a state id, and a state id is a content hash. An
advisory edge is a heuristic correlation. The distinction is structural, not a
severity field: it is carried in the type.

**An advisory edge is never a fact.** It is reported, it may be shown, and it must
never widen a rollback closure or cause a mutation. Concretely: `EffectOverlap`
means "these two commands touched the same file", which is not causality — the
file may have been rewritten by anything, including processes Rewind never saw.

### 4.3 Uncertainty representation

```text
EdgeConfidence ::= Known | Advisory
```

In addition, an edge may be **absent because the evidence is unavailable** — the
two failure modes are distinct and must stay distinct:

- `Unknown` — the edge *would* be computable but the inputs are missing or
  unusable. Example: a `Completed` operation whose `pre_state_id`/`post_state_id`
  is `NULL`, or whose referenced state row does not exist. This is `EdgeUnknown`
  and it is a **blocking** fact for planning, not an absent edge.
- `Absent` — the evidence was available and does not support an edge.

The graph must be able to report, for any queried pair or neighbourhood, which of
the two it is. Representing "I could not tell" as "no edge" is exactly the
inference Phase 2 is forbidden to make.

### 4.4 Authority

The graph is a **derived, rebuildable view** over the catalog. It is never
persisted as authority, and it is recomputed from the catalog on demand. If a
future phase wants to cache it, the cache must be invalidated by catalog change,
and this contract's determinism requirement still applies.

---

## 5. Cycles

The graph is **not assumed to be a DAG**. `EffectOverlap` and `TemporalOrdering`
are heuristics and can produce cycles even where the lineage chain cannot.

Rules:

- Cycle detection is mandatory and deterministic (stable node ordering, iterative
  DFS with an explicit stack; no recursion on unbounded input).
- A cycle is **reported as a structured conflict** naming every member node id in
  canonical order.
- A cycle is **never** broken, and no edge is ever dropped to make the graph
  sortable.
- If a cycle intersects the required closure of a rollback plan, the plan is
  refused. If it does not, the cycle is informational and the plan proceeds.
- Self-edges (`from == to`) are always a conflict, never silently dropped.

Required regression coverage (charter §6): simple cycle, longer cycle,
self-dependency, two independent cycles, cycle plus unknown interval, cycle plus
conflicting target.

---

## 6. Multi-select rollback: targets, closure, refusal

### 6.1 What multi-select is not

It is not "for each selected operation, call undo". Phase 1's `undo` requires the
live state to equal the operation's `post_state_id` (`src/rollback.rs:45-55`) and
marks the operation `Undone`. Undoing an older operation while a newer one stands
would silently discard the newer one's identity. The planner must make that
consequence explicit instead.

### 6.2 Target selection

A target is a set of `(operation_id, direction)` where direction is
`Undo` or `Redo`. Validity is evaluated with the same predicates Phase 1 uses,
read from the persisted record — never re-implemented as a different rule:

- Undo target: `status == COMPLETED && reversibility == FULLY_REVERSIBLE`
  (`src/rollback.rs:36-38`).
- Redo target: `status == UNDONE && reversibility == FULLY_REVERSIBLE`
  (`src/rollback.rs:83-85`).

A target failing its predicate yields a `RollbackConflict::TargetNotEligible`
carrying the observed status and reversibility — not a generic error.

### 6.3 Required closure

The required closure is computed on the **lineage chain only** (Known evidence):
starting from the oldest undo target, every operation that is newer in the
workspace's operation order and is still `COMPLETED` must also be undone first,
because the live state must walk back through them. Ordering is defined in §10.

Advisory edges **never** enter the closure (§4.2). They are surfaced in the plan
as context, explicitly marked `Advisory`, with the sentence that they were not
acted on.

### 6.4 Refusal conditions

A plan is refused, with a structured reason, when any of:

| Condition | Reason variant |
| --- | --- |
| an operation in the closure is `PARTIALLY_REVERSIBLE` / `UNAVAILABLE` / `UNSUPPORTED` | `BlockReason::NotReversible { operation_id, reversibility }` |
| the closure intersects an open unknown interval | `BlockReason::UnknownInterval { interval_id }` |
| a state, manifest or CAS blob the closure needs is missing or fails its hash | `BlockReason::MissingHistoricalState { state_id }` |
| the closure contains an unsupported filesystem object (`Fingerprint::Unsupported`) | `BlockReason::UnsupportedObject { path }` |
| the workspace condition is not `HEALTHY`, or a bypass marker is pending | `BlockReason::WorkspaceNotHealthy { condition }` |
| an unfinished transaction exists | `BlockReason::RecoveryRequired { transaction_id }` |
| targets conflict with each other (below) | `BlockReason::ConflictingTargets { .. }` |
| a cycle intersects the closure | `BlockReason::CycleInClosure { nodes }` |
| the live state does not match the newest target's expected state | `BlockReason::LiveStateMismatch { expected, found }` |

**Conflicting targets** are defined precisely: two targets conflict if one is a
descendant of the other in the lineage chain *and* their directions differ, or if
the same operation id appears twice with different directions. Two undo targets
where one is newer than the other is **not** a conflict — the newer one is simply
absorbed into the closure of the older.

### 6.5 Overlapping dependencies

Overlapping closures are a normal case, not an error: the closure is the union,
and the shared operations appear once. The plan lists, per included operation, the
target(s) that required it, so "why is this being undone?" is always answerable.

### 6.6 Unknown intervals are hard boundaries

If the closure would traverse an interval whose evidence is unknown, the planner
must **not** treat it as empty and must not assume the interval is irrelevant. It
reports `BlockReason::UnknownInterval`, names the interval, and states why a
deterministic decision is impossible. The distinction survives end-to-end: it is
a typed value from the planner through the CLI to the exit code.

---

## 7. Conflicts as structured data

Conflicts are domain values, not printed strings. The CLI and any future
interactive surface render the same values.

```text
RollbackConflict ::=
    TargetNotEligible { operation_id, status, reversibility }
  | ConflictingTargets { operation_ids, detail }
  | DependencyUnknown { operation_id, missing: EvidenceInput }
  | UnknownInterval { interval_id, start_state, end_state }
  | UnsupportedObject { operation_id, path, descriptor }
  | MissingHistoricalState { operation_id, state_id }
  | IncompatibleOrdering { operation_ids }
  | CycleDetected { nodes }
  | LiveStateMismatch { expected_state_id, found_state_id }
```

Each conflict must be distinguishable programmatically, must carry the durable ids
that caused it, and must be constructible in a unit test without running the CLI.
Direct conflicts (this operation cannot be undone), dependency conflicts (the
closure cannot be satisfied), unknown-evidence conflicts, unavailable-evidence
conflicts, unsupported objects, and reconciliation-required states are **separate
variants** — the charter requires that they be told apart, and a single
`Conflict(String)` would not do it.

The existing `RewindError` is untouched. Phase 2 adds its own planning types and
maps to `RewindError` only at the boundary where the existing engine consumes
them.

---

## 8. Plan first, execute second

```text
RollbackPlan {
    workspace_id,
    targets:            Vec<RollbackTarget>,
    order:              Vec<PlannedStep>,      // deterministic, see §10
    included:           Vec<IncludedOperation>, // why each operation is in the closure
    advisory_context:   Vec<DependencyEdge>,    // heuristics, explicitly not acted on
    conflicts:          Vec<RollbackConflict>,
    block_reasons:      Vec<BlockReason>,
    unknowns:           Vec<UnknownFact>,       // what could not be determined
    evidence_summary:   Vec<EdgeEvidenceRecord>,
    decision:           PlanDecision,           // Executable | Refused { reasons }
}
```

The plan must answer, without executing anything: what will change, why it is
required, which dependency caused it, what conflicts exist, what evidence supports
each step, what remains unknown, whether execution is permitted, and in what order
steps will occur.

Planning is pure with respect to the workspace: same inputs, same plan; no
mutation; no lease; no UUID generation; no clock reads that affect the decision.

Execution is a separate step that re-validates and then delegates to the Phase 1
engine. **Re-validation is mandatory**: between planning and execution the
workspace may change, so execution re-checks the condition gate, `recover_locked`,
the live-state match, and each target's eligibility, exactly as `undo` does today.
A plan is a proposal, never a capability token.

---

## 9. Empty evidence is not empty

A recurring trap this contract closes explicitly:

- `BoundaryOnly` and `CaptureFailed` operations always carry `effects: Vec::new()`
  (`src/cli.rs:585`, `src/workspace.rs:477`). An empty effect list therefore does
  **not** mean "this command changed nothing" for those kinds.
- Consequently, the planner must treat those kinds as *evidence-unavailable* for
  `EffectOverlap` purposes (`DependencyUnknown`), never as "no paths shared".

This is the same rule as §4.3, applied to the operation level, and it is the
single most likely source of a bogus "independent" conclusion.

---

## 10. Determinism

Given the same catalog, states, history and evidence, the graph and the plan are
identical. Concretely forbidden as ordering inputs: `HashMap`/`HashSet` iteration
order, directory enumeration order, thread scheduling, and un-tie-broken
timestamps. Random UUIDs are never generated during planning.

Explicit tie-breakers, in order:

1. Operation order is `(created_at, id)` ascending — `id` is
   `INTEGER AUTOINCREMENT` and is therefore a stable total order within a
   workspace even when two operations share a timestamp.
2. Node ids are compared by their canonical string form.
3. Closure order is the reverse of (1): newest-first for undo, oldest-first for
   redo.
4. Conflict lists are sorted by (variant, primary id); advisory edge lists by
   `(from, to, evidence kind)`.

Required test: construct equivalent history in different insertion orders and
assert byte-identical plan serialization.

---

## 11. New API required (minimal and additive)

### 11.1 Read accessor for unknown intervals

The `unknown_intervals` table has no read accessor: only the boolean
`Catalog::has_open_unknown` exists (`src/db.rs:449`). A planner must name the
interval that blocks it, so Phase 2 adds:

```text
struct UnknownIntervalRow { id: i64, start_state_id: Option<String>,
                            end_state_id: Option<String>, reason: String,
                            is_open: bool, created_at: i64, closed_at: Option<i64> }
Catalog::unknown_intervals(workspace_id) -> Result<Vec<UnknownIntervalRow>>
```

Read-only `SELECT`; no schema change; no behaviour change to existing callers.

### 11.2 A side-effect-free scan for conflict prediction

Planning needs "does the live state equal `expected_state_id`?" — the check
`undo` performs by calling `Workspace::scan`, which ingests into the CAS
(`src/scan.rs:117`). Phase 2 therefore separates hashing from ingestion:

```text
ScanOptions { deadline, ingest: bool }   // ingest defaults to true → Phase 1 unchanged
Workspace::observe(&self, deadline) -> Result<ScanResult>   // ingest = false
```

The manifest, and therefore `state_id`, are computed by exactly the same code
path; only the CAS write is skipped. This preserves every Phase 1 invariant (the
strong-capture path still ingests) and makes the read-only claim of §3(C3) true
rather than aspirational. A test must assert that `observe` leaves the CAS
directory byte-identical.

### 11.3 Everything else is existing surface

The planner reads only: `Workspace::{row, condition, baseline_id, state_manifest,
status}`, the `Catalog` read accessors listed in the survey, `JournalStore::{load,
unfinished, pending_archives}`, `Cas::{verify, read_bytes}`, and `diff_manifests`.
The forbidden set (`Workspace::scan`, `persist_state`, `reconcile_locked`,
`ensure_healthy_locked`, `enforce_pending_safety_gate`, `record_capture_failure`,
`run_command`, `create_snapshot`, `rollback::*` mutators, `Catalog` mutators,
`Cas::put_*`, `JournalStore::write`) is listed in the Phase 2 test suite as a
documented boundary.

---

## 12. CLI surface

New read-only commands, wired into the existing clap enum and the single `run()`
dispatch (`src/cli.rs:47-92`, `src/cli.rs:121`):

```text
rewind inspect graph   [--json] [--operation <id>]
rewind inspect history [--json] [--limit <n>]
rewind plan rollback   --undo <id>... [--redo <id>]... [--json]
```

Output rules:

- Human text on stdout, diagnostics on stderr — consistent with the existing
  convention and with the hook's stdout purity contract (`src/cli.rs:415-430`),
  which must not be disturbed.
- `--json` emits the serialized plan/graph and is the **interface of record** for
  machine consumers; `serde::Serialize` on the new types is therefore required.
- Exit codes: `0` plan is executable and nothing failed; `3` planning completed
  and the plan is **refused** (matching the existing "blocked / needs operator
  action" convention at `src/cli.rs:168`, `src/cli.rs:189`, and `doctor`'s
  accumulated `result` at `src/cli.rs:307`, returned at `src/cli.rs:384`); `1` an
  unexpected error. A refusal is not an error and must not be reported as one.
- `inspect` and `plan` never take the lease and never mutate (§3 C3).
- No new safety information may be hidden behind an interactive surface: whatever
  the CLI prints must be sufficient to understand the decision.

---

## 13. Minimum interactive surface

The interface is **not** the source of truth, and it is the last thing built.

Minimum Phase 2 interactive surface, and nothing more: a read-only, line-oriented
navigator that renders the same `RollbackPlan`/graph objects as the CLI, over
stdin/stdout, with no new dependency. It may page operations, show a node's
closure and its evidence, and print a plan. It must not execute anything, and a
non-interactive caller must receive the identical decision from the same domain
API.

Concretely: no `ratatui`, no `crossterm`, no alternate-screen rendering, no
themes, no colour work, no animation. Charter §13 requires that the TUI be
presentation only; the cheapest honest way to satisfy that is to keep it
structurally incapable of deciding anything — it formats values it is given.

If the domain model and the CLI planning path are not fully green, the interactive
surface is not built at all, and that is an acceptable Phase 2 outcome to report.

---

## 14. Testing obligations

All planning tests run without a terminal and without the CLI, against the domain
API. Builders in `tests/common/` are extended, not duplicated.

Graph construction: isolated node; linear chain; branching; converging
dependencies; cycle; longer cycle; self-edge; duplicate edge; mixed Known/Advisory
evidence; `EdgeUnknown` from a `NULL` or dangling `pre_state_id`.

Planning: single target; multiple independent targets; overlapping closures; shared
dependency; conflicting targets; unknown interval in closure; unsupported state in
closure; missing historical state; cycle in closure; deterministic ordering;
identical plan under shuffled insertion order.

Safety: planning performs zero workspace and catalog mutation (CAS directory and
catalog file compared byte-wise before/after); a refused plan performs no mutation;
unknown evidence never becomes trusted; refusal precedes mutation; the writer lease
and recovery gates still bind; an approved plan executes through the Phase 1 engine
(asserted by observing a real journal, not by inspecting the plan).

End-to-end: real state created → history recorded → graph built → multiple targets
selected → deterministic plan built and inspected → plan executed → filesystem
verified → journal/recovery semantics verified intact. Plus negative end-to-end
cases where planning correctly refuses.

Verification gate for the phase: `cargo fmt --all -- --check`, `cargo check
--all-targets --all-features`, `cargo clippy --all-targets --all-features -- -D
warnings`, `cargo test --all-targets --all-features`, repeated runs of the
concurrency-sensitive tests, and green CI on ubuntu-latest, macos-latest and
windows-latest **for the exact pushed commit**. Local Windows results alone never
justify "Phase 2 verified".

---

## 15. Performance

Graph construction and planning are measured on a representative workspace and the
measurements are recorded. No latency SLA is claimed, because the repository has
no justified contract for one; the Phase 1 restore baseline remains a measured
limitation and is not repaired here. Optimization may never trade away
correctness, and no optimization work is in scope for this phase unless a
measurement shows planning is unusable.

---

## 16. Commit plan

1. This contract.
2. `Catalog::unknown_intervals` read accessor + `UnknownIntervalRow`.
3. `ScanOptions { ingest }` + `Workspace::observe`, with the CAS-immutability test.
4. `src/depgraph.rs`: nodes, edges, evidence, confidence, cycle detection,
   deterministic ordering (+ tests).
5. `src/plan.rs`: targets, closure, conflicts, block reasons, plan
   (+ tests).
6. CLI `inspect` / `plan rollback`, including `--json` (+ tests).
7. Interactive surface (conditional on 4-6 being green) (+ tests).
8. Phase 2 verification documentation and the final verdict.

Each commit has one purpose; no unrelated refactors; nothing from Phase 3/4/5.

---

## 17. Decisions deferred / open questions

These are deliberately left open and must be resolved in the commit that needs
them, with the reasoning recorded:

- Whether `EffectOverlap` edges should be limited to operations whose
  `confidence` is at least `LOW_CONFIDENCE_OBSERVATION`, or whether all operation
  kinds may contribute (with `BoundaryOnly`/`CaptureFailed` excluded per §9).
- Whether `inspect graph` should be bounded by default (a large workspace's full
  edge set may be large) and what the default bound is.
- Whether the plan should expose a re-validated "execute this exact plan" token,
  or whether execution always re-plans from the same targets (current contract:
  re-plan-and-re-validate; a token would be a new authority concept and is
  out of scope).
- Whether the interactive surface is built at all in this phase (§13).

---

## 18. What would make Phase 2 fail

Recording these so they cannot be quietly rationalised later:

- A dependency edge treated as fact when its evidence is a heuristic.
- An unknown interval treated as empty because the graph could not model it.
- A closure that silently widens or narrows to make a plan executable.
- A second rollback engine, or a plan that bypasses the lease, recovery-first,
  or the healthy gate.
- A cycle broken by dropping an edge.
- Non-deterministic ordering inside a plan.
- Any Phase 1 invariant weakened to make the new UX nicer.

If Phase 2 needs an architectural change that would affect a Phase 1 invariant,
the work stops and the conflict is documented here rather than resolved silently.
