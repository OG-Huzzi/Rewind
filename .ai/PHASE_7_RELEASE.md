# Phase 7 Contract — Release Engineering

Status: **CONTRACTED; RELEASE BLOCKED pending the owner's license decision.**
The repository contains no license file. Preparation that does not select a
license or publish artifacts may proceed; no release tag or GitHub Release is
permitted until the owner supplies a license file and matching Cargo metadata.

## 0. Verified baseline and readiness audit

At the start of this phase, `main` was clean at `36cfeba280b6cadcd975e1511eec38a4f8ba36e8`, equal to `origin/main`. There were no tags. The remote is `https://github.com/OG-Huzzi/Rewind.git`.

| Surface | Baseline finding |
|---|---|
| Crate metadata | `version = 0.1.0`, `edition = 2021`, and a one-phrase description exist. `license = "MIT"` is present without a corresponding license file and must be removed until the owner decides. `repository`, `readme`, `keywords`, `categories`, and `rust-version` are absent. CI uses the stable toolchain; the local stable compiler observed during this audit is Rust 1.98.1. |
| License | No `LICENSE`, `LICENCE`, or `COPYING` file exists anywhere in the repository. This is a release stop condition. The phase does not choose or create one. |
| Existing README | It says the project is at Phase 3; says Windows junctions are refused/unsupported; and says large rollback optimization is deferred to Phase 2. Those statements are stale: Phase 5 added supported Windows junctions and Phase 6 was delivered; ADR-018 already measured and improved rollback verification. The README also lacks the final Phase 5 capability matrix and current release/install information. |
| CLI cold start | Default `cargo run --quiet -- --help` could not link because this host has no MSVC `link.exe`. The installed GNU toolchain and GCC worked: `cargo +stable-x86_64-pc-windows-gnu run --target x86_64-pc-windows-gnu -- --help` built the current source. `--version` reported `rewind 0.1.0`; all 19 visible top-level commands and all visible nested command help pages exited 0. |
| Install path | `cargo +stable-x86_64-pc-windows-gnu install --path . --root <fresh-temp-root> --locked` succeeded. The installed binary reported `rewind 0.1.0`, initialized a scratch workspace, captured a file creation, undid it, redid it, listed/showed operation 1, and `recover` reported a healthy workspace with no unfinished transaction. The unqualified MSVC path remains unavailable on this host because `link.exe` is absent; this is an environment limitation, not an install-path failure. |
| Crate contents | Baseline `cargo package --list` succeeded with warnings that documentation, homepage, and repository metadata were missing. It included `.ai/`, `.github/`, `examples/`, `integration/`, `phases/`, and `tests/`. The package boundary will be explicit and will omit internal planning/verification material, test fixtures, examples, and shell integration files. |

The earlier Phase 6 CI evidence is recorded in `.ai/TEST_STATUS.md`; CI for each final Phase 7 commit must still be observed and recorded before calling the phase verified.

## 1. Scope and boundaries

This phase prepares release readiness: crate metadata; a user-facing README;
a tag-only GitHub release workflow with Linux, macOS, and Windows binary
archives; version/tag rules; and fixes for factual or operational defects
found in the first-user surface audit.

Out of scope: code signing and notarization; Homebrew, Scoop, Chocolatey,
winget, or other third-party package managers; crates.io publication (deferred
until the owner supplies a publishing token); `.msi`, `.deb`, or `.pkg`
installers; a website; and product feature work. Product behavior stays fixed
unless the surface audit finds a defect; any safety-design change stops for
owner review.

The system boundary remains unchanged: Rewind is not a kernel monitor, a
version-control replacement, a remote-side-effect reverser, a system-wide
transaction layer, or a command re-runner. Redo applies recorded state; it
never runs the captured command again. Passive hooks and the watcher have
different confidence levels and remain advisory/conditional as their existing
contracts specify.

## 2. License gate and package metadata

No one implementing this phase may choose a license, copy a license text, set
an SPDX license identifier, set `license-file`, or create a release while no
owner-selected license file exists. Remove the current unsupported `MIT`
manifest claim. Mark the README with an explicit owner-decision placeholder.
The owner must choose and provide the license file; only then may the manifest
field be set to that file's actual SPDX identifier or `license-file` path.

