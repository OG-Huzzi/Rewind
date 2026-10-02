# Phase 7 Test Status

Contract: `.ai/PHASE_7_RELEASE.md` (AC1–AC12); decision: ADR-023.

Local environment: Windows, `stable-x86_64-pc-windows-gnu`, NTFS. The
default MSVC Rust toolchain cannot link on this host because `link.exe` is
absent; this does not affect hosted Windows CI, which uses MSVC.

- `cargo fmt --all -- --check`: PASS.
- `cargo check --all-targets`: PASS.
- `cargo clippy --all-targets --color never -- -D warnings`: PASS.
- `cargo test --color never`: 188 passed / 0 failed on two consecutive
  final-source runs (71 library unit tests plus 117 integration tests;
  binary and doc-test harnesses contain no tests).
- Incident history retained: an earlier full-suite run passed, then the
  next two full-suite attempts reported
  `passive_observations_and_capture_failures_carry_no_evidence` failing in
  `tests/phase6_recipes` and the Windows process exited with
  `0xc0000374 STATUS_HEAP_CORRUPTION`. The test passed alone; the complete
  Phase 6 test binary passed with `--nocapture` and `--test-threads=1`; and
  three subsequent standard runs of that integration binary passed. The two
  complete final-source runs above also passed. No root cause was found, so
  this is an unexplained local process failure, not a diagnosed flake.
- CLI audit: all 30 visible root/nested help pages exited 0; `--version`
  printed `rewind 0.1.0`. `show` without an ID, `inspect history --limit
  nope`, and `inspect history --json nope` returned readable usage errors
  (exit 2). `status` outside an initialized workspace and `init` with a
  nonexistent path returned readable errors (exit 1); the nonexistent path
  was not created. No panic was observed.
- Fresh-root source install and README PowerShell quick start passed:
  version, init, capture, status, undo, redo, list, show, and recovery.
  Undo removed the created file; redo restored it; recovery reported a
  healthy workspace with no unfinished transaction.
- `cargo package --list`: PASS with the expected warning that no license or
  license-file is specified. Entries are limited to `.cargo_vcs_info.json`,
  `Cargo.lock`, `Cargo.toml`, `Cargo.toml.orig`, `README.md`, and `src/**`.
  No `.ai/`, tests, examples, integrations, or workflow files are packaged.
- Release workflow YAML parsed and static assertions passed: only the `v*`
  tag trigger, full 40-character action SHAs, all three native runner jobs,
  quality gates before build, and one aggregated release job. No
  `actionlint` binary was available. Release workflow was not run because
  the owner license gate prohibits creating a tag.
