# Engineering Roadmap

Status: Phase 1 foundation implementation under verification

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

## 6. Phase 5 - Platform expansion

Expand object and metadata support only with platform-specific tests and an
updated capability matrix. A new platform is not considered supported merely
because a common API name exists.

## 7. Current gate

Phase 0.7 was documentation-only. Phase 1 remains limited to the frozen
foundation contract until its implementation and independent verification are
complete.
