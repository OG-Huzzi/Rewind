# Phase 5 Contract Amendment — Slice 3: Windows Alternate Data Streams

Status: **IMPLEMENTED** (this slice, Phase 5.3). The mechanics below were
**probe-verified against a real NTFS volume before this contract was
written**; the probe example was a scratch tool and is not committed.
Local gates (fmt, check, clippy `-D warnings`, full suite 162/0 on
Windows) are green; **CI run #49 on `1721503` is green on ubuntu, macOS,
and Windows** (the first push's POSIX clippy `unused_mut` was fixed by
`1721503`; see `.ai/TEST_STATUS.md`).
One amendment during implementation: stream restoration copies from the
verified CAS blob in bounded chunks instead of `read_bytes` (no stream
blob is ever loaded whole into memory — §5 step 4 reflects this).

Baseline: `main` at `7d3faa0` (Phase 5 slice 2 CI-verified, run #47 green on
ubuntu/macOS/Windows).

## 1. Selection (exactly one capability)

Deferred metadata classes in the slice-1 matrix: extended attributes, ACLs,
alternate data streams, ownership, sparse files, chflags. Evaluation:

| Candidate | Faithful capture+restore | CI testability | Verifiability in the working environment | Verdict |
|---|---|---|---|---|
| ACLs | Divergent models (POSIX ACLs / NFSv4 / SDDL); cross-platform fidelity not honestly claimable | partial | no | deferred |
| Ownership (uid/gid) | Privilege assumptions CI cannot verify (the matrix's own recorded reason) | poor | no | deferred |
| Sparse layout, chflags | Presentation/kernel layout; chflags needs privileges (macOS) | poor | no | deferred |
| POSIX xattrs | Yes in the `user.*` namespace, but namespace semantics differ per filesystem (user.* restricted to regular files and directories on Linux; macOS system namespaces privileged) — no local POSIX host to probe against; the first behavioral signal would arrive from CI | ubuntu+macos | **no — blind implementation**; the FIFO slice's FIFO-open CI hang is the recorded example of exactly this failure mode | deferred, with this blocker recorded |
| **Windows alternate data streams (ADS)** | Byte-exact capture, restore, and re-verification of named `$DATA` streams | windows-latest (NTFS) | **fully probeable on the working Windows/NTFS machine** — the junction-slice methodology | **selected** |

ADS is also the Windows analogue of the slice-1/2 rationale: a stream-carrying
file was recorded as a plain regular file whose hidden named content was
invisible to state identity, so undo could silently drop streams when the
file was restored from the default stream alone.

## 2. Probe-verified mechanics (the facts this contract is built on)

- Enumeration via `FindFirstStreamW` returns entries of the form
  `":$DATA"` (the default stream — the file's own content) and
  `":name:$DATA"` for named streams, with the stream size in bytes.
- Named streams are created/read through the ordinary path syntax
  `path:name` and `path:name:$DATA` with plain std APIs; empty streams
  (0 bytes) are valid; names may contain dots and spaces; deletion is
  `remove_file` on the spec. NTFS stream names are case-insensitive;
  enumeration is the single source of recorded truth.
- **`fs::rename` moves the file with every stream** (quarantine is safe).
- **`fs::copy` carries all streams** (std uses `CopyFileW` on Windows), so
  quarantine archival can stay faithful instead of refusing.
- **Writing a stream spec on a junction path follows the reparse point into
  the target directory** (probe: `junc:jstream` landed inside the target as
  `dir:jstream`; the junction itself gained nothing). Stream writes therefore
  happen only on leaves already verified as real regular files.
- **Directories can carry named streams on NTFS** (enumerable and writable).
  This slice does not make them state; see the exclusions below.
- Stream writes via std are direct; the CAS `materialize` temp+rename
  protocol cannot be used for stream specs (the temp path itself would parse
  as a stream spec).

## 3. Scope

**In scope:** named `$DATA` streams (alternate data streams) attached to
**regular files** on Windows, captured, compared, restored, and verified as
part of the file's fingerprint.

**Supported:** Windows on NTFS. On non-NTFS Windows filesystems named
streams cannot exist (FAT-family volumes have no named streams), so the
scanner simply records an empty stream set — no refusal is needed; there is
nothing to be unfaithful about.

**Explicitly out of scope (documented, not silent):**

- Streams attached to directories: they exist on NTFS, are enumerable, and
  are **not part of recorded state** — the same documented non-state class as
  timestamps and ownership. The contract says so rather than hiding it.
- Streams on symlinks and junctions: never enumerated; these objects are
  classified before the regular-file branch and are never opened for stream
  purposes. Reparse-point leaves never gain streams through Rewind.
- Non-`$DATA` stream types (any enumerated entry not of the form
  `:name:$DATA` other than the default `::$DATA`): a scan error naming the
  entry — honest refusal, never a guessed parse.
- POSIX: the scanner can never produce a stream set (enumeration is
  Windows-only); a foreign manifest containing streams is refused at
  materialization with an explicit unsupported error rather than guessed at —
  mirroring the FIFO refusal on Windows and the junction refusal on POSIX.

## 4. Capture semantics

- `Fingerprint::RegularFile` gains `streams: BTreeMap<String, String>`
  (stream name → CAS content hash), `#[serde(default, skip_serializing_if =
  "BTreeMap::is_empty")]`.
- The stream **name** is the enumerated form with the leading `:` and the
  trailing `:$DATA` removed (e.g. `:Zone.Identifier:$DATA` →
  `Zone.Identifier`); recorded verbatim, never normalized.
- The stream **content** is ingested into the CAS exactly like file content
  (hash-and-store under ingest; hash-only under observe). The empty stream is
  a value like any other.
- The default stream remains the file's existing `content_hash`/`size`;
  nothing about the current file fingerprint changes.
- **No drift, no schema bump:** a file without streams serializes
  byte-identically to the pre-slice form (the field is skipped), so existing
  workspaces' state ids are unchanged. A file *with* streams previously
  compared equal to itself without them; the first scan after upgrade makes
  state identity strictly stronger, resolved by the existing reconciliation
  checkpoint path — the same upgrade argument as the FIFO and junction slices.
- Enumeration failure (permission, I/O): `ScanIncomplete` — the workspace
  degrades honestly; no guessed stream set, no silent omission.
- POSIX and stream-free files: an empty map, omitted from serialization.

## 5. Restoration semantics

Ordering inside the existing per-step machinery:

1. Quarantine rename of the replaced object (streams travel with the file).
2. Install the staged default-stream content, apply metadata.
3. **`verify_mutation_confined(leaf_exists = true)`** — the leaf must be a
   real regular file, not a reparse point. This check precedes any stream
   write because a stream write on a junction path follows the junction out
   of the workspace (probe-verified).
4. Write each named stream directly from the CAS — content verified
   (`Cas::verify`) before the stream file is created, then copied from the
   blob in bounded chunks (a stream blob is never loaded whole into
   memory), flushed, and no traversal beyond the verified leaf.
5. The step's post-apply path-scoped re-scan compares the **complete
   fingerprint** — default-stream hash, size, metadata, and every stream
   name/hash — so a partial or wrong stream write fails the step into
   `RecoveryRequired` through the existing machinery.

Crash between the `Applying` journal write and step completion (including
mid-stream-write): recovery classifies exactly as for a half-installed
regular file today — the step is not silently completed; the journal lands
`RecoveryRequired` and `recover --reconcile` remains the documented exit,
with the quarantined original (streams intact) as the recovery record.
Idempotent re-application after an interrupted-but-classifiable step behaves
like every other object type.

## 6. Safety and uncertainty

- **No-follow:** streams are enumerated only for objects already classified
  as regular files by `symlink_metadata` (reparse points report
  `is_file() == false`); streams are written only after the leaf is verified
  as a regular file under real parent components. The residual TOCTOU window
  is the documented Phase 0.7 conditional guarantee, unchanged.
- **No silent loss:** stream content is in the CAS before any mutation is
  journaled; quarantine carries streams; the archive copies and verifies
  streams (below); restoration that cannot reach the recorded stream set
  fails the step rather than reporting success.
- **No guessing:** unparseable enumeration entries, unreadable streams, and
  foreign `streams` on POSIX are explicit errors, never approximations.
- Journal durability, writer serialization, reconciliation authority, and
  watcher advisory semantics are untouched.

## 7. Archive behavior

`fs::copy` carries streams, so `copy_artifact` needs no change;
`verify_archive_pair` additionally compares, on Windows, the stream name
sets, sizes, and BLAKE3 content hashes of quarantined source and archived
copy — `ArchiveStatus::Archived` is still only set after full verification,
so a shallow copy can never authorize disposal of stream content.
`artifact_matches_fingerprint` (quarantine-backup verification and recovery
classification) also compares stream name/hash sets on Windows.

## 8. Testable acceptance criteria

- **AC1 (model, all platforms):** a legacy `RegularFile` without the field
  deserializes; a stream-free file serializes byte-identically to the
  pre-slice form (no drift); a stream-carrying fingerprint round-trips.
- **AC2 (Windows):** a file with named streams scans with every stream name
  and hash recorded; content lands in the CAS; a stream-free file scans with
  an omitted stream set.
- **AC3 (Windows):** an operation that adds/removes/modifies a stream is
  captured; undo restores the exact recorded stream set byte-for-byte; redo
  restores the new set; the empty stream and multi-stream cases are covered.
- **AC4 (Windows):** an external stream modification after capture refuses
  undo before any mutation (conflict), exactly as for main content.
- **AC5 (Windows):** quarantine moves the file with streams; the archive
  copy carries them and archive verification compares them.
- **AC6 (Windows):** the path-scoped fingerprint equals the full-scan
  fingerprint for stream-carrying files (rollback-perf invariant preserved).
- **AC7 (POSIX/all):** the full existing suite stays green; streams never
  appear off-Windows; restore of a foreign stream-carrying fingerprint is
  refused with a named error.

## 9. Known testing gaps (disclosed, not claimed)

- macOS/Linux execute only the model-level test (AC1, AC7 refusal path) —
  the variant is Windows-produced only; this mirrors the FIFO slice's
  Windows gap.
- Streams on symlinks are not probeable locally (symlink creation needs a
  privilege this environment lacks); the scanner never enumerates reparse
  leaves, so the path is unreachable by construction.
- Directory-attached streams are writable by users but deliberately not
  state; no test asserts their non-capture beyond the contract text.