- GitHub Actions CI run #65, push commit
  `6591600c105d33372de91b183f6a09341c5189f1`, completed successfully on
  Ubuntu, macOS, and Windows: [run details](https://github.com/OG-Huzzi/Rewind/actions/runs/37026986787).
  Each platform passed format, all-target check, Clippy with warnings
  denied, and the full suite. A fresh clone of the pushed commit installed
  locally and passed the README quick start through recovery.
- The release workflow itself remains unrun because no tag is permitted
  before the owner license gate is resolved. No tag, Release, or public
  artifact exists.

# Phase 1 Test Status

Status after the Phase 6 evidence layer. Newer records are at the top;
earlier phase records are preserved below.

## Phase 6 (manifest/lockfile evidence layer)

Contract: `.ai/PHASE_6_RECIPES.md` (AC1–AC10); decision: ADR-022.

Local suite on Windows (x86_64-pc-windows-gnu → default toolchain updated
to 1.99.0 during this phase, see the CI record below; debug, NTFS):
`cargo fmt --all -- --check` PASS, `cargo check --all-targets` PASS,
`cargo clippy --all-targets --all-features -- -D warnings` PASS, and
`cargo test --all-targets` **187 passed / 0 failed, twice consecutively**
(verified on 1.98.1 before the toolchain update and re-verified on 1.99.0
after it) — 61→70 lib (9 new recipes unit tests), 10 boundary_correlation,
13 foundation, 8 hardening, 15 phase2_dependency, 17 phase3_watcher, 7
phase4_timeline, 16 phase5_platform, **12 phase6_recipes** (all new), 3
rollback_path_scan, 8 rollback_tree, 8 shell_integration. Every existing
suite unchanged and green.

New Phase 6 tests (all driving the real capture/undo/redo machinery unless
noted):

- `recipes::tests::*` (9 lib unit tests, all platforms): the three
  transitions with recorded hashes, same-hash no-change policy,
  manifest-only policy, root-only/nested/similar-name suppression, exact
  case rules, non-regular object suppression, manifests-recorded-when-
  present, deterministic ordering across all six kinds, npm shrinkwrap as
  a second npm lockfile name.
- `phase6_recipes::lockfile_lifecycle_yields_evidence_with_verifiable_hashes`
  (AC1+AC3): created/changed/removed through real capture, hashes verified
  against the CAS.
- `phase6_recipes::no_false_positives_for_nested_similar_or_deviating_names`
  (AC2+AC4): nested `sub/Cargo.lock`, `.bak`/`.old` names, unrecognized
  `Gemfile.lock`, deviating casing (`cargo.lock` recognized nowhere),
  directory-at-lockfile-name and file→directory replacement (suppressed,
  never guessed).
- `phase6_recipes::same_hash_rewrites_and_manifest_only_changes_are_not_evidence`
  (AC3 boundary): byte-identical rewrite and manifest-only edit yield no
  entry.
- `phase6_recipes::every_recognized_kind_is_derived_through_real_capture`
  (AC1): all six kinds in one capture, documented manifests per kind,
  deterministic lockfile-path ordering, CAS verification.
- `phase6_recipes::passive_observations_and_capture_failures_carry_no_evidence`
  (AC5): a real `hook pre`/`hook post` flow whose observed change is a
  lockfile change still records no evidence; the shared
  `record_capture_failure` path likewise.
- `phase6_recipes::legacy_catalogs_gain_the_table_idempotently_and_rows_read_unchanged`
  (AC6): pre-phase catalog simulated by dropping `operation_evidence`;
  reopen recreates it idempotently; legacy rows read and display
  byte-identically (field skipped in `show`); new captures work; a
  malformed blob is an explicit error.
- `phase6_recipes::presentation_renders_evidence_and_evidence_free_output_is_unchanged`
  (AC7): `list`/`show`/timeline human + JSON render evidence for stores
  that contain it (names only in human surfaces; hashes in JSON), and
  evidence-free stores are byte-identical to the pre-phase form on every
  surface, including `inspect history --json`.
- `phase6_recipes::timeline_json_is_byte_identical_for_evidence_bearing_and_free_stores`
  (AC7): Phase 4 AC3 determinism extended; the range is computed from the
  recorded operation times (clock-independent, per the adversarial
  review — see below).
- `phase6_recipes::undo_and_redo_are_unaffected_by_recorded_evidence`
  (AC8): undo/redo restore exact states; the evidence record is unchanged
  by either direction; workspace stays HEALTHY.
- `phase6_recipes::real_cargo_action_yields_evidence_with_verifiable_hashes`
  (AC9): captured `cargo generate-lockfile --offline` (probed offline-safe,
  no dependencies) yields added evidence; after a captured manifest
  rewrite, a second run yields the changed transition; hashes verified
  against the CAS.
- `phase6_recipes::real_npm_action_yields_evidence_with_verifiable_hashes`
  (AC9): captured `npm install --package-lock-only --ignore-scripts
  --offline --no-audit --no-fund` (probed offline-safe with a blackholed
  registry for a zero-dependency project) yields added evidence, then the
  changed transition after a version bump.
- `phase6_recipes::evidence_hashes_are_the_recorded_fingerprint_ids`
  (AC1): the evidence presents exactly the recorded fingerprint CAS ids.

Adversarial review of the slice diff (contract §8 Step 5) answered every
prompt question and found two test-side defects, both fixed (`8002933`):
the determinism test's fixed calendar range would have rotted in 2027
(environment dependence — replaced with recorded-time-derived ranges),
and `inspect history --json` was an untested machine surface (now
covered both ways). No production code changed in the review.

Platform gaps (disclosed, not passing claims): the real-manager tests
execute wherever the tool exists — cargo on all three CI runners (the CI
itself installs Rust), npm on all three runners; on a host without the
tool the test reports an honest skip. go has no offline-safe
evidence-producing operation (probed: local `replace` writes no `go.sum`;
sums exist only for fetched modules), so go recognition is covered
file-based and by the recorded probe; pnpm/uv/pip recognition is covered
file-based and by the recorded probes, with no real-manager CI test.

CI (GitHub Actions, `OG-Huzzi/Rewind`, fmt/check/clippy -D warnings/test
on ubuntu-latest, macos-latest, windows-latest):

- Run #63 on `b058cdb`: **green on ubuntu, macOS, and windows** (workflow
  run 36981061095; check-runs success × 3) — including the real-manager
  cargo/npm tests on every runner and the POSIX shebang fix.
