# Evidence for a Future 1.0 Decision

Status: **evidence collected for owner review; no 1.0 declaration is made.**
Technical evidence can inform the version decision, but only the project
owner decides whether and when to declare `1.0.0`.

## Verified capability evidence

- The foundation implements explicit workspace identity and an external
  store; typed filesystem state capture; strong command capture; conditional
  undo/redo and recovery; unknown-interval and degradation gates; and
  single-writer enforcement. The architecture and phase contracts define
  these guarantees and their limits.
- Phase 5's final capability matrix is recorded in
  `.ai/PHASE_5_VERIFICATION_REPORT.md`: regular files and directories on all
  supported platforms; inside-root creatable symlinks; POSIX FIFOs on Linux
  and macOS; Windows junctions; Windows named NTFS streams and explicit
  DACL ACEs on regular files. Unsupported, foreign-platform, and deferred
  objects are refused or recorded with the named limits described there.
- Phase 6 adds evidence for a small probed set of root-level lockfiles.
  It records only the changed lockfile path and before/after content hashes
  from a strong capture. It does not claim package transaction, registry,
  cache, or remote-state reversibility (`.ai/PHASE_6_RECIPES.md`, ADR-022).
- The Phase 6 full suite passed 187/0 twice consecutively and CI run #63
  (`36981061095`) passed on Ubuntu, macOS, and Windows; details and the
  intermediate real failures/fixes are in `.ai/TEST_STATUS.md`.
- Phase 7 local gates on the prepared release changes passed: formatting,
  all-target compilation, Clippy with warnings denied, and two consecutive
  full runs at 188/0. A preceding Windows GNU full-suite process terminated
  with `STATUS_HEAP_CORRUPTION`; it was not root-caused. The Phase 7 final
  commit's hosted CI result is pending the push and must be added here after
  observation.

## Release and packaging evidence

- Cargo package metadata is prepared at version `0.1.0`; `.ai/`, tests,
  examples, workflow files, and shell integration material are excluded by
  an explicit package allowlist.
- Source installation into an empty temporary install root and the README
  quick start have been exercised on Windows with the GNU Rust toolchain.
- The tag-only release workflow is prepared with native Linux, macOS, and
  Windows builds, format/check/Clippy/test gates before archive builds,
  full-SHA-pinned actions, and one Release aggregation job. It has not run.
- There is no license file. Cargo's stale `MIT` declaration was removed;
  no tag, GitHub Release, or public artifact has been created. This blocks
  distribution and remains an owner decision, not evidence for a version
  declaration.

## Limits and owner decisions

- Undo/redo covers supported recorded local filesystem state under the
  contracts' evidence and conflict checks. It does not reverse remote or
  system-wide effects and redo does not rerun a command. Passive hooks and
  the watcher carry lower-confidence evidence; gaps and unsupported state
  gate operations.
- POSIX xattrs, directory DACLs/streams, ownership, SACLs, sparse layout,
  macOS flags, broader package ecosystems, and subproject lockfiles remain
  deferred as documented with their probe or scope blockers.
- Hosted CI for the prepared final commit, a licensed tagged build, archive
  inspection, and installed-binary smoke tests from each platform archive
  remain outstanding.
- The owner must decide the license and provide its file, decide whether
  `1.0.0` accurately communicates the project's maturity, and separately
  supply a crates.io publishing token if that distribution path is wanted.

No version change should be made from this evidence file alone.
