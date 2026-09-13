# Implementation Changelog

## Unreleased

- Began Phase 1 implementation after the locked Phase 0.7 contract.
- Recorded the implementation map and verification environment.
- Added the external workspace store, SQLite catalog, typed manifests, BLAKE3
  CAS, strong/passive capture, degradation gates, reconciliation, snapshots,
  same-filesystem rollback/redo, journal recovery, and diagnostics CLI.
- Added real filesystem tests and corrected parent-directory rollback planning,
  Windows sync tolerance, reparse refusal, and interrupted CAS cleanup.

## Phase 1.1 — rollback correctness and recovery repair

Fixes for the independent verification findings in
`.ai/PHASE_1_INDEPENDENT_VERIFICATION.md`:

- V-F01 (P0): the rollback planner now records the state each step is
  expected to be in at execution time. A directory's expected fingerprint is
  narrowed to the children that survive in the target state, so a
  transaction's own earlier child removals are no longer misread as external
  conflicts. Undo/redo of non-empty directory trees and interrupted
  recursive rollbacks complete and recover correctly; unexpected external
  objects still trip the conflict gate.
- V-F02 (P1): added `rewind recover` (automatic classification) and
  `rewind recover --reconcile` (explicit abandon-and-reconcile exit path
  with artifact archival and a recorded ABANDONED journal state plus an
  unknown interval). Doctor and refusal messages now point at the real
  commands.
- V-F03 (P2): Windows reparse objects are classified by reading the actual
  reparse tag (FSCTL_GET_REPARSE_POINT). Junctions are
  `UNSUPPORTED(WINDOWS_JUNCTION)`; only a positively identified symlink is
  treated as SYMLINK; unreadable tags are refused.
- V-F04 (P2): post-commit archive copies are flushed through a write handle
  (FlushFileBuffers requires write access), so quarantined bytes actually
  reach the external archive.
- V-F05 (P2): removed the invalid clap `last` + `trailing_var_arg`
  combination that panicked `rewind run` in debug builds.
- V-F06 (P2): `atomic_write` now flushes the payload before publication and
  requests directory-entry durability where the platform supports it;
  journal, pointer, and boundary-log writes are durable.
- V-F07 (P3): transaction staging is removed after commit once the archive
  holds every quarantined artifact; staging that still holds the only copy
  of quarantined bytes is retained.
- V-F09 (P3): shipped `integration/rewind.bash` and `integration/rewind.zsh`
  passive-hook integration with automatic per-shell session ids; the user no
  longer has to invent session identifiers.
- Preserve-step backup paths are no longer planned for directory-to-directory
  steps that never quarantine, and interrupted-transaction archives skip
  never-executed steps, so the post-commit archive reports real failures
  only.
- Added the `tests/rollback_tree.rs` regression suite (9 real-filesystem
  tests): non-empty/nested/large tree undo and redo, interrupted recursive
  rollback recovery, genuine external modifications during rollback,
  junction classification, archive verification with staging cleanup, and
  CLI parse/e2e coverage.