- Run on `7efbc14`: **all three platforms failed at the Format step**
  (annotations: "Process completed with exit code 1" with no clippy or
  test-panic annotations — a plain run step). Classified: a **real
  failure**, not a flake, and not a toolchain mystery (although CI's
  `@stable` now resolves to 1.99.0, released 2026-09-28, the diff also
  reproduces on 1.98.1's rustfmt). Root cause: the adversarial-review
  fix commit (`8002933`) edited `tests/phase6_recipes.rs` through a
  scripted edit that introduced an over-long line, and the fmt gate was
  not re-run after it — only the test suites were. Process defect in the
  review step, fixed by re-running the gate discipline: `cargo fmt
  --all` applied (`126baaf`), and the full gate sequence re-executed on
  the updated 1.99.0 toolchain (fmt/check/clippy `-D warnings` + suite
  187/0 twice).
- Run #62 on `156b039`: macOS green; ubuntu and windows failed with two
  further **real test-environment defects, not flakes**, both fixed in
  `b058cdb`: (1) ubuntu — three script-driven tests failed with
  `Io(Os { code: 8, "Exec format error" })` because this suite's POSIX
  script bodies carried no shebang (a shebang-less script is not
  executable on Linux; macOS happened to fall back — non-uniform
  behavior the phase forbids relying on). Fix: the helper prepends
  `#!/bin/sh` to every POSIX body (the foundation suite's convention),
  documented on `tests/common::shell_script`. (2) windows —
  `passive_observations_and_capture_failures_carry_no_evidence` required
  the post-hook to record a PassiveObservation, but the hook's 50 ms
  scan deadline (Phase 1.3, not env-tunable) blew on the loaded runner
  and the hook landed in its documented conservative CaptureFailed
  model. Fix: the test accepts both documented outcomes (found by the
  boundary's exact command text) and asserts the Phase 6 property in
  each branch — a non-strong operation never carries evidence. No
  evidence assertion was weakened; the wall-clock-dependent kind
  expectation is gone.

## Phase 5 close-out / CI stabilization

Contract: `.ai/PHASE_5_CLOSEOUT.md` (root causes §2, changes §3). The
three timing flakes observed during Phase 5 were root-caused and fixed
deterministically:

- `boundary_correlation::overlapping_background_posts_keep_identity` —
  the hook's 2 s lease-retry budget expired under runner load while peer
  hooks serialized fsync-heavy bookkeeping; the test now gives its spawned
  hooks `REWIND_HOOK_LEASE_RETRY_MS=60000` and replaces the fixed 500 ms
  sleep with a claim barrier (hold lease → poll until all three hooks have
  claimed → drop), making overlap a verified fact. The bypass fallback
  itself stays covered against the default budget by
  `post_during_writer_activity_gates_durably_and_leaves_others_pending`.
- `shell_integration::rapid_commands_keep_command_identity` — the spawn of
  the last real command's post-hook depended on a PROMPT_COMMAND cycle at
  end of input, which buffered interactive bash delivers unreliably
  (probe: zero cycles for fully buffered input). A trailing sentinel
  command now guarantees the spawn cycle; the shell env sets
  `REWIND_HOOK_LEASE_RETRY_MS=60000`; the sentinel's boundary joins the
  tolerated extras; the Healthy branch accepts `>= expected.len()`
  observations for the sentinel's own possible observation.
- `rollback_tree::capture_tree_creation` — diagnostics only: the capture
  assertion now surfaces `capture_error`/exit code (no scanner change
  without a reproduced, diagnosed failure).

Local evidence: the two reworked binaries pass 3 consecutive runs each;
the full suite passed twice consecutively (166/0), with fmt/check/clippy
`-D warnings` green. CI: **run on `899a667` green on ubuntu, macOS, and
windows on the first attempt — no re-run needed** — including both
previously-flaky suites on the loaded runners.

## Phase 5 slice 4 (Windows NTFS DACLs — explicit ACEs)

Contract: `.ai/PHASE_5_NTFS_DACL.md`; decision: ADR-021. The DACL mechanics
were probed on the host (Windows 11 build 26200, NTFS, non-admin) through
the exact implementation APIs before the contract was written
(`GetNamedSecurityInfoW`/`SetNamedSecurityInfoW` read→set→read SDDL-identical,
explicit-only rebuild re-deriving inherited ACEs, protected flag round trip,
`fs::rename` preserving the DACL, `fs::copy` dropping explicit ACEs, DACL
apply working on readonly files) — the probe was a scratch tool and is not
committed.

Local suite on Windows (x86_64-pc-windows-gnu, Rust 1.98.1, debug, NTFS):
`cargo fmt --all -- --check` PASS, `cargo check --all-targets` PASS,
clippy `--all-targets --all-features -- -D warnings` PASS, and
`cargo test --all-targets` **166 passed / 0 failed** — the same 166/0 under
`--all-targets --all-features`: 61 lib, 10 boundary_correlation, 13
foundation, 8 hardening, 15 phase2_dependency, 17 phase3_watcher, 7
phase4_timeline, **16 phase5_platform** (3 model-level on all platforms + 5
Windows junction lifecycle + 4 Windows stream lifecycle + 4 Windows DACL
lifecycle), 3 rollback_path_scan (DACL cases added to the
fingerprint-equivalence test), 8 rollback_tree, 8 shell_integration. The
rollback bench harness still runs end-to-end at N=9 (mixed scenario) with
the new capture path.

New DACL tests (all driving the real capture/undo/redo machinery on
Windows/NTFS unless noted):

- `stream_fingerprints_are_backward_compatible` (all platforms, AC1)
  extended: DACL-free files still serialize byte-identically; DACL-carrying
  and protected-empty (deny-all) fingerprints round-trip through serde;
  `describe()` names the record.
- `dacl::explicit_dacls_are_captured_into_the_fingerprint` (AC2): an
  icacls grant is captured with its exact type/flags/mask/SID string and the
  unprotected flag; a plain file serializes without the field.
- `dacl::dacl_changes_are_captured_and_reversible` (AC3): undo restores the
  exact DACL-free pre-state, redo the exact grant; the protected
  (`/inheritance:d`) state with its copied explicit ACEs undoes and redoes
  exactly; content untouched throughout.
- `dacl::external_dacl_divergence_refuses_undo` (AC4): an external DACL
  modification after capture refuses undo before any mutation; both grants
  survive untouched.
- `dacl::dacl_carrying_files_archive_faithfully` (AC5): the archive copy
  carries the quarantined file's explicit DACL (`copy_artifact` re-applies
  what `fs::copy` drops) and matched after `verify_archive_pair` compared
  both sides.
- `rollback_path_scan::path_scoped_fingerprints_match_the_full_scan`
  (AC6, Windows block): DACL-carrying files classify identically through
  the path-scoped and full scans, with the explicit ACE record intact.

Empirical findings recorded during testing (behavior, not flakes):

- icacls deny masks include SYNCHRONIZE (e.g. `(W)` = 0x100116): a
  deny-ACE'd file denies even reads, so Rewind's capture fails with Access
  Denied and the workspace degrades honestly (the contract's
  `ScanIncomplete` path). Tests use grants, which keep the file scannable.
- `icacls /inheritance:r` deletes inherited ACEs, producing the
  protected-empty deny-all state (real state; unreadable; capture degrades
  honestly). `/inheritance:d` disables inheritance and copies the ACEs —
  the testable protected form.
- icacls *replaces* an existing trustee's grant instead of adding a second
  one; divergence tests grant a second trustee.
- The GitHub Windows runners' temp dirs carry **non-inheriting default
  ACLs**, and their `icacls /grant` also **converts the previously
  inherited ACEs into explicit copies** (locally it does not — another
  icacls behavior difference). Two CI rounds failed on count- and
  delta-based assertions written against this host's icacls semantics;
  the implementation was correct throughout (it captured exactly the
  explicit ACEs present in both environments). The tests now assert the
  environment-agnostic contract — the recorded explicit set equals the
  live set, contains the grant, and undo/redo restore recorded states
  exactly — green locally and on runners; the contract AC wording was
  amended to match.

Platform gaps (disclosed, not passing claims): POSIX platforms run only the
model-level tests (AC1); the DACL variant is Windows-produced only, and the
off-Windows refusal paths are compile-gated (`cfg(not(windows))`), mirroring
every prior Phase 5 slice. FAT-family behavior was not probed (no FAT
volume); a failed security read degrades honestly regardless.

CI (GitHub Actions, `OG-Huzzi/Rewind`, fmt/check/clippy -D warnings/test on
ubuntu-latest, macos-latest, windows-latest):

- Run #52 on `6ea9a6a`: ubuntu+macOS green; windows Tests failed — the
  DACL tests' count assertions assumed an inheriting parent (runner temp
  dirs create files with explicit SYSTEM/Admins/owner ACEs).
- Run #53 on `1c95a3b`: windows still failed — runner icacls also converts
  previously inherited ACEs to explicit copies, so baseline-relative
  *counts* were still environment-dependent.
- Run #54 on `9591f44`: windows still failed — the AC6 equivalence test had
  the same count assertion.
- **Run #55 on `dcc45ef`: green on ubuntu, macOS, and windows** (run id
  36598054188) — the slice's CI verification. On this run windows first
  failed in `boundary_correlation::overlapping_background_posts_keep_identity`
  ("no operation for COMMAND_C"), a Phase 1.4 hook-timing test unrelated to
  the DACL slice (it passed on every earlier run of this slice, including
  three with the full DACL code); the failed job was re-run unchanged and
  passed — recorded as a runner-load flake in the same class as the
  `undo_of_nested_tree_with_many_files` flake above, with the
  implementation untouched. The record commit `b188e06` itself saw one
  more instance of the same flake family on ubuntu
  (`shell_integration::bash_rapid_commands_keep_command_identity`, a
  doc-only commit, green on re-run) — three timing flakes in six runs, all
  in Phase 1.3/1.4 hook-timing tests under runner load, none touching this
  slice's code paths.

