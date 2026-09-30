# Phase 5 Verification Report (signoff)

Status: **Phase 5 (platform expansion) CLOSED** — four capability slices
delivered, each contract-first, each CI-verified on ubuntu/macOS/windows;
the capability matrix is final for this phase with all remaining items
explicitly deferred and blocked. Close-out/CI-stabilization contract:
`.ai/PHASE_5_CLOSEOUT.md`.

## 1. Delivered slices (capability → contract → decision → CI evidence)

| Slice | Capability | Contract | Decision | Implementation commits | CI verification |
|---|---|---|---|---|---|
| 1 | POSIX named pipes (FIFOs) as first-class objects; precise unsupported-object descriptors | `.ai/PHASE_5_PLATFORM_EXPANSION.md` | ADR-017 | phase-5 series (see `git log`) | green on ubuntu/macOS/windows; POSIX FIFO lifecycle evidence recorded in TEST_STATUS |
| 2 | Windows junctions as first-class literal-leaf objects (reparse data restored byte-faithfully, never followed) | `.ai/PHASE_5_WINDOWS_JUNCTIONS.md` | ADR-019 | `33238bc` + `0f96706` | run #46/#47 green on all three platforms |
| 3 | Windows alternate data streams (named `$DATA` streams on regular files) as CAS-backed fingerprint state | `.ai/PHASE_5_ALTERNATE_DATA_STREAMS.md` | ADR-020 | `5768863` (+`1721503` POSIX lint fix) | run #49 on `1721503` green (id 36582533573); run #50 on `642577e` green |
| 4 | Explicit NTFS DACL ACEs (plus the protected flag) on regular files as fingerprint state | `.ai/PHASE_5_NTFS_DACL.md` | ADR-021 | `d5b84fa` (+`1c95a3b`/`9591f44`/`dcc45ef` runner-environment test fixes) | run #55 on `dcc45ef` green (id 36598054188) |

Associated phases verified alongside: the rollback-performance phase
(ADR-018, run #43 on `6ec5e06`) and the Phase 5.4 audit + close-out
(`a4bf08c`, `8617948`).

## 2. Final capability matrix state

**Supported (capture, restore, verify, quarantine/archive as applicable):**

- Regular file — every platform; POSIX permission modes restored
  authoritatively; named NTFS streams (ADS) and explicit NTFS DACLs on
  Windows.
- Directory — every platform.
- Symlink with an inside-root, creatable target — every platform.
- POSIX named pipe (FIFO) — Linux/macOS; existence + permission mode.
- Windows junction — Windows; literal reparse data; target never followed.

**Recorded and refused (never guessed, never silently dropped):**

- Symlinks with escaping or unknown targets — recorded literally, restore
  refused.
- Unix sockets, character/block devices — `UNSUPPORTED` with named
  descriptors (`UNIX_SOCKET`, `CHARACTER_DEVICE`, `BLOCK_DEVICE`).
- Unrecognized reparse points — refused with the tag value named.
- Foreign fingerprints off their home platform (a `NamedPipe` on Windows;
  `Junction`/streams/DACLs on POSIX) — refused at materialization and never
  matched against quarantined artifacts.
- NULL DACLs (allow-everything) and non-simple DACL ACE types — named
  `ScanIncomplete` errors, not approximations.

**Deferred, with the recorded blocker (not silently unfinished):**

- Directory-attached NTFS streams and directory DACLs — real NTFS state,
  deliberately out of the file-scoped slices; each needs its own contract
  amendment.
- POSIX xattrs — capture/restore are implementable in the `user.*`
  namespace, but no POSIX host exists in the working environment (no WSL,
  no Docker) to probe against; the gate forbids implementing blind.
- Ownership (uid/gid, Windows owner/group), SACL/audit ACEs —
  privilege-bound (CI cannot verify).
- Sparse-file layout — kernel/presentation, not content identity.
- chflags — macOS-only; no macOS host.
- Phase 4 leftovers (unchanged by Phase 5): package-specific recipes
  (evaluated and deferred, ADR-016); range-based restore (excluded,
  ADR-015).

## 3. Verification summary

- Local (Windows host): full suite 166/0 (`cargo test --all-targets`,
  twice consecutively at close-out; also green under `--all-features`),
  fmt/check/clippy `-D warnings` green.
- Every slice's tests drive the real capture/undo/redo machinery on the
  platform that produces the object (POSIX lifecycle tests on Linux/macOS
  CI, Windows lifecycle tests on windows CI); POSIX-only or Windows-only
  gaps are disclosed per slice in TEST_STATUS, never claimed as passing.
- CI: the three-platform matrix (ubuntu-latest, macos-latest,
  windows-latest) is green for every slice's head commit (per-slice runs
  in §1). During slice 4 and close-out, four CI failures occurred; each
  was diagnosed, classified, and recorded in TEST_STATUS: three were
  test-environment/fixture assumptions (runner ACL shapes) fixed by
  making the tests environment-agnostic, and the timing flakes
  (`boundary_correlation` overlap, `shell_integration` rapid commands,
  `rollback_tree` capture flag) were root-caused and fixed
  deterministically in the close-out (`.ai/PHASE_5_CLOSEOUT.md` §2–§3);
  the bypass fallback and every identity assertion remain fully
  exercised.
- Not executed anywhere: power-loss durability (unchanged Phase 1
  limitation), FAT-family DACL behavior (no FAT volume; failed reads
  degrade honestly), stream/DACL behavior on non-probed filesystems.

## 4. Close-out changes (this phase)

- `REWIND_HOOK_LEASE_RETRY_MS`: documented tuning knob for the hook's
  lease-retry budget (default unchanged at 2000 ms; bypass fallback
  unchanged and still covered by
  `post_during_writer_activity_gates_durably_and_leaves_others_pending`).
- `boundary_correlation::overlapping_background_posts_keep_identity`:
  deterministic claim barrier (hold lease → poll all claims → drop)
  replaces the fixed sleep.
- `shell_integration::rapid_commands_keep_command_identity`: trailing
  sentinel command guarantees the last real command's post-hook spawn;
  generous retry budget in the shell env; sentinel boundary tolerated as
  an extra.
- `rollback_tree::capture_tree_creation`: capture failures now surface
  `capture_error`/exit code in the assertion message.

## 5. Signoff position

Phase 5 is complete per its contract: object and metadata support expanded
only with platform-specific tests and an updated capability matrix; a
platform is not claimed supported merely because an API name exists.
Everything else on the matrix is explicitly deferred with its blocker.
Recommended next phase: **ecosystem recipes** (the deferred ADR-016 work),
after which release engineering (packaging, user-facing documentation,
1.0) is the remaining planned phase.
