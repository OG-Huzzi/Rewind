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
  implementation commit `6591600c105d33372de91b183f6a09341c5189f1` passed
  CI run #65 on Ubuntu, macOS, and Windows ([run details](https://github.com/OG-Huzzi/Rewind/actions/runs/37026986787)).

## Release and packaging evidence

- Cargo package metadata is version `0.1.0` with owner-selected
  `license = "MIT"`; the root `LICENSE` and `/LICENSE` package include entry
  are verified. `.ai/`, tests, examples, workflow files, and shell integration
  material are excluded by an explicit package allowlist.
- Source installation into an empty temporary install root and the README
  quick start have been exercised on Windows with the GNU Rust toolchain,
  including a fresh clone of pushed commit `6591600`.
- CI run #68 on the MIT-license commit passed on Ubuntu, macOS, and Windows.
  CI run #69 passed on all three platforms after the release publisher gained
  a full-history checkout required by `--notes-from-tag`.
- The tag-only release workflow passed end-to-end on run #2
  (`37034442821`): preflight, release preparation, all native builds, and
  aggregate publication passed. It created GitHub Release ID `401984860`
  ([v0.1.0](https://github.com/OG-Huzzi/Rewind/releases/tag/v0.1.0)). The
  first run failed only at publication because its job had no `.git` checkout;
  the exact cause and no-consumer re-tag are recorded in `.ai/TEST_STATUS.md`.
- All three release assets were downloaded. Linux
  `rewind-v0.1.0-x86_64-unknown-linux-gnu.tar.gz` and macOS
  `rewind-v0.1.0-aarch64-apple-darwin.tar.gz` were verified by archive listing
  only. Windows `rewind-v0.1.0-x86_64-pc-windows-msvc.zip` was listed,
  extracted, and smoke-tested on Windows with version, init, capture, and
  undo; recovery also reported HEALTHY.
- The owner-selected license gate is resolved. No 1.0 declaration is made;
  the owner still decides whether `1.0.0` accurately communicates maturity
  after considering the verified capabilities and deferred items.

## Limits and owner decisions

- Undo/redo covers supported recorded local filesystem state under the
  contracts' evidence and conflict checks. It does not reverse remote or
  system-wide effects and redo does not rerun a command. Passive hooks and
  the watcher carry lower-confidence evidence; gaps and unsupported state
  gate operations.
- POSIX xattrs, directory DACLs/streams, ownership, SACLs, sparse layout,
  macOS flags, broader package ecosystems, and subproject lockfiles remain
  deferred as documented with their probe or scope blockers.
- Deferred capability classes remain as listed above; each needs evidence
  and a contract amendment before implementation.
- The owner must decide whether `1.0.0` accurately communicates maturity and
  separately supply a crates.io publishing token if that distribution path
  is wanted.

No version change should be made from this evidence file alone.