## Phase 5 slice 3 (Windows alternate data streams)

Contract: `.ai/PHASE_5_ALTERNATE_DATA_STREAMS.md`; decision: ADR-020. The
ADS mechanics were probe-verified against a real NTFS volume before the
contract was written (`FindFirstStreamW` entry forms, `path:name:$DATA`
spec paths through std APIs, `fs::rename`/`fs::copy` carrying streams,
stream writes on junction paths following the reparse point, stream writes
denied on readonly files) — the probe example was a scratch tool and is not
part of the tree.

Local suite on Windows (x86_64-pc-windows-gnu, Rust 1.98.1, debug, NTFS):
`cargo fmt --all -- --check` PASS, `cargo check --all-targets` PASS,
clippy `--all-targets --all-features -- -D warnings` PASS, and the full
suite **162 passed / 0 failed** (`cargo test --all-targets`): 61 lib,
10 boundary_correlation, 13 foundation, 8 hardening, 15 phase2_dependency,
17 phase3_watcher, 7 phase4_timeline, **12 phase5_platform** (3 model-level
on all platforms + 5 Windows junction lifecycle + 4 Windows stream
lifecycle), 3 rollback_path_scan (stream cases added to the
fingerprint-equivalence test), 8 rollback_tree, 8 shell_integration.

New stream tests (all driving the real capture/undo/redo machinery on
Windows/NTFS unless noted):

- `stream_fingerprints_are_backward_compatible` (all platforms, AC1): a
  stream-free file serializes byte-identically to the pre-slice form, a
  legacy manifest without the field deserializes, a stream-carrying
  fingerprint round-trips.
- `streams::named_streams_are_captured_into_the_fingerprint` (AC2): named,
  empty, and spaced/dotted stream names captured with exact CAS bytes; the
  default stream untouched; a stream-free file serializes without a
  `streams` field.
- `streams::stream_changes_are_captured_and_reversible` (AC3): modify/
  create/delete of streams captured in one operation; undo restores the
  exact recorded set (created stream removed), redo restores the new set
  (deleted stream removed); the default stream is untouched throughout.
- `streams::external_stream_divergence_refuses_undo` (AC4): an external
  stream modification after capture refuses undo before any mutation;
  main content untouched by the refusal.
- `streams::stream_carrying_files_archive_faithfully` (AC5): quarantine
  carries streams (rename), the archive copy carries them
  (`fs::copy`/`CopyFileEx`), and `verify_archive_pair` compared stream
  name sets and content before marking `Archived`.
- `rollback_path_scan::path_scoped_fingerprints_match_the_full_scan`
  (AC6, Windows block): stream-carrying files classify identically through
  the path-scoped and full scans, with the full stream set recorded
  (rollback-perf invariant preserved).

Platform gaps (disclosed, not passing claims): POSIX platforms run only the
model-level tests (AC1 + the refusal-path compile unit) — the stream
variant is Windows-produced only, mirroring the junction slice's POSIX gap.
`undo_of_nested_tree_with_many_files` was observed to fail once under full
parallel suite load on the Windows host ("tree creation was not captured")
and passed 3/3 in isolation plus the two full re-runs; recorded as a host
load flake, not addressed by changing the test.

