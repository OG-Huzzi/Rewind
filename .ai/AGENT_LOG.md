# Agent Log

## Phase 7 (release engineering preparation) — implemented; release gated

- Baseline verified at clean `main` / `origin/main` `36cfeba`; no tags and
  no license file. The stale `license = "MIT"` metadata was not treated as
  a legal decision.
- Committed the Phase 7 contract and ADR-023 before production-facing
  changes. Kept version `0.1.0`, removed unsupported license metadata,
  added accurate Cargo metadata and an explicit crate allowlist, rewrote the
  README with evidence and conditional claims, and added descriptions for
  every real CLI command plus a help-metadata unit test.
- Added the `v*`-tag-only release workflow with a read-only version/license
  preflight before old Release deletion, native Linux/macOS/Windows gates
  before builds, target-derived archive names, full-SHA-pinned actions, and
  one Release aggregation job. Static YAML/structure checks passed. No tag
  or release was created.
- Local fmt/check/Clippy gates pass. Two consecutive final-source full test
  runs passed 188/0. A previous Windows GNU full-suite process terminated
  with `STATUS_HEAP_CORRUPTION`; isolation and repeated Phase 6 runs passed,
  but the cause was not determined and remains a recorded limitation.
- Audited all 30 visible CLI help pages and three malformed/error paths.
  A fresh-root install and README quick start succeeded through capture,
  undo, redo, and recovery. Package listing contains only the declared
  source boundary and Cargo-generated metadata (with the expected warning
  that the license is absent).
- Pushed to `main` at `6591600`; GitHub Actions CI run #65 passed on
  Ubuntu/macOS/Windows. A fresh remote clone at that commit installed with
  Cargo and passed the README capture/undo/redo/recovery sequence.
- Remaining: stop before tagging until the owner provides license terms,
  the license file, and matching Cargo metadata. Collect 1.0 evidence
  without declaring the version; crates.io remains gated on an owner token.

## Phase 6 (manifest/lockfile evidence layer) — executed

- Baseline verified: `main` at `77567e9` = `origin/main`, tree clean; CI
  green on the baseline (check-runs read via the public API); local suite
  166/0. Push access confirmed by dry-run.
- Audit reconciled a documented-vs-code discrepancy: the repository has no
  schema-migration mechanism (Phase 2 recorded this); Phase 4's
  `created_at` was never an additive migration — it existed since the
  initial schema. The established extension pattern is the idempotent
  `CREATE TABLE IF NOT EXISTS` DDL in `Catalog::initialize()`; Phase 6
  follows it (no `ALTER TABLE`).
- Probes executed on the host (Windows 11, NTFS) and recorded in the
  contract §3: cargo 1.98.1, npm 11.6.2, pnpm 11.3.0, go 1.26.5, python
  3.14.4/pip 26.2.1, uv 0.11.26; each manager's real operation observed
  inside a Rewind capture (lockfiles appear as ordinary RegularFile
  entries); offline-safe variants probed; the npm probe's nested
  `node_modules/.package-lock.json` is the depth-policy witness. Not
  installed: yarn, bun, poetry, pipenv → those ecosystems stay
  unrecognized (no-guess rule).
- Contract `.ai/PHASE_6_RECIPES.md` + ADR-022 committed (`e2d18b1`)
  before any production code; pushed.
- Implementation commit `94adeaa`: `src/recipes.rs` (pure recognition),
  `RecipeKind`/`RecipeEvidence` in model.rs, `operation_evidence` table +
  transactional writes + reads in db.rs, capture integration in
  workspace.rs, presentation in cli.rs/timeline.rs (schema version
  deliberately kept at 1). Test commit in the same push:
  `tests/phase6_recipes.rs` (12 integration tests incl. real-manager
  cargo+npm with CAS-verified hashes and honest tool-absent skips).
- Review fix `8002933`: clock-independent determinism ranges (the fixed
  calendar range would have rotted in 2027) and the missing
  `inspect history --json` surface assertions. No production change.
- Gates: fmt/check/clippy `-D warnings` green; full suite 187/0 twice
  consecutively.
- CI run on `7efbc14` failed on all three platforms at the Format step:
  REAL failure, not a flake. Root cause: the review-fix commit's scripted
  edit introduced an over-long line and the fmt gate was not re-run after
  it (only tests were) — a process defect in the review step, recorded
  honestly in TEST_STATUS. Fixed: fmt applied (`126baaf`), default
  toolchain updated to 1.99.0 (the version CI's `@stable` resolves to),
  all gates re-run green (suite 187/0 twice).
- CI run #62 on `156b039`: macOS green; ubuntu/windows failed with two
  further REAL test-environment defects, fixed in `b058cdb`: (1) POSIX
  script bodies lacked shebangs → ENOEXEC on ubuntu (macOS fell back —
  non-uniform); the helper now prepends `#!/bin/sh` (house convention,
  documented on tests/common::shell_script). (2) The passive test raced
  the hook's 50 ms scan deadline on windows; it now accepts both
  documented post-hook outcomes and asserts the evidence property in
  each branch.
- CI run #63 on `b058cdb`: **green on ubuntu/macOS/windows** (workflow
  run 36981061095). Phase 6 COMPLETE; release engineering is the next
  planned phase.

## Phase 5 close-out / CI stabilization — executed

