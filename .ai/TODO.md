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