CI (GitHub Actions, `OG-Huzzi/Rewind`, fmt/check/clippy -D warnings/test
on ubuntu-latest, macos-latest, windows-latest): run #48 on `1cfac81`
failed on ubuntu+macOS clippy (`variable does not need to be mutable` —
the `streams` map is mutated only by the cfg(windows) enumeration;
windows-latest passed in full on `1cfac81`, compile + clippy + all tests
including the stream lifecycle suite). Fixed by `1721503`
(cfg_attr(not(windows), allow(unused_mut)), the slice-2 pattern), and
**run #49 on `1721503` is green on ubuntu, macOS, and windows** — the
slice's CI verification (run id 36582533573).

## Phase 5 slice 2 (Windows junctions as first-class objects)

Contract: `.ai/PHASE_5_WINDOWS_JUNCTIONS.md`; decision: ADR-019. The
junction mechanics were probe-verified against real `mklink /J` junctions
before implementation (buffer layout, `FSCTL_SET_REPARSE_POINT` =
0x000900A4, byte-identical round trip, quarantine rename, dangling-target
restore, `attrib +R` following the junction) — the probe example was a
scratch tool and is not part of the tree.

Local suite on Windows (x86_64-pc-windows-gnu, Rust 1.98.1, debug):
fmt, `cargo check --all-targets`, clippy `--all-targets -- -D warnings`,
`cargo test --doc`, and the full suite **157 passed / 0 failed**
(`cargo test --all-targets`): 61 lib, 10 boundary_correlation, 13
foundation, 8 hardening, 15 phase2_dependency, 17 phase3_watcher, 7
phase4_timeline, **7 phase5_platform** (2 model-level on all platforms + 5
Windows junction lifecycle tests), 3 rollback_path_scan (junction cases
added to the fingerprint-equivalence test), 8 rollback_tree (the retired
junction-is-unsupported test replaced by the phase5 suite, per the amended
contract AC7), 8 shell_integration.

New junction tests (all driving the real capture/undo/redo machinery on
Windows):

- `junction_fingerprint_is_backward_compatible` (all platforms): kind name,
  restore support, serde round trip, legacy manifests without the variant.
- `junctions::a_junction_scans_as_a_supported_junction`: classification with
  the reparse data as the record, no traversal into the junction, target
  untouched.
- `junctions::junction_creation_is_captured_and_reversible`: capture → undo
  removes → redo recreates with byte-identical reparse data.
- `junctions::replacing_a_junction_with_a_directory_is_reversible`: both
  directions through the quarantine machinery; the target subtree is never
  touched by the quarantine rename.
- `junctions::an_external_junction_target_is_restored_without_being_followed`:
  outside-root and nonexistent targets recorded and restored literally; the
  missing target is never created; the external target's content is never
  touched.
- `junctions::external_divergence_of_a_junction_refuses_undo`: a post-capture
  retarget refuses undo with no mutation.
- `rollback_path_scan`: junction fingerprints identical between the
  path-scoped and full scans (live + dangling), and neither scan traverses a
  junction.

