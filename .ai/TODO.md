# Phase 1 TODO

- [x] Create the Rust package and module boundaries.
- [x] Implement external workspace registration and writer locking.
- [x] Implement SQLite catalog and state/degradation records.
- [x] Implement typed deterministic scan and BLAKE3 CAS.
- [x] Implement strong run capture and reconciliation.
- [x] Implement passive boundary commands with fail-open behavior.
- [x] Implement anchored same-filesystem rollback and redo.
- [x] Implement journal recovery and doctor diagnostics.
- [x] Add shell hook boundary adapters without introducing a daemon.
- [x] Add initial real-filesystem, adversarial, security, and conflict tests.
- [x] Complete cross-platform verification. (Closed at Phase 5 close-out:
      CI runs fmt/check/clippy(-D warnings)/test on ubuntu-latest,
      macos-latest, and windows-latest (MSVC) for every push, and the
      platform-specific suites execute natively on each runner; the
      historical "38-scenario matrix" and "native MSVC/Linux/macOS where
      toolchains exist" items are superseded by this standing coverage —
      see `.ai/PHASE_5_VERIFICATION_REPORT.md` §3.)

# Phase 3 TODO

- [x] Write the continuous-observation contract (`.ai/PHASE_3_CONTINUOUS_OBSERVATION.md`).
- [x] Platform-neutral event model + adapters (notify: inotify/FSEvents/ReadDirectoryChangesW) + deterministic fake adapter.
- [x] Serve loop: batching, bounded coalescing, raw event log with rotation, heartbeat lifecycle, durable degradation records.
- [x] `rewind watch start/status/stop` with detached spawn that never
      inherits the caller's stdio pipes (Windows: raw `CreateProcessW`,
      `bInheritHandles = FALSE`).
- [x] Crash/restart, offline, and overflow semantics with durable
      `WATCHER_GAP`/`OVERFLOW` records consumed by the single-writer
      enforcement points.
- [x] 15 integration + 28 unit tests; full suite 119/119; measured
      performance recorded in the verification report.
- [x] CI green on ubuntu/macos/windows for the pushed commit
      (run #36111890265 on `d69fdca`, after two diagnostic rounds).

# Phase 5 TODO

- [x] Slice 1 — POSIX named pipes as first-class objects (ADR-017).
- [x] Slice 2 — Windows junctions as first-class literal-leaf objects (ADR-019).
- [x] Slice 3 — Windows alternate data streams on regular files (ADR-020).
- [x] Slice 4 — explicit NTFS DACL ACEs on regular files (ADR-021).
- [ ] Deferred metadata classes (each requires its own contract amendment
      and probe evidence before implementation): directory DACLs; POSIX
      xattrs (blocked: no POSIX probe host — no WSL/Docker on the working
      machine); owner/group and SACL (privilege-bound); sparse-file layout;
      chflags (no macOS host).

# Phase 6 TODO

- [x] Contract `.ai/PHASE_6_RECIPES.md` + ADR-022 committed before any
      production code.
- [x] Probes: cargo/npm/pnpm/go/python+pip/uv real operations recorded,
      including offline-safe variants and the nested
      `node_modules/.package-lock.json` depth-policy witness.
- [x] Evidence layer: pure recognition (`src/recipes.rs`), additive
      `operation_evidence` persistence via the established idempotent DDL
      pattern, capture-side integration, presentation on
      list/show/timeline with byte-identical unchanged-output guarantees.
- [x] Tests: 9 unit + 12 integration (file-based lifecycle, no-false-
      positives, all six kinds, passive/failure boundaries, legacy-catalog
      migration, presentation, determinism, undo/redo, real-manager
      cargo+npm); full suite 187/0 twice consecutively; all local gates
      green.
- [x] Adversarial review completed; findings fixed (test-side defects in
      `8002933` and `b058cdb`, recorded in TEST_STATUS).
- [x] CI green on ubuntu/macOS/windows for the phase commits (run #63 on
      `b058cdb`; the two intermediate failures and their real root causes
      are recorded in TEST_STATUS).
- [ ] Deferred (unchanged, still deferred): unprobed ecosystems (yarn,
      bun, poetry, Pipfile, Gemfile, composer, …) and monorepo
      sub-project lockfiles — each requires probe evidence and a contract
      amendment; package-transaction semantics remain rejected (ADR-016).
