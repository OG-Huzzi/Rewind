# Phase 0 Repair Report

Status: historical record, superseded by Phase 0.7

Phase 0 established the product direction: explicit workspace identity,
external persistent storage, deterministic state artifacts, no hardlinks for
immutable CAS, and refusal rather than blind rollback. The repair also rejected
whole-directory swaps, generic three-way merging, silently guessed roots, and
unqualified performance claims.

The remaining issues discovered after that repair were not closed by merely
leaving the baseline pointer unchanged. Phase 0.7 therefore supersedes this
report on:

- failed-capture degradation and reconciliation;
- UNKNOWN_INTERVAL semantics;
- typed ABSENT/file/directory/symlink/unsupported fingerprints;
- same-filesystem local quarantine versus external archive;
- source/destination physical recovery maps;
- platform-specific Windows/POSIX transactions;
- conditional crash-safety language.

This report is retained only to explain the evolution of the design. It is not
an implementation contract. Use
.ai/PHASE_0_7_ARCHITECTURE_FINALIZATION_REPORT.md and
phases/phase-01-foundation.md for current behavior.

Phase 1 remained unimplemented during the repair and remains unimplemented
during Phase 0.7.