The remaining metadata is: one-sentence description; repository URL;
`readme = "README.md"`; useful keywords and valid Cargo categories; existing
edition 2021; and `rust-version = "1.98.1"`, justified by the observed stable
1.98.1 toolchain used for local validation while CI installs stable. Keep the
version at `0.1.0` for the first release. Use an explicit Cargo `include`
allowlist containing the manifest, lockfile, README, and `src/**`; add the
owner-supplied license file to that allowlist when the gate is resolved. This
keeps `.ai/`, workflow files, test fixtures, examples, `phases/`, and shell
integration scripts out of the packaged crate. Verify the resulting contents
with `cargo package --list`.

## 3. Version and tag policy

The first release, once unblocked, is an annotated `v0.1.0` tag on the
release-readiness commit; do not invent a version bump for packaging alone.
Every later tag must exactly match `Cargo.toml`'s version with a leading `v`.
When a version changes, update the manifest, lockfile as applicable, README,
changelog, and release documentation together. Never call the project `1.0.0`
or create a `v1.0.0` tag without the owner's explicit decision. The evidence
for that decision is collected in `.ai/RELEASE_1_0_EVIDENCE.md`; evidence of
technical readiness informs the owner but does not make the decision.

## 4. Release workflow design

Add `.github/workflows/release.yml` with only `push.tags: ["v*"]` as its
trigger. Do not change or weaken `.github/workflows/ci.yml`, which remains the
authority for normal pushes and pull requests.

For each of `ubuntu-latest`, `macos-latest`, and `windows-latest`, run the CI
gates in this order: `cargo fmt --check`; `cargo check --all-targets`; `cargo
clippy --all-targets --color never -- -D warnings`; and `cargo test --color
never`. Only after those gates pass may that job run `cargo build --release
--locked`, verify that the binary's version equals the tag without `v`, and
package it. The target name comes from that runner's `rustc -vV` host field,
so the asset names describe the binary actually built:

- Unix: `rewind-v<version>-<rust-target>.tar.gz`
- Windows: `rewind-v<version>-<rust-target>.zip`

Each archive contains only `rewind` (or `rewind.exe`) and a short
`INSTALL.txt` with extraction/PATH instructions and a pointer to the README.
Each matrix job uploads its archive as a workflow artifact only after all
gates pass. One release job waits for all three jobs, downloads their
artifacts, and attaches all three to one GitHub Release. The workflow uses
only the standard `GITHUB_TOKEN`; build jobs have `contents: read`, and only
the release preparation/creation jobs receive `contents: write`.

Serialize runs for the same tag. At the start of a tag run, if a GitHub Release
already exists for that tag, delete that release while retaining the git tag;
this ensures a failed rebuild cannot leave old release assets presented as a
successful new run, and a successful rerun creates a fresh release with exactly
the three current archives. Re-tagging an already-consumed public version is
not allowed; follow the no-consumers re-tag rule in the handoff. If deletion,
build, upload, or release creation fails, the run is failed and the tag is not
reported as a release.

All actions in the release workflow use verified full commit SHAs. The
prepared pins are `actions/checkout@11d5960a326750d5838078e36cf38b85af677262`
(v4.4.0), `actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02`
(v4.6.2), and `actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093`
(v4.3.0). Rust is installed with the hosted runner's `rustup` rather than an
additional action. Review and update a pin only from its upstream repository.

## 5. README contract and claim evidence

The README is the front door for a reader who has not read `.ai/`. It must
contain: a concise product/boundary description; conditional capabilities and
their evidence; the exact Phase 5 object/platform table; refusals and
deferred work; prerequisites and clone/install instructions; the tagged
binary download naming scheme (marked unavailable until the license gate is
resolved); a runnable quick start; and an explicit license placeholder until
the owner decides.

Every guarantee must identify its supporting contract and, where applicable,
the test suite that exercises it, or be removed. In particular, describe
strong capture as filesystem state inside an explicitly initialized
workspace; undo/redo as conditional on known recorded states and supported
objects; passive observation as lower-confidence; degradation and unknown
intervals as gates; single-writer enforcement; local quarantine/external
archive; and recovery as conditional on available evidence. State that
unsupported objects are refused with named reasons, unknown intervals stop
attribution, remote/system-wide effects are not reversed, redo does not rerun
commands, and the watcher is advisory. No roadmap promises, superlatives, or
claims stronger than the code/contracts/tests.

