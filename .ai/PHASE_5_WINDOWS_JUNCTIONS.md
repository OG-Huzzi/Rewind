# Phase 5 Contract Amendment — Windows Junctions as First-Class Objects

Status: **PROPOSED-THEN-IMPLEMENTED** (this slice). This is an amendment to
the Phase 5 platform-expansion contract (`.ai/PHASE_5_PLATFORM_EXPANSION.md`,
slice 1: POSIX named pipes); the roadmap gate for further expansion is a
contract amendment with a capability-matrix update, which this document is.

Baseline: `main` at `7de39b4` (rollback-performance phase, CI run #44 green
on ubuntu/macOS/Windows; local suite 152 passed / 0 failed).

## 1. Why this slice

Slice 1's rationale was to "turn a real class of refused rollback into a
correct, deterministic one." The Windows equivalent of the FIFO case is the
**NTFS junction (mount-point reparse point)**: a common object in Windows
development workflows (`mklink /J` redirections for caches and dependency
folders), classified today as `UNSUPPORTED(WINDOWS_JUNCTION)` — and a single
unsupported object in a captured state makes the operation irreversible. On
Windows, an ordinary workspace containing one junction poisons undo exactly
the way a FIFO poisoned POSIX workspaces before slice 1.

The junction is the only remaining candidate that is all of: an object
class (not metadata), testable on real CI hardware (junction creation needs
no privilege, unlike symlinks' `SeCreateSymbolicLink`), restorable
faithfully without following it, and confined to one platform (POSIX never
produces the variant).

Deferred classes stay deferred for unchanged reasons (capability matrix,
`.ai/PHASE_5_PLATFORM_EXPANSION.md` §2): xattrs/ACLs/ADS (no faithful
cross-platform restore story yet), sockets/device nodes (runtime/machine
state), sparse layout/chflags/ownership (layout or privilege-dependent).

## 2. Measured facts this contract rests on (probe, 2026-09-27, Windows 11/NTFS)

Every mechanical claim below was verified by direct probe against a real
`mklink /J` junction, not inferred from documentation:

- The junction reparse buffer layout is: 8-byte header (tag `0xA0000003`,
  `ReparseDataLength`, `Reserved`), then four `u16` fields
  (substitute-name offset/length, print-name offset/length, relative to
  `PathBuffer`), then `PathBuffer` = substitute UTF-16 + NUL + print UTF-16
  + NUL. `ReparseDataLength = 8 + substitute_len + 2 + print_len + 2`;
  lengths exclude NULs; `PrintNameOffset` follows the substitute NUL.
- `fs::symlink_metadata` reports a junction as `is_symlink() == true`,
  `is_dir() == false`, `is_file() == false`; std classifies both SYMLINK and
  MOUNT_POINT tags as symlinks (name surrogates), so the reparse-tag read —
  never the file type — distinguishes them.
- `fs::read_link` on a junction returns the **print name**.
- A junction whose target was deleted still stats (`symlink_metadata`)
  fine; `fs::metadata` (follow) fails with os error 2; `Path::exists()`
  returns false. Following APIs never see data, but no-follow APIs always do.
- `FSCTL_SET_REPARSE_POINT` is `0x000900A4`
  (CTL_CODE(0x0009, 0x029, METHOD_BUFFERED, FILE_ANY_ACCESS)). With a handle
  opened `GENERIC_WRITE`, `OPEN_EXISTING`,
  `FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS`, on a plain
  directory, setting a buffer reconstructed from the parsed names of a real
  junction produces a junction whose reparse buffer is **byte-identical** to
  the original. No privilege is required (junctions, unlike symlinks).
- Setting the readonly attribute with `fs::set_permissions(readonly = true)`
  on the plain directory **before** the reparse-point set yields a junction
  whose `symlink_metadata` reports readonly, and the reparse set still
  succeeds. Conversely, `attrib +R` on an existing junction **follows** it
  and marks the target — no built-in user tooling can mark the junction
  entry itself, so a readonly junction entry only arises programmatically,
  in the order this implementation uses.
- `fs::rename` of a junction (the quarantine move) moves the reparse point
  itself; the target subtree is untouched, and the reparse data arrives
  intact. The original path is gone.
- A junction to a **nonexistent** target is created without error: dangling
  targets restore exactly like live ones; nothing is followed.

## 3. Supported and refused behavior

### 3.1 Supported (Windows only)

- Scan: a junction is recorded as the new `Fingerprint::Junction` carrying
  the **substitute name** (the NT path that governs resolution), the
  **print name**, a BLAKE3 hash over the canonical pair, and the entry's
  own `MetadataFingerprint` (readonly). It is a literal leaf: the scanner
  never descends into it, never resolves its target, and never opens it
  except with `FILE_FLAG_OPEN_REPARSE_POINT` to read the reparse data.
- Capture/undo/redo: an operation that creates, deletes, or replaces a
  junction is fully reversible. Restoration recreates a plain directory,
  applies the recorded readonly attribute while it is still a plain
  directory, then sets the recorded reparse data
  (`FSCTL_SET_REPARSE_POINT`), then reads the reparse data back and
  requires the recorded substitute and print names. The final full-scan
  state comparison verifies the complete fingerprint (names + metadata).
- Escaping and dangling junction targets are recorded and restored
  **literally** — identical to POSIX symlinks whose targets point outside
  the workspace: the target is never followed, never checked for existence,
  and never created. Restoring a junction pointing at an external path does
  not touch the external path.
- Quarantine (undo of a replacement) renames the junction itself, exactly as
  for every other object class; the post-mutation confinement machinery is
  unchanged.

### 3.2 Refused / unchanged

- A junction inside a *parent chain* of any mutation is still refused
  (`ensure_parent_confinement` sees a reparse point): no rollback step ever
  resolves a path through a junction.
- Archiving a quarantined junction to the external archive is refused with a
  named reason ("junction quarantine objects are not archived; the journal's
  recorded fingerprint is the recovery record") — the same best-effort
  archival posture as FIFOs (`ArchiveStatus::Failed`, local quarantine
  retained). A junction has no content; its identity is fully held by the
  journal's recorded fingerprint, so nothing can be lost by refusing the
  copy. The pre-existing behavior this replaces would have mislabeled the
  junction as an "unreadable symlink" — the refusal was already correct, the
  reason was wrong; both are fixed here.
- Restoring a `Junction` fingerprint on POSIX/Linux/macOS is refused with an
  explicit unsupported error (the scanner never produces the variant there;
  a foreign manifest containing one is refused at materialization, exactly
  as for `NamedPipe` on Windows).
- A junction whose reparse data cannot be read or parsed (unreadable buffer,
  malformed offsets, non-Unicode names) is recorded as
  `UNSUPPORTED(WINDOWS_JUNCTION)` with a named reason — never guessed, never
  partially restored.
- Unrecognized or unreadable reparse tags remain `UNSUPPORTED`, now with the
  actual tag value recorded in the descriptor when readable (the capability
  matrix already promised "tag recorded when readable"; the implementation
  now does it).

## 4. Correctness invariants (all preserved)

1. **Never follow.** No code path resolves a junction target. The scanner
   classifies from reparse data alone; the restorer applies the recorded
   bytes; verification compares recorded bytes to read-back bytes. `fs::metadata`
   (follow) is never called on a junction; `fs::set_permissions` is only
   called while the path is a plain, freshly created directory inside the
   workspace (it would follow a junction).
2. **Literal-leaf identity.** A junction's state is exactly its reparse data
   plus its own entry attributes; equality of fingerprints is equality of
   that state, bit for bit (the recorded pair hashes are derived from the
   same bytes that restoration writes).
3. **No unsafe restoration.** Creation is `create_dir` → set readonly (plain
   dir) → set reparse data → read back and compare. Any divergence between
   the read-back and the record fails the step into `RecoveryRequired`;
   nothing is silently accepted.
4. **Journal, quarantine, confinement untouched.** Junction steps use the
   same step machinery: expected-before conflict gate, quarantine-by-rename,
   post-mutation parent-confinement verification, per-step durability
   (`synchronous = FULL`), and the final full-scan state comparison that
   anchors global correctness (ADR-018 unchanged).
5. **Nothing assumed unchanged.** Each step re-observes its path live; there
   is no caching of reparse data across steps, no watcher input, and no
   trust of stale evidence.
6. **Serde compatibility.** `Fingerprint::Junction` is additive; every
   existing manifest deserializes unchanged, and an old binary reading a
   junction manifest fails with an explicit unknown-variant error (honest
   refusal). No `STATE_SCHEMA_VERSION` bump: nothing previously readable
   becomes unreadable. Workspaces whose junctions were recorded `UNSUPPORTED`
   upgrade to `Junction` on the next scan; the resulting state-id drift is
   resolved by an explicit reconciliation checkpoint, never silently — the
   same upgrade path slice 1 established for FIFOs.
7. **Watcher advisory.** Watcher events for junction paths are hints; the
   watcher still never gates anything and never writes in the workspace.

## 5. Capability matrix update

| Object | Linux | macOS | Windows | Notes |
|---|---|---|---|---|
| Windows junction | N/A | N/A | **supported** | **new (this slice)**: literal leaf; reparse data (substitute + print names) recorded and restored byte-faithfully; readonly attribute restored; target never followed |
| Other reparse point | N/A | N/A | unavailable (restore refused) | tag value recorded in the descriptor when readable |

All other rows are unchanged. POSIX never produces `Junction`; Windows never
produces `NamedPipe`.

## 6. Acceptance criteria

- **AC1 (Windows):** a junction scans as `WINDOWS_JUNCTION` with the
  recorded substitute and print names; the manifest has no entries beneath
  it (never traversed); `is_supported_for_restore() == true`.
- **AC2 (Windows):** an operation that creates a junction is captured and
  reversible: undo removes it; redo recreates it with byte-identical
  reparse data and the recorded readonly attribute.
- **AC3 (Windows):** replacing a junction with a regular directory (with
  content) is reversible in both directions; the quarantine rename moves the
  junction, never its target subtree.
- **AC4 (Windows):** a junction whose target lies outside the workspace
  root — including a target that does not exist — is recorded and restored
  literally; the external path is never followed, checked, or created.
- **AC5 (Windows):** external modification of a junction's reparse data
  after capture still refuses undo with `Conflict` before any mutation
  (unchanged conflict semantics).
- **AC6 (all platforms):** `Fingerprint::Junction` reports its kind name and
  restore support, round-trips through serde, and old manifests without it
  deserialize unchanged.
- **AC7 (all platforms):** the full existing suite passes unmodified except
  `junction_is_unsupported_object` (rollback_tree.rs), whose premise this
  amendment deliberately invalidates; it is replaced by the junction
  lifecycle tests above and by the POSIX socket tests, which remain the
  cross-platform witness that unsupported objects still refuse undo.
- **AC8 (Windows):** the path-scoped fingerprint scan
  (`Workspace::scan_path`) produces fingerprints identical to the full scan
  for junctions, including dangling, nested, and readonly junctions (ADR-018
  equivalence extended to the new object class).
- **AC9:** recovery: an interrupted junction-undo classifies and completes
  through the existing journal machinery (backup verification reads reparse
  data, never follows), and `recover --reconcile` remains the only exit from
  an unclassifiable transaction.

## 7. Known testing gaps (disclosed, not claimed)

- An unrecognized-tag reparse point cannot be manufactured with built-in
  Windows tooling, so the "other reparse point → UNSUPPORTED with tag in
  descriptor" branch is not end-to-end testable on CI; it is exercised only
  by the unreadable/None branch of the same classifier. The refusal path
  itself (any unsupported object refuses undo) remains witnessed by the
  POSIX unix-socket test.
- A junction whose own entry carries FILE_ATTRIBUTE_READONLY cannot be
  produced with built-in tooling (`attrib +R` follows the junction and
  marks the target), so the readonly-restore branch of junction creation is
  implemented and probe-verified at the mechanism level but not exercised
  end-to-end. The scanner records the attribute honestly, the default
  (readonly = false) path — including fingerprint equality of the recorded
  attributes — is covered end-to-end by every junction lifecycle test, and
  the final full-scan comparison would catch any attribute divergence.
- Windows symlink restore does not re-apply the readonly attribute today
  (pre-existing, unchanged by this slice; a readonly symlink would fail
  post-restore verification conservatively). Junctions restore readonly
  faithfully; the symlink gap is disclosed as pre-existing and out of scope.
- The junction-creation FFI (`CreateFileW`/`DeviceIoControl`
  `FSCTL_SET_REPARSE_POINT`) is the crate's fourth documented minimal FFI
  site, alongside `reparse_tag` (scan.rs), `CreateProcessW`
  (watch/detach_windows.rs), and `mkfifo(2)` (rollback.rs).
