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
- [ ] Complete the full 38-scenario matrix on every supported platform.
- [ ] Run native MSVC, Linux, and macOS verification where toolchains exist.

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
- [ ] CI confirmation on ubuntu/macos/windows for the pushed commit.
