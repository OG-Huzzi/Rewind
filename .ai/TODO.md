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
