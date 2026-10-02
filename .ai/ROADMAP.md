# Engineering Roadmap

Status: Phases 1–7 release preparation implemented and CI-verified. Public
publishing remains blocked until the owner supplies a license file and
matching Cargo metadata. See `.ai/PHASE_7_RELEASE.md`.

## 1. Phase strategy

The project advances only after the current phase has an explicit semantic
contract. Phase 1 contains enough degradation, reconciliation, fingerprint,
rollback, and recovery behavior to be correct without a future daemon or DAG.
Later phases improve coverage and ergonomics; they do not repair a missing
Phase 1 safety invariant.

## 2. Phase 1 - Foundation

Phase 1 will implement, after independent verifier approval:

- explicit workspace identity and external persistent store;
- canonical typed filesystem manifests and CAS ingestion;
- strong rewind run pre/post capture;
- passive boundary observation with degradation gates;
- full reconciliation and unknown-interval history;
- single-writer rollback/redo/restore;
- same-filesystem local quarantine and post-commit external archive;
- physical state-map crash recovery;
- Linux/macOS/Windows capability-specific behavior;
- status, inspection, and refusal diagnostics.

The final contract is phases/phase-01-foundation.md.

## 3. Phase 2 - Dependency-aware inspection

After Phase 1 semantics are stable, investigate causal dependency graphs,
multi-select rollback, richer conflict presentation, and an interactive
interface. These features may not weaken the unknown-interval or conflict
rules.

## 4. Phase 3 - Continuous observation

Investigate a background watcher/indexer. Watchers remain advisory because
overflow, coalescing, missing process attribution, and offline gaps still
require full reconciliation.

## 5. Phase 4 - Time and ecosystem integrations

Investigate time-range views and package-specific recipes only after the core
state model can represent their limitations. Remote and system-wide side
effects remain outside local rollback.

Delivered (2026-09): the read-only time-range view `rewind inspect timeline`
(half-open ranges, evidence tiers, explicit uncertainty) per the Phase 4
contract; package-specific recipes evaluated and **deferred** (ADR-016).
Range-based restore and causal attribution remain excluded.

## 6. Phase 5 - Platform expansion

Expand object and metadata support only with platform-specific tests and an
updated capability matrix. A new platform is not considered supported merely
because a common API name exists.

Delivered (2026-09): POSIX named pipes (FIFOs) as supported objects with
real POSIX CI tests, precise unsupported-object descriptors, and the
object×platform capability matrix in `.ai/PHASE_5_PLATFORM_EXPANSION.md`.
Sockets, device nodes, junctions, xattrs/ACLs, and ownership remain
unsupported or deferred as documented there.

Amendment slice 2 delivered (2026-09): Windows junctions are first-class
literal-leaf objects — recorded reparse data restored byte-faithfully and
never followed — per `.ai/PHASE_5_WINDOWS_JUNCTIONS.md` (ADR-019); the
capability matrix is amended there and in the slice-1 contract §3.

Amendment slice 3 delivered (2026-09): Windows alternate data streams
(named `$DATA` streams on regular files) are captured, restored, verified,
quarantined, and archived as part of the file's fingerprint — per
`.ai/PHASE_5_ALTERNATE_DATA_STREAMS.md` (ADR-020); directory-attached
streams remain documented non-state, and xattrs/ACLs/ownership/sparse/
chflags remain deferred with the recorded feasibility blockers.

Amendment slice 4 delivered (2026-09): explicit NTFS DACL ACEs (plus the
protected flag) on regular files are captured, restored, verified,
quarantined, and archived as part of the file's fingerprint — per
`.ai/PHASE_5_NTFS_DACL.md` (ADR-021); directory DACLs, owner/group, SACL,
sparse layout, and chflags remain deferred, and POSIX xattrs remain
deferred with the recorded no-POSIX-probe-host blocker.

Post-roadmap measurement-driven phase (2026-09): rollback step verification
narrowed to the affected path (ADR-018) after profiling showed per-step full
scans were 82-84% of undo time with quadratic scaling — 400-file undo
15.8 min → 1.25 min, identical safety anchors (`.ai/PHASE_ROLLBACK_PERF.md`).

## 7. Phase 5 close-out and current gate

Phase 5 is **closed** (signoff: `.ai/PHASE_5_VERIFICATION_REPORT.md`): four
capability slices delivered and CI-verified, the capability matrix is final
for this phase, and every remaining metadata/platform item is explicitly
deferred with its recorded blocker. The close-out phase
(`.ai/PHASE_5_CLOSEOUT.md`) also root-caused the three CI timing flakes and
fixed them deterministically (hook lease-retry tuning knob, claim barrier,
spawn sentinel, capture diagnostics) without weakening any assertion.

## 8. Phase 6 - ecosystem recipes (manifest/lockfile evidence layer)

Delivered (2026-10): the ADR-016 unlock condition, implemented narrowly per
`.ai/PHASE_6_RECIPES.md` (ADR-022). Strong capture now records, per changed
lockfile, the recipe kind, the exact root-relative lockfile path, the
pre/post CAS hashes (or absent), and the recognized manifest paths — derived
purely from the recorded manifests (no lockfile parsing, no network, no new
CAS objects), stored as additive operation metadata in an idempotently
created `operation_evidence` table, and rendered only on surfaces for stores
that contain it. What ADR-016 rejected remains rejected: no package
transactions, no registry/cache/global-state claims, no per-version
semantics. Still deferred: every unprobed ecosystem (yarn, bun, poetry,
Pipfile, Gemfile, composer, …), monorepo sub-project lockfiles (root-only
depth policy), and any restore or re-run semantics for package actions.

## 9. Phase 7 - Release engineering

Preparation delivered: accurate package metadata and crate allowlist, an
evidence-linked user README, complete CLI help descriptions, and a
tag-triggered three-platform release workflow. Local release-preparation
gates pass; the full 188-test suite passed twice consecutively, with an
earlier unexplained Windows GNU `STATUS_HEAP_CORRUPTION` retained in the
test record. CI run #65 on commit `6591600` is green on Ubuntu, macOS, and
Windows ([run details](https://github.com/OG-Huzzi/Rewind/actions/runs/37026986787)).

The GitHub Release is blocked until the owner supplies legal terms and an
actual license file. Do not create the intended `v0.1.0` annotated tag or
publish artifacts before Cargo metadata names that license and the workflow
preflight can confirm its file. The owner also decides whether to declare
`1.0.0`; technical evidence is collected in
`.ai/RELEASE_1_0_EVIDENCE.md`. Crates.io remains deferred until an owner
publishing token is supplied.

Further capability expansion remains possible but requires a new contract
amendment with a capability-matrix update before implementation, and—for
anything not probeable in the working environment—a real probe host first.
