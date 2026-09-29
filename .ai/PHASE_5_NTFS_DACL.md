# Phase 5 Contract Amendment — Slice 4: Windows NTFS DACLs (explicit ACEs)

Status: contract for this slice (Phase 5.4). The mechanics below were
**probe-verified on the real host (Windows 11 build 26200, NTFS, non-admin)
through the exact APIs the implementation uses** — `GetNamedSecurityInfoW`,
`SetNamedSecurityInfoW`, `ConvertSidToStringSidW`,
`ConvertSecurityDescriptorToStringSecurityDescriptorW`,
`GetSecurityDescriptorControl` — before this contract was written. The probe
was a scratch tool and is not committed.

Baseline: `main` at `a4bf08c` (Phase 5 slice 3 CI-verified, runs #49/#50
green on ubuntu/macOS/Windows).

## 1. Selection (exactly one capability)

The Phase 5.4 audit (`.ai/AGENT_LOG.md`, 2026-09) evaluated the slice-1
deferred list against real host capabilities:

| Candidate | Faithful capture+restore | Probeable in the working environment | Verdict |
|---|---|---|---|
| **Windows NTFS DACLs (explicit ACEs)** | Yes — explicit (non-inherited) ACEs plus the protected/auto-inherit control flag, round-tripped SDDL-identically | **Yes — fully, locally, without admin** (probe evidence in §2) | **selected** |
| POSIX xattrs | Yes in the `user.*` namespace | **No POSIX host exists** (no WSL, no Docker, no xattr tools; non-admin). Per the phase gate, not implementable without a real probe. Deferred with this recorded blocker | deferred |
| Ownership (uid/gid, Windows owner) | Requires privilege assumptions CI cannot verify (the matrix's own recorded reason) | no | deferred |
| Sparse-file layout | Kernel/allocation presentation, not content identity | partially | deferred |
| chflags | macOS-only | no macOS host | deferred |

DACls close the Windows analogue of the slice-1/2/3 fidelity gaps: a file's
explicit permission ACEs are invisible to state identity today, so undo
restores a file from staged content alone and silently drops recorded
`icacls`-level permission state.

## 2. Probe-verified mechanics (the facts this contract is built on)

- Reading owner+group+DACL via `GetNamedSecurityInfoW` and the control word
  via `GetSecurityDescriptorControl` works **without admin** for files the
  user owns. Baseline control word `0x8404` =
  `SE_DACL_PRESENT(0x4) | SE_DACL_AUTO_INHERITED(0x400) |
  SE_SELF_RELATIVE(0x8000)`.
- Every environment ACE on this host (and in general on NTFS with inherited
  ACLs) carries the `INHERITED_ACE` flag (0x10). Explicit ACEs (added via
  `icacls /deny` or `/grant`) carry no such flag — the classification is
  programmatic and exact: a deny ACE read as
  `type=1 flags=0x00 mask=0x00120089 sid=S-1-1-0` beside nine `ID`-flagged
  inherited allows.
- **Round-trip is byte-identical.** Applying a rebuilt ACL containing only
  the explicit ACEs with `SetNamedSecurityInfoW |
  UNPROTECTED_DACL_SECURITY_INFORMATION` re-derives the inherited ACEs from
  the parent and the resulting SDDL is **identical** to the pre-restore SDDL
  (verified twice: with an explicit deny, and after removing it). ACL read
  order is stable across set.
- **The protected flag round-trips.** Applying the explicit set with
  `PROTECTED_DACL_SECURITY_INFORMATION` yields `D:PAI(...)` and control
  `0x9404` (`SE_DACL_PROTECTED` 0x1000); only the explicit ACEs remain.
- **`fs::rename` preserves the DACL** — quarantine is safe (same evidence
  class as the junction and ADS slices).
- **`fs::copy` (CopyFileW) DROPS explicit ACEs** — a copy of a file with an
  explicit allow ACE had SDDL ≠ source (the copy was all-inherited). The
  archive path must re-apply the source's explicit DACL to the copy before
  verification.
- **`SetNamedSecurityInfoW` works on a readonly file** — no ordering
  constraint against the readonly attribute (unlike stream writes).
- **A protected DACL can lock out even the owner.** A file with
  `D:PAI(A;;0x100116;;;WD)` (Everyone write-only, nothing else) denied
  `fs::copy`'s read of the source. Reading a security descriptor can
  therefore fail on a workspace file that Rewind could otherwise scan.

## 3. Scope

**In scope:** the explicit (non-`INHERITED_ACE`) DACL ACEs and the
protected/auto-inherit control flag of **regular files** on Windows/NTFS,
captured, compared, restored, and verified as part of the file's
fingerprint.

**Supported:** Windows on NTFS. On filesystems without security descriptors
(FAT-family) a NULL DACL reads back as "no explicit DACL" — there is no
explicit state to record and nothing to be unfaithful about. A read that
*fails* (access denied, I/O error) is never treated as "no DACL": it is a
`ScanIncomplete` degradation (see §6).

**Explicitly out of scope (documented, not silent):**

- **Directory explicit DACLs.** Real NTFS state, deliberately deferred to a
  later slice to keep this slice narrow — the same documented-deferral
  pattern as directory streams. Files-only.
- **Owner, group, and SACL** (audit ACEs). Owner/group changes are
  privilege-bound (SeTakeOwnershipPrivilege); SACL reading requires
  SeSecurityPrivilege. Never captured, never restored.
- **Inherited ACEs.** Parent-derived, not file state — recorded nowhere and
  never compared (recording them would make fingerprints
  parent-path-dependent). On restore with UNPROTECTED they re-derive from
  the live parent (probe-verified); inherited-ACE drift is therefore not a
  fidelity failure of this slice.
- **Junctions, symlinks, and all reparse points** never get DACL capture or
  restore: they are classified before the regular-file branch, exactly like
  stream enumeration. Only plain regular files are touched.
- **POSIX:** the scanner can never produce a DACL record; a foreign manifest
  carrying one is refused at materialization and never matches a quarantined
  artifact — mirroring the FIFO/junction/streams refusals.
- **Non-simple ACE types.** DACL ACEs of types other than allow (0) and deny
  (1) — e.g. object ACEs (5/6) with GUID bodies — are a named scan error,
  never a guessed parse (their body layout differs; parsing them as simple
  ACEs would corrupt the SID).

## 4. Capture semantics

- `Fingerprint::RegularFile` gains
  `dacl: Option<DaclFingerprint>`,
  `#[serde(default, skip_serializing_if = "Option::is_none")]`.
- `DaclFingerprint { protected: bool, aces: Vec<DaclAce> }` with
  `DaclAce { ace_type: u8, flags: u8, mask: u32, sid: String }`:
  - `ace_type`: 0 (allow) or 1 (deny); anything else is a scan error (§3).
  - `flags`: the ACE's flags with `INHERITED_ACE` (0x10) stripped — recorded
    verbatim otherwise (object/container-inherit bits are meaningful on
    directories and are simply usually zero on files).
  - `mask`: the access mask, verbatim.
  - `sid`: the trustee SID in string form (`ConvertSidToStringSidW`),
    recorded verbatim — never resolved to an account name, never assumed to
    exist on the restoring machine (literal-restore semantics, like
    junction names).
- ACE order is the ACL's own order (probe-verified stable); it is part of
  the record because ACE evaluation order is semantics, not presentation.
- **When is the field `None`?** When the DACL is absent (`SE_DACL_PRESENT`
  clear / null DACL pointer) or present-but-unprotected with zero explicit
  ACEs (behaviorally "inherit everything" — nothing to record). A
  **protected** DACL with zero explicit ACEs is `Some` — that state is
  "deny all", which is real state and must survive.
- **No drift, no schema bump:** a file without an explicit DACL serializes
  byte-identically to the pre-slice form (the field is skipped), so existing
  workspaces' state ids are unchanged. A file *with* an explicit DACL
  previously compared equal to itself without one; the first scan after
  upgrade makes state identity strictly stronger, resolved by the existing
  reconciliation checkpoint — the same upgrade argument as ADR-017/019/020.
- Capture failure (permission, I/O — including the probe-verified
  owner-lockout case): `ScanIncomplete` — the workspace degrades honestly;
  no guessed DACL, no silent omission.
- DACLs are **not CAS content** (no bytes are stored; only the structured
  record). Nothing enters the CAS for this slice; anchors and `doctor`
  therefore need no new verification steps.

## 5. Restoration semantics

Ordering inside the existing per-step machinery:

1. Quarantine rename of the replaced object (probe-verified: the DACL
   travels with the file).
2. Install the staged content, `verify_mutation_confined(leaf_exists =
   true)`, `apply_metadata` (readonly included).
3. If the desired fingerprint records a DACL: rebuild an ACL from exactly
   the recorded ACEs (SID strings re-materialized via
   `ConvertStringSidToSidW` — well-formed SID strings convert even when no
   account resolves) and apply with `SetNamedSecurityInfoW`, using
   `PROTECTED_DACL_SECURITY_INFORMATION` when `protected` is recorded and
   `UNPROTECTED_DACL_SECURITY_INFORMATION` otherwise. Works on readonly
   files (probe-verified). If the desired fingerprint records **no** DACL
   but the freshly installed file carries explicit ACEs (impossible through
   Rewind's staging — a fresh file inherits only — but possible if staging
   was interrupted and re-classified), nothing is applied; the post-apply
   verification below still compares the full fingerprint.
4. The step's post-apply path-scoped re-scan compares the **complete
   fingerprint** — content hash, size, metadata, streams, and the recorded
   DACL — so a wrong or partially applied DACL fails the step into
   `RecoveryRequired` through the existing machinery.

Crash between the `Applying` journal write and step completion (including
mid-DACL-apply): recovery classifies exactly as for a half-installed
regular file today — the journal lands `RecoveryRequired`; the quarantined
original (DACL intact, probe-verified through rename) is the recovery
record; `recover --reconcile` remains the documented exit. Idempotent
re-application after an interrupted-but-classifiable step behaves like every
other object type.

If `SetNamedSecurityInfoW` fails (rights, privilege, external lock), the
step fails — never a success report with an unapplied DACL.

## 6. Safety and uncertainty

- **No-follow:** DACL capture happens only for objects already classified as
  regular files by `symlink_metadata` (reparse points report
  `is_file() == false`); DACL apply happens only after the leaf is verified
  a real regular file under real parent components. The residual TOCTOU
  window is the documented Phase 0.7 conditional guarantee, unchanged.
- **No silent loss:** the explicit DACL is part of state identity before any
  mutation is journaled; quarantine carries it; the archive re-applies and
  verifies it (below); restoration that cannot reach the recorded DACL
  fails the step rather than reporting success.
- **No guessing:** a failed security-descriptor read is `ScanIncomplete`
  (the owner-lockout case is real — probe §2); a non-simple ACE type is a
  named scan error; a NULL DACL is recorded as `None` (that is what the
  object *is*, not a guess); foreign DACL records off-Windows are refusals.
- Journal durability, writer serialization, reconciliation authority, and
  watcher advisory semantics are untouched. The single-writer protocol and
  the path-scoped step verification (ADR-018) are reused unchanged.

## 7. Archive behavior

`fs::copy` drops explicit ACEs (probe-verified), so `copy_artifact`
re-applies the **source's live explicit DACL** to the archive copy
immediately after copying, before verification.
`verify_archive_pair` additionally compares, on Windows, the explicit-ACE
sets and protected flags of quarantined source and archived copy —
`ArchiveStatus::Archived` is still only set after full verification, so a
permission-stripping copy can never authorize disposal of the last
surviving copy. `artifact_matches_fingerprint` (quarantine-backup
verification and recovery classification) also compares the recorded DACL
against the quarantined object's live explicit DACL on Windows, and never
matches a foreign fingerprint that claims a DACL off-Windows.

## 8. Testable acceptance criteria

- **AC1 (model, all platforms):** a legacy `RegularFile` without the field
  deserializes; a DACL-free file serializes byte-identically to the
  pre-slice form (no drift); a DACL-carrying fingerprint round-trips
  through serde, including the protected-empty (deny-all) case.
- **AC2 (Windows):** a file with an explicit deny and/or allow ACE scans
  with exactly those ACEs recorded (type, flags, mask, SID string) and the
  unprotected control flag; a plain file scans with an omitted (`None`)
  DACL.
- **AC3 (Windows):** an external `icacls` grant/deny and an
  `/inheritance:r` (protected) change are captured; undo restores the exact
  recorded explicit ACE set and control flag (verified by re-read); redo
  restores the post-state; the protected-empty (deny-all) state round-trips.
- **AC4 (Windows):** an external DACL modification after capture refuses
  undo before any mutation (conflict), exactly as for content.
- **AC5 (Windows):** the archive copy of a DACL-carrying quarantined file
  carries the same explicit ACE set (re-applied by `copy_artifact`) and
  `verify_archive_pair` compared it before marking `Archived`.
- **AC6 (Windows):** the path-scoped fingerprint equals the full-scan
  fingerprint for DACL-carrying files (rollback-perf invariant preserved).
- **AC7 (POSIX/all):** the full existing suite stays green; DACL records
  never appear off-Windows; restore of a foreign DACL-carrying fingerprint
  is refused with a named error.

## 9. Known testing gaps (disclosed, not claimed)

- macOS/Linux execute only the model-level test (AC1, AC7 refusal path) —
  the variant is Windows-produced only; this mirrors every prior Phase 5
  slice.
- FAT-family volumes could not be probed (no FAT volume available); the
  NULL-DACL-reads-as-`None` behavior is specified from the security-model
  definition, and any *failed* read still degrades honestly, so an
  unexpected FAT behavior can only ever produce a degradation or a refusal,
  never silent loss or guessed state.
- SACL, owner/group, and directory DACLs are out of scope (§3) and have no
  tests because there is nothing to test — they are not captured.
- The ACE body of non-simple types is refused by design; no such ACE was
  probeable on this host (they do not occur on file DACLs in practice), so
  the refusal path is exercised by unit-level construction only if at all.
