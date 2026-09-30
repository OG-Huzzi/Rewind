# Agent Log

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