Deliberate-failure check: the external-divergence test initially failed for
a test-side reason (`mklink /J` refuses to create over the captured
command's plain directory) — fixed by removing the directory in the external
writer's own command; the refusal behavior itself was correct on the first
run.

Known gaps (disclosed in the contract §7, not claimed): an unrecognized-tag
reparse point cannot be manufactured with built-in tooling (branch not
end-to-end testable; unsupported-refusal machinery remains witnessed by the
POSIX unix-socket test); a readonly junction entry cannot be produced with
built-in tooling (`attrib +R` follows the junction), so the readonly-restore
branch is implemented and probe-verified at the mechanism level only, with
the default-attribute path covered end-to-end by every lifecycle test.
POSIX cannot produce the variant; ubuntu/macOS CI runs the two model-level
tests plus the unchanged suites (the junction restore refusal path off
Windows is compile-gated the same way the FIFO path is).

CI: the phase commit `33238bc` (run #45, 2026-09-27) was fully green on
**windows-latest** — including the Tests step that executed all five
junction lifecycle tests on a real MSVC runner — but ubuntu-latest and
macos-latest failed clippy on `variants Junction and Refuse are never
constructed`: the Windows-only `ReparseClassification` variants are
matched on every platform but constructed only by the Windows classifier,
a dead-code finding invisible to the Windows-host clippy (the same
Phase 1.2 pattern). Fixed on `0f96706` with
`cfg_attr(not(windows), allow(dead_code))` on exactly those two variants,
documenting that POSIX's classifier is a constant `Continue`. **Run #46
(`36297995921`) on `0f96706` is green on ubuntu-latest, macos-latest, and
windows-latest** — Format, Compile (all targets), Clippy (-D warnings),
and Tests all success on every job, verified per-job through the GitHub
API.

The 152-test record below describes `7de39b4` before this slice.

---

## Rollback performance phase (path-scoped step verification)

Contract: `.ai/PHASE_ROLLBACK_PERF.md` (measured bottleneck, scope,
invariants, objective, adversarial criteria); decision: ADR-018.

Methodology: `examples/rollback_bench.rs` — deterministic harness seeding N
files, one real captured operation modifying every file, then end-to-end
undo/redo timings plus one bare full scan. Environment: Windows 11, NTFS,
rustc 1.98.1, **debug build** (the historical baseline's profile). The
400-file baseline independently reproduced the historical Phase 2 record
(950.5 s vs the recorded ≈ 15.7 min).

Before → after (same machine, same harness):

| Files | UNDO before | UNDO after | speedup | REDO before | REDO after |
|---|---|---|---|---|---|
| 25 | 3.69 s | 2.05 s | 1.8× | 4.23 s | 2.09 s |
| 100 | 57.26 s | 9.35 s | 6.1× | 48.02 s | 13.24 s |
| 400 | 950.51 s (≈15.8 min) | 74.73 s (≈1.25 min) | 12.7× | 1012.88 s | 70.99 s |

The measured bottleneck: `apply_step` performed two full workspace scans per
step (2N+2 per rollback) while consuming only the affected path's fingerprint
— 82-84% of undo time at every scale. The fix (`scan::scan_fingerprint_at`,
shared per-entry classification with the full scanner) makes the fingerprints
identical by construction; global correctness stays anchored on the final
full-scan state comparison, which is unchanged. The remaining ~75 s at 400
files is journal durability (`synchronous = FULL`) plus actual mutations —
explicitly out of scope.

Scenario and payload variants (same harness and environment, post-optimization
code, debug build; the harness grew `--scenario modify|create|delete|mixed`
and scenario-aware final-state assertions, and every variant asserts exactly N
rollback steps and the correct final content after redo):

| Variant (N=100) | capture | UNDO | REDO |
|---|---|---|---|
| modify, payload 200 B | 0.93 s | 9.35 s | 13.24 s |
| modify, payload 10 240 B | 1.81 s | 7.68 s | 7.88 s |
| create 100 files | 1.21 s | 4.11 s | 3.69 s |
| delete 100 files | 0.88 s | 2.84 s | 4.45 s |
| mixed (34 deleted / 33 modified / 33 created) | 1.10 s | 4.55 s | 4.40 s |

Reading: undo time tracks the *touched* work (steps) rather than total
payload — a 51× payload increase leaves undo at the same order (9.35 s →
7.68 s, within run-to-run noise on this machine), and create/delete/mixed
land in the same seconds band as modify. Pre-optimization equivalents were
not re-measured per scenario (the dominant cost — 2N+2 full scans — is
scenario-independent and was measured directly in the modify table above);
the harness's historical-model line reconstructs it from one bare scan.

Suite after the change (Windows, debug): **152 passed / 0 failed**
(`cargo test --all-targets`), including 3 new
`tests/rollback_path_scan.rs` tests proving fingerprint equivalence across
every object type (files, nested/deep directories, absent paths, and — on
POSIX — symlinks, FIFOs, sockets), live re-observation, plan-time conflict
semantics, and unchanged `RecoveryRequired` behavior. The `rollback_tree`
suite itself dropped from ≈ 45 s to ≈ 10 s as a side effect. POSIX-specific
equivalence runs ride the ubuntu/macOS CI jobs.

CI verified for the phase commits: run #42 on `dd80f6b` and run #43 on
`6ec5e06` (the phase-final tree, 2026-09-26), each completed / success on
ubuntu-latest, macos-latest, and windows-latest, with Format, Compile (all
targets), Clippy (-D warnings), and Tests green per job — the ubuntu and
macOS Tests steps therefore executed the POSIX-only `rollback_path_scan`
equivalence cases (symlinks, FIFOs, sockets) and the POSIX-gated Phase 5
tests. Re-verified locally 2026-09-26 (Windows, rustc 1.98.1): fmt, clippy
`-D warnings`, `cargo test --all-targets` (152 passed / 0 failed), and
`cargo test --doc` all green.

The 149-test record below describes `aae28f4` before this phase.

---

## Phase 5 (platform expansion — POSIX named pipes)

Local suite on Windows (x86_64-pc-windows-gnu, Rust 1.98.1): **149 passed /
0 failed** (`cargo test --all-targets`; fmt, clippy
`--all-targets --all-features -- -D warnings`, `cargo test --doc` green).
The new `tests/phase5_platform.rs` suite runs 1 model-level test
(backward-compatible serde) on every platform and 4 POSIX-gated tests — FIFO
scan classification, FIFO create/undo/redo with recorded mode, FIFO→file
replacement reversibility, and the UNIX_SOCKET refusal — which the local
Windows host **cannot execute**; they are verified by the Linux and macOS CI
jobs (this is the same pattern as Phase 1.1: local cross-checks are exactly
what CI covers). Windows behavior for all pre-existing object types is
unchanged and covered by the unchanged Windows CI job.

Deliberate-failure checks and CI-repair cycles (real defects the POSIX CI
caught that the Windows host cannot even compile):
1. `std::os::unix::fs::mkfifo` is unstable (rust-lang/rust#139324) — lib and
   tests failed to compile on Linux/macOS; fixed by declaring `mkfifo(2)`
   directly (crate FFI site #3, `mode_t` per platform ABI) and creating test
   FIFOs via the `mkfifo` utility.
2. `undo` takes `Option<i64>` (test passed it double-wrapped) and an unused
   unix-only binding — compile errors invisible locally.
3. **A real product hang:** `sync_target` opened any non-symlink target
   read-only to fsync it; `open(2)` on a FIFO blocks until a writer appears,
   so redo/undo that recreate a FIFO hung the POSIX runners until the run
   was cancelled (~6 h). Fixed by never opening a FIFO: its directory-entry
   durability is the parent-directory fsync that was already there.

**CI verified (commit `4d5f242`, run of 2026-09-26): ubuntu-latest,
macos-latest, and windows-latest all completed / success.** The ubuntu
runner's log shows the full `phase5_platform` suite executing: 5 passed /
0 failed — `posix::a_fifo_scans_as_a_supported_named_pipe`,
`posix::fifo_creation_is_captured_and_reversible`,
`posix::replacing_a_fifo_with_a_file_is_reversible` (the FIFO create → undo
→ redo lifecycle with recorded-mode preservation, verified on real Linux),
`posix::unix_sockets_stay_unsupported_and_refuse_undo`, and the
cross-platform serde compatibility test. Windows ran the same suite with the
POSIX module compiled out (1 passed), its object behavior unchanged.

The 148-test record below describes `1f37ec3` before the Phase 5 work.

---

## Phase 4 first slice (time-range history view)

Local suite (x86_64-pc-windows-gnu, Rust 1.98.1): **148 passed / 0 failed**
(`cargo test --all-targets`; fmt, clippy `--all-targets --all-features -- -D
warnings`, and `cargo test --doc` green): 61 lib unit tests (5 `humantime`
incl. adversarial timestamp rejection and non-ASCII panic-safety, 7
`timeline` incl. boundary/tiling/uncertainty/determinism, plus the 49
prior), and a new **7-test `phase4_timeline`** integration suite driving the
real CLI: empty history, half-open boundaries over a real capture, byte-identical
JSON across invocations, uncertainty exposure with byte-identical catalog +
workspace across the read-only view, watcher summary with rotation coverage
and unparseable-line accounting, invalid ranges mutating nothing, and human
output tier honesty.

Deliberate failure checks: the read-only byte-identity assertion was
verified to catch a WAL-checkpoint window (test now checkpoints before
snapshotting); exit-code semantics for malformed bounds were driven from
observed failures to the contract's exit 2.

The 129-test record below describes `c4aeef7` before the Phase 4 work.

---

## Post-Phase-3 audit-fix branch (`fix/phase3-watcher-quoting`)

Local suite on the branch (x86_64-pc-windows-gnu, Rust 1.98.1): **129
passed / 0 failed** (`cargo test --all-targets`, run twice; fmt, clippy
`--all-targets --all-features -- -D warnings`, and `cargo test --doc` all
green): 49 lib unit tests (the 47 from `c881ba3` plus 2 new capacity
tests), 10 boundary_correlation, 13 foundation, 8 hardening,
15 phase2_dependency, **17 `phase3_watcher`** (16 plus a real
detached-spawn end-to-end test), 9 rollback_tree, 8 shell_integration.

New on the branch, beyond `c881ba3`:

- Adapter queue capacity: `poll_reports_overflow_when_a_batch_exceeds_the_pending_capacity`
  (a batch over `PENDING_CAPACITY` delivers its preserved prefix in order,
  exactly once, then reports `Overflow` once, then idles) and
  `serve_loop_records_a_degradation_when_capacity_is_exhausted`
  (production channel → serve loop → durable OVERFLOW record + preserved
  prefix in the dirty index). The exhaustion tests were verified to fail
  with the capacity check disabled.
- Windows detached spawn end-to-end: `detached_spawn_delivers_the_intended_root_to_a_real_child`
  spawns the real binary through the production detached path with a
  `--root` containing a space and a trailing backslash; the child must
  discover the workspace, report a fresh heartbeat, and stop cleanly.
  Verified to fail against the pre-fix `format!("\"{argument}\"")`
  quoting. Note: this test cannot distinguish a *dropped* trailing
  separator (invisible to discovery) — that variant is pinned by the
  byte-exact serialization table, the CRT round-trip decoder, and the
  real-child clap echo test.
- Unicode quoting cases in the serialization table, the CRT round-trip,
  and the real-child clap echo test.

The 119-test record below describes `796ced6` before the fix branch.

---

**Full suite after Phase 3: 119 passed / 0 failed** (`cargo test
--all-targets`, x86_64-pc-windows-gnu, Rust 1.98.1, bash on PATH so the
shell-integration tests ran rather than skipped), **and CI run #36111890265
on `d69fdca` is green on ubuntu-latest, macos-latest and windows-latest**
(after two earlier rounds surfaced one POSIX-only compile defect and three
test-side wait races, both root-caused and fixed — see
`.ai/PHASE_3_VERIFICATION_REPORT.md` §9): 41 lib unit tests (13 Phase 2 +
28 Phase 3), 10 boundary_correlation, 13 foundation, 8 hardening,
15 phase2_dependency, 9 rollback_tree, 8 shell_integration — every Phase 1/2
suite unchanged and green — plus the new **15 `phase3_watcher`** tests.

Phase 3 tests prove, beyond "it runs" (details and evidence in
`.ai/PHASE_3_VERIFICATION_REPORT.md` §5-6):

- **Overflow is injected through a deterministic fake adapter** (contract
  §16): the degradation record is durable immediately, the dirty set
  survives, the watcher marks itself degraded, the next writer opens the
  unknown interval naming the reason, reconcile closes it and consumes the
  marker, and the watcher alone never gates anything.
- Coalescing collapses duplicates into the dirty index while the raw event
  log keeps every event (evidence is never erased); create+delete leaves
  the path dirty; the index serialization is sorted and byte-identical for
  identical inputs.
- The dirty cap degrades instead of growing; events outside the canonical
  root and under `.rewind` are dropped and counted as anomalies.
- Crash/restart and offline semantics: a 13-minute-dead watcher produces a
  `WATCHER_GAP` spanning last-heartbeat→now that gates the next writer and
  closes only via reconcile; a graceful stop leaves everything after it
  unobserved; a first-ever start records nothing.
- Isolation: across a serve loop with events, `metadata.sqlite` is
  byte-identical and the workspace tree fingerprint-identical — the
  watcher writes nothing outside its own store directory.
- Real-adapter lifecycle through the CLI: detached start (which must not
  hang pipe-captured invocations — see the verification report's D2),
  RUNNING via fresh heartbeat, a real file write landing in the index and
  event log, a concurrent writer with no lease interference, bounded stop,
  second-start refusal, and a dead watcher reporting FAILED.
- Measured (contract §16, debug build, NTFS): idle CPU 15.6 ms over 10 s;
  event→index latency 113–208 ms at a 250 ms batch; ~150 bytes per raw
  event; ~19 bytes per dirty path.

Known gaps that remain verification work, not passing claims:

- Platform overflow was injected through the fake adapter, not induced on
  real hardware; the real-adapter overflow path is exercised only by
  construction (the `Rescan` flag mapping), matching the contract's
  allowance for deterministic injection.
- Real power-loss durability of watcher artifacts is not claimed (they are
  advisory; the catalog and journals remain the durable records).

---

## Phase 1.3 status (historical)

The suite uses real temporary workspaces, real filesystem mutations, real
subprocess writers, and real crash simulation. No mocked filesystems.

Passed locally (Rust stable 1.98.1, `x86_64-pc-windows-gnu` host,
NTFS): 36 integration tests, 0 failed.

- 13 foundation tests (portable command vectors: `.cmd` via `cmd /C` on
  Windows, executable `.sh` on POSIX; CRLF/LF handled by `common::echoed`).
- 9 rollback_tree tests (V-F01/V-F02 regressions: non-empty/nested tree
  undo and redo, interrupted rollback recovery, external-modification
  refusals, junction classification, archive verification, CLI e2e).
- 6 shell-integration tests (Phase 1.3, `tests/shell_integration.rs`):
  the shell wrapper exits while a stub 45 s post-hook is still running
  (threshold-free ordering proof; bash everywhere, zsh where the runner
  ships it — windows-latest reports the skip honestly); real-wrapper
  observation/degradation flows; busy catalog, terminated hook process,
  missing workspace, and unwritable CAS all fail open and gate the next
  writer without fabrication.
- 8 hardening tests (Phase 1.2 fixes, `tests/hardening.rs`):
  - lock owner metadata survives a failed contender (in-process, and
    against a real cross-process `rewind run` writer);
  - bounded passive hook fails open on a real 1500-file workspace:
    exit 0 in bounded time, CAPTURE_FAILED + unknown interval recorded,
    undo refused, `reconcile` restores HEALTHY with no fabricated
    STRONG operation;
  - the hook defers unfinished-transaction recovery to the writer and
    records a durable bypass marker that gates reconcile-first;
  - committed-metadata crash window repairs idempotently (journal is
    authority, catalog rows converge, second repair is a no-op);
  - recursive archive verification catches nested corruption, missing
    and extra entries, wrong directory shape, type mismatch, and wrong
    symlink targets (POSIX);
  - v1 manifests (no `target_kind`) deserialize with unknown kind, and
    state ids hash the schema-versioned envelope;
  - recovery failure never leaves a false HEALTHY, and
    `recover --reconcile` archives, marks ABANDONED, and checkpoints;
  - symlinked-parent rollback refusal (POSIX): external target never
    followed (real symlink to outside the workspace);
  - symlink target kinds recorded from evidence and restore faithfully
    (POSIX: file, directory, dangling).

Local gates: `cargo fmt --all -- --check`, `cargo check --all-targets`,
`cargo clippy --all-targets -- -D warnings`, `cargo test` — all passing.

CI (GitHub Actions, `OG-Huzzi/Rewind`): ubuntu-latest, macos-latest,
windows-latest (MSVC host) — fmt/check/clippy(-D warnings)/test per
platform. windows-latest (MSVC) passed in full on the first Phase 1.2
run (compile, clippy, and all 30 tests). Initial runs surfaced one
POSIX-only clippy warning (`unneeded return` in `metadata_fingerprint`,
invisible on the Windows host clippy) — fixed; the final per-platform
results are recorded in `.ai/PHASE_1_2_HARDENING_REPORT.md`.

Measured performance (documented per charter; no performance redesign):

- 400-file tree: snapshot ≈ 2.5 s per invocation (scan + CAS);
  restore/undo of 400 modified files ≈ 15.7 minutes in the debug build
  (per-step full-workspace rescan dominates). Recorded as the Phase 2
  baseline; the Phase 1.2 charter forbids performance redesign.

Known gaps that remain verification work, not passing claims:

- Real power-loss durability is not testable in CI; kill-based crash
  injection and physical quarantine-move simulation are the strongest
  available evidence, plus one real `kill -9` mid-undo of a 300-file
  tree that recovered correctly during Phase 1.1 verification.
- Windows symlink creation is capability-skipped when the runner account
  lacks SeCreateSymbolicLink (GitHub-hosted runners grant it; local
  accounts may not). The graceful-skip path is itself asserted.
- The TOCTOU confinement check narrows the symlink-swap window to the
  interval between the final pre-mutation check and the mutation itself
  (accepted residual race under the Phase 0.7 conditional guarantee).

---

## Phase 1.1 status (historical, 2026-09)

Passed: 22 real-filesystem tests.

- 13 foundation tests (Phase 1 suite).
- 9 rollback_tree regression tests: non-empty/nested/large tree undo and
  redo, interrupted recursive rollback recovery, external-modification
  refusals, junction classification, archive verification with staging
  cleanup, CLI parse/e2e coverage.

Gates at that time: fmt/check/clippy(-D warnings) passed; tests 22/22 on
`x86_64-pc-windows-gnu`; Linux/macOS cross-checks could not compile
(bundled SQLite C cross-toolchain absent) and MSVC was unavailable —
which is exactly what Phase 1.2 CI now covers.