- Contract `.ai/PHASE_5_CLOSEOUT.md` committed (`8617948`) before code.
- Root causes: (1) overlap test lost a hook to the 2 s lease-retry budget
  under runner load (claimed boundary + bypass marker + exit 0, no
  operation); (2) the rapid test's last post-hook spawn depended on an
  end-of-input PROMPT_COMMAND cycle that buffered interactive bash
  delivers unreliably (probe: 0 cycles); (3) rollback_tree's captured
  flag swallowed capture_error.
- Fixes: `REWIND_HOOK_LEASE_RETRY_MS` tuning knob (default unchanged;
  bypass fallback unchanged and still covered); claim barrier replaces
  the sleep; trailing sentinel command guarantees the spawn; assertion
  diagnostics. No assertion weakened anywhere.
- Signoff: `.ai/PHASE_5_VERIFICATION_REPORT.md` — Phase 5 CLOSED with the
  final capability matrix state; recommended next phase: ecosystem
  recipes (ADR-016 deferral), then release engineering.
## Phase 5.4 execution (slice 4: explicit NTFS DACL ACEs) — implemented

- User approved the Windows-DACL candidate after the audit below.
- Probe round 2 (scratch tool, not committed) through the implementation
  APIs: explicit-only rebuild + `SetNamedSecurityInfoW(UNPROTECTED)`
  reproduces the SDDL byte-identically (inherited ACEs re-derive); the
  protected flag round-trips (`D:PAI`, control 0x9404); `fs::rename`
  preserves the DACL; **`fs::copy` drops explicit ACEs** (archive re-apply
  required); DACL apply works on readonly files; a protected deny-all DACL
  can lock out even the owner (reads must degrade honestly).
- Contract `.ai/PHASE_5_NTFS_DACL.md` + ADR-021 + matrix committed
  (`cb3f963`) before implementation; implementation commit `d5b84fa`.
  One contract amendment during implementation (documented in the contract
  status): a NULL DACL (`SE_DACL_PRESENT` set, null pointer —
  allow-everything) is a named `ScanIncomplete` error, not `None`.
- Empirical testing findings (recorded in TEST_STATUS): icacls deny masks
  include SYNCHRONIZE (denies even reads → honest degradation; tests use
  grants); `/inheritance:r` produces protected-empty deny-all while
  `/inheritance:d` copies ACEs (the testable protected form); icacls
  replaces an existing trustee's grant.
- Gates: fmt, check, clippy `-D warnings`, `cargo test --all-targets`
  166/0 (also under `--all-features`), bench harness sanity at N=9.
## Phase 5.4 audit (candidate selection) — STOPPED pending approval

- Baseline verified: `main` at `642577e` = `origin/main`, tree clean; slice-3
  (ADS) CI-verified (runs #49/#50 green on all three platforms).
- Deferred-capability traces searched in `src/`/`tests/`: **none** for xattrs,
  ACLs, sparse, or chflags — the slice-1 matrix's deferral list is accurate.
  `STATE_SCHEMA_VERSION` is 2; no migration logic exists.
- Host probe (Windows 11 build 26200, Git Bash/MSYS only, **non-admin**):
  **WSL is NOT installed** (`wsl.exe --status` errors), Docker absent,
  `getfattr`/`setfattr` absent. → **POSIX xattrs cannot be probed locally.**
- Windows DACL probe on throwaway temp files (no repo changes): SDDL read
  without admin works (`Get-Acl`); an explicit deny ACE round-trips
  (`icacls /deny Everyone:(R)` → `(D;;FR;;;WD)` appears in SDDL with no `ID`
  flag; `/remove:d` restores the original SDDL byte-for-byte). Environment
  ACEs are dominated by `ID`-flagged *inherited* ACEs → a recordable state
  must be **explicit ACEs + protected/auto-inherit control flags only**.
  Open probe question: whether `CopyFileW` (archive path) carries explicit
  ACEs (docs suggest not) — decides archive re-apply vs refusal.
- Candidate verdicts: xattrs **blocked locally** (probeable only via a
  CI-runner probe commit or after WSL2/Docker install); **Windows DACLs
  (explicit-ACE set) fully probeable locally** — recommended candidate;
  ownership/sparse/chflags remain deferred for the recorded reasons.
- Stale-doc discrepancy found: `.ai/PROJECT_CONTEXT.md` status line still
  stops at slice 2 (to fix with the phase's doc pass).
- **Workflow state: STOPPED after audit per Phase 5.4 §15 — no code, no
  contract, no implementation until the candidate is approved.**

## Phase 1 start

- Re-read the authoritative architecture and Phase 1 contract.
- Confirmed no existing source code, Cargo project, or Git metadata.
- Installed Rust stable 1.98.1 because the environment had rustup but no
  installed/default toolchain.
- Chose a single deterministic Rust package with separate library modules and a
  thin CLI.
- No architecture semantics have been changed.

## Verification pass

- GNU Windows-target check and Clippy pass.
- Full integration suite expanded to 12 real-filesystem tests and passes.
- Windows symlink test records the unavailable-privilege capability instead of
  treating the environment as a filesystem failure.
- Remaining gate: complete cross-platform and journal-boundary matrix review;
  MSVC linker is unavailable in this environment.

## Final verification pass

- `fmt --check`, GNU Windows-target `check`, Clippy with `-D warnings`, and all
  targets passed.
- 13 real-filesystem tests passed, including known partial rollback recovery.
- CLI help rendered successfully.
- Phase 1 remains incomplete because the full 38-case and native platform
  matrix was not executable in this environment.
