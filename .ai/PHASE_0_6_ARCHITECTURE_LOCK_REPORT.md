# Phase 0.6 Architecture Lock Report

Status: historical and superseded by Phase 0.7

Phase 0.6 separated SQLite catalog durability from filesystem mutation,
externalized persistent storage, rejected hardlinks for immutable CAS, defined
two observation tiers, and introduced a journaled rollback concept. Those
decisions remain useful historical inputs.

The independent verifier identified six remaining blockers after Phase 0.6:

1. “Baseline did not advance” could still be read as “baseline remains
   trusted” after a failed passive capture.
2. A missing path and a directory did not have a complete typed fingerprint
   model.
3. External quarantine was presented too close to an atomic rename assumption.
4. Recovery inspection was too destination/hash-centric.
5. Windows and POSIX behavior were described as if they were equivalent.
6. Crash-safety language exceeded the documented assumptions.

All Phase 0.6 claims that conflict with those corrections are superseded.
In particular, the old statement that the next operation may re-evaluate a
failed delta from the unchanged baseline is not current architecture. The
current rule is CAPTURE_FAILED -> UNKNOWN_INTERVAL ->
DEGRADED/RECONCILIATION_REQUIRED, with no normal attribution until a full
reconciliation checkpoint succeeds.

The current state model, fingerprint model, same-filesystem quarantine design,
source/destination journal protocol, platform matrix, adversarial tests, and
hostile review are in:

- .ai/PHASE_0_7_ARCHITECTURE_FINALIZATION_REPORT.md
- phases/phase-01-foundation.md
- .ai/DECISIONS.md

Phase 0.6 did not create production code. Phase 0.7 also creates documentation
only. The former “proceed to implementation” wording is withdrawn.
