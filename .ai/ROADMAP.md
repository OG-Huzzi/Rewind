# Engineering Roadmap

Status: Phase 4 (time and ecosystem integrations) delivered and CI-verified
(`1f37ec3`); Phase 5 (platform expansion) first slice — POSIX named pipes —
delivered per `.ai/PHASE_5_PLATFORM_EXPANSION.md`.

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

## 7. Current gate

Phases 1-4 are implemented and CI-verified. Phase 5's contract
(`.ai/PHASE_5_PLATFORM_EXPANSION.md`) governs the object-expansion slice;
further expansion (new object kinds, metadata classes, or platforms) requires
a contract amendment with a capability-matrix update before implementation.
