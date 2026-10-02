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

# Phase 7 TODO

- [x] Contract and ADR-023 committed before release-facing implementation.
- [x] Correct Cargo package metadata and restrict the crate contents to the
      declared allowlist; the owner later selected MIT and the package now
      includes the matching LICENSE file.
- [x] Replace stale README claims with evidence-linked capability guidance,
      install steps, and a runnable quick start; audit CLI help and malformed
      inputs.
- [x] Add the license-preflight, tag-only three-platform release workflow;
      statically inspect its gates, permissions, aggregation, and full-SHA
      action pins.
- [x] Local fmt/check/Clippy pass; full suite 188/0 twice consecutively;
      install and README quick start pass. The earlier unexplained Windows
      GNU heap-corruption incident remains documented in TEST_STATUS.md.
- [x] Push the prepared commit series; CI run #65 green on Ubuntu, macOS,
      and Windows. Record the hosted run and pushed-clone install evidence.
- [x] Owner selected MIT (SPDX `MIT`); add `LICENSE`, matching Cargo
      metadata/package allowlist, and license notices to README.
- [x] CI run #68 validates the license commit; add a full-history checkout
      to the release publisher after run #1 exposed the missing repository.
- [x] CI run #69 validates the publisher fix. Annotated `v0.1.0` tag and
      release workflow run #2 succeeded; all three assets were downloaded,
      Unix archive listings inspected, and the Windows binary smoke-tested.
- [x] Record the verified release and workflow history in project docs and
      README, then push and verify CI for the documentation commit.
- [ ] Owner separately decides whether to declare 1.0.0 using
      `.ai/RELEASE_1_0_EVIDENCE.md`; provide a crates.io token only if that
      publication path is wanted.