The platform table must match `.ai/PHASE_5_VERIFICATION_REPORT.md` exactly:
regular files (POSIX modes, plus Windows named NTFS streams and explicit
DACLs on regular files), directories, inside-root creatable symlinks, POSIX
FIFOs, Windows junctions, recorded/refused objects, and each deferred class
with its blocker. README command examples must be checked against actual
`--help` output. Install instructions may only claim the paths actually
executed or verified by final CI.

## 6. Surface audit policy

Defects include factual help/README mismatches, missing guidance for a
first-user flow, cold-start failures attributable to Rewind, unreadable or
misleading errors, and panics for malformed input. Cosmetic-only wording with
no factual or operational effect is documented without churn. Run every
visible command and nested-command `--help`, `--version`, the README quick
start, install smoke tests, and several malformed/missing-argument/path cases.
Fix a real defect only in a small tested commit; do not change a safety
decision under this phase.

## 7. Acceptance criteria and verification plan

- **AC1 — Baseline/audit:** record branch, tree, HEAD/remotes/tags and each
  readiness finding in this contract.
- **AC2 — License gate:** preserve the owner decision; until a real license
  file and matching manifest metadata exist, create no tag or release and
  publish no artifacts.
- **AC3 — Contract-first:** this contract and ADR-023 are committed together
  before production metadata, README, or workflow edits.
- **AC4 — Metadata/package:** all non-license metadata is accurate; no license
  field remains before the owner decision; `cargo package --list` contains
  only the declared allowlist plus the owner license after it exists.
- **AC5 — README:** every requested section and caveat is present, every
  material claim cites contract/test evidence, platform states match Phase 5,
  and the runnable quick start succeeds on the host where it is claimed.
- **AC6 — CLI:** all visible root/nested help pages and version are observed;
  examples match parser behavior; malformed inputs produce errors without
  panics.
- **AC7 — Install:** install from a clone into a fresh temporary root and
  smoke-test version, init, capture, undo, redo, and recovery.
- **AC8 — Workflow:** only version tags trigger it; exact CI gates pass on all
  three runner OSes before each build; build uses `--release --locked`; full
  SHA pins and minimal token permissions are present; one release receives
  exactly the correctly named platform archives.
- **AC9 — Release evidence:** after AC2 is resolved, create the annotated
  `v0.1.0` tag, observe the workflow and Release, download all three archives,
  inspect the two non-host archives, and smoke-test the host archive through
  init/capture/undo. Do not claim this criterion before it is actually run.
- **AC10 — 1.0 evidence:** add `.ai/RELEASE_1_0_EVIDENCE.md` separating
  verified capability/test/CI evidence from owner decisions and deferred
  work.
- **AC11 — Review/docs:** hostile review covers claims, permissions, tag
  trigger, stale assets, package contents, and version consistency; update the
  phase contract, ADR index, roadmap, current state, handoff, test status,
  TODO, changelog, and agent log.
- **AC12 — Final verification:** local fmt/check/clippy/full suite pass; if
  tests change, run the full suite twice consecutively; final commit CI is
  green on all three OSes; release CI/artifacts are verified only after the
  license gate is resolved. Push directly to `main`; report exact blockers.

## 8. Alternatives, consequences, and revert

Alternatives rejected: choosing MIT based on the stale manifest (legal
decision and no file evidence); bumping to 1.0 for packaging (owner decision);
publishing separate releases from concurrent matrix jobs (race/partial
release); uploading untested binaries; shipping `.ai/` material in the crate;
and using floating action branches/tags without a full-SHA review.

Consequences: product behavior and runtime dependencies stay unchanged;
packaged source is intentionally narrower than the repository; releases are
host-native binaries and do not claim signing/notarization; crates.io remains
deferred pending an owner token; and the absence of a license blocks public
release until resolved.

Revert by reverting the release workflow/metadata/README commits and this
contract/ADR in reverse order. If a tag was created, delete its GitHub Release
and tag only when it has no external consumers; assets can be rebuilt from the
same source commit. Never rewrite branch history or force-push.
