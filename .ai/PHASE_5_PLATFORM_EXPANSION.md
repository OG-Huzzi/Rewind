# Phase 5 Contract — Platform Expansion

Status: **IMPLEMENTED** (commit on `main`; see CURRENT_STATE.md / TEST_STATUS.md
for the verified record). The sections below are the contract the
implementation follows.

Baseline: `main` at `1f37ec3` (Phase 4 slice, CI run 36228897539 green on
ubuntu/macOS/Windows; local suite 148 passed / 0 failed).

## 1. What "platform expansion" means here

The roadmap: *"Expand object and metadata support only with
platform-specific tests and an updated capability matrix. A new platform is
not considered supported merely because a common API name exists."*

An audit of the scanner (2026-09) found the expansion surface is **object
types**, not operating systems: Linux, macOS, and Windows are all
implemented end-to-end (scan, capture, watcher, rollback, recovery, CLI,
CI), and POSIX permission metadata is already recorded and authoritatively
restored. The gap is that every filesystem object which is not a regular
file, directory, or symlink is recorded as `UNSUPPORTED` — and a single
unsupported object in a captured pre/post state makes the operation
irreversible (undo refuses; see the junction test). On POSIX this silently
poisons rollback for ordinary workspaces that contain a **named pipe
(FIFO)** — a common object in build, service, and shell workflows.

## 2. The selected slice: POSIX named pipes (FIFOs) as first-class objects

**Why this slice:** it is the smallest capability that turns a real class of
refused rollback into a correct, deterministic one; it fits the existing
typed-fingerprint architecture exactly (the same way symlinks gained
`target_kind`); it needs no new dependency (creation uses
`std::os::unix::fs::mkfifo`); and it can be genuinely tested on two of the
three CI platforms (ubuntu, macOS) — not merely compiled.

**What becomes available (user-facing):**

- A workspace containing a FIFO scans as `NAMED_PIPE` instead of
  `UNSUPPORTED`; `rewind list/show/diff` and the timeline report a supported
  object instead of a refusal.
- Strong capture of an operation that creates, deletes, or replaces a FIFO
  records it as part of the state.
- Undo/redo restore the FIFO (recreated via `mkfifo`) with the recorded
  permission mode, under the same journal, quarantine, confinement, and
  verification machinery as every other object type.
- A FIFO's *content in flight* is not and cannot be captured: a FIFO is a
  kernel synchronization object, not a data container. Only its existence
  and metadata are state. This is stated in output and documentation.

### Explicitly NOT expanded (deferred or permanent)

| Object / capability | Platforms | Decision |
|---|---|---|
| Unix domain sockets, character/block devices | POSIX | Remain `UNSUPPORTED`. Sockets are runtime state (nothing meaningful to restore); device nodes require privileges and are machine state, not workspace state. Scan descriptors made precise (`UNIX_SOCKET`, `CHARACTER_DEVICE`, `BLOCK_DEVICE`) so refusals name what was found. |
| Named pipes as workspace objects | Windows | N/A — NTFS has no FIFO filesystem objects; the scanner never produces the new fingerprint. A manifest *containing* `NamedPipe` (foreign store) is refused at materialization with an explicit unsupported error rather than guessed at. |
| Windows junctions / other reparse points | Windows | Remain `UNSUPPORTED` (existing safety decision, unchanged). |
| Extended attributes, ACLs, alternate data streams | all | Deferred. Recording them without restoring them faithfully would claim metadata support that does not exist. |
| Sparse files, file flags (chflags), ownership (uid/gid) | POSIX | Deferred; ownership restoration in particular would require privilege assumptions CI cannot verify. |

## 3. Capability matrix (object types × platforms)

Per Phase 0.7 language: **supported** = captured, restored, and verified;
**unavailable** = refused with a named reason; N/A = the object cannot exist
on that platform.

| Object | Linux | macOS | Windows | Notes |
|---|---|---|---|---|
| Regular file | supported | supported | supported | content in CAS, mode restored on POSIX |
| Directory | supported | supported | supported | |
| Symlink (inside-root target) | supported | supported | supported | Windows flavor from reparse data |
| Symlink (escaping / unknown target) | recorded, restore refused | recorded, restore refused | recorded, restore refused | never followed |
| Windows junction | N/A | N/A | unavailable (restore refused) | `WINDOWS_JUNCTION` |
| Other reparse point | N/A | N/A | unavailable (restore refused) | tag recorded when readable |
| **Named pipe (FIFO)** | **supported** | **supported** | unavailable (restore refused) | **new**; existence + mode only, no content |
| Unix domain socket | unavailable (restore refused) | unavailable (restore refused) | N/A | `UNIX_SOCKET` |
| Character / block device | unavailable (restore refused) | unavailable (restore refused) | N/A | descriptor names the device class |
| Non-Unicode filename | unavailable (scan refusal) | unavailable (scan refusal) | unavailable (scan refusal) | never guessed |

Platform guarantees are otherwise unchanged: local-first deterministic
core, reconciliation authoritative, watcher advisory, single writer,
conditional safety language for everything outside the workspace boundary.

## 4. Compatibility and uncertainty behavior

- **Persisted data:** the new `Fingerprint::NamedPipe` variant is additive to
  the serde-tagged enum; every existing manifest deserializes unchanged
  (old manifests never contain the variant). The reverse — an old binary
  reading a new manifest containing a FIFO — fails with an explicit
  unknown-variant error, which is honest refusal, not corruption. No schema
  version bump is required because no previously readable input becomes
  unreadable.
- **Existing workspaces:** workspaces created before this change scan
  identically except that FIFO entries upgrade from `UNSUPPORTED` to
  `NAMED_PIPE`. This can make a previously-irreversible operation reversible
  after re-reconciliation — a strictly stronger capability obtained by
  re-scan, never by trusting stale evidence.
- **Uncertainty:** unchanged. A FIFO's in-flight content is unknowable and
  is not claimed; watcher observations remain advisory; conflicts and
  refusals keep their existing semantics.

## 5. Safety invariants preserved

- FIFO materialization goes through the same journal step machinery:
  quarantine-by-rename of the replaced object, post-mutation confinement
  verification, parent confinement, per-step durability, and post-apply
  re-scan verification (`compatible_after`).
- `mkfifo` creates the pipe with a private 0o600 mode before the recorded
  authoritative mode is applied — no window where a world-accessible pipe
  exists that the manifest did not specify.
- The scanner must never open, read, or block on a FIFO: the FIFO branch
  classifies from `file_type` only, exactly like directories and symlinks.
  (The pre-existing regular-file path reads only regular files.)
- The same rule governs durability: `sync_target` never opens a FIFO —
  `open(2)` on a FIFO blocks until a writer appears (caught as a real
  multi-hour POSIX CI hang, see TEST_STATUS). A FIFO has no byte content to
  fsync; the durability of its directory entry is the parent-directory fsync
  that sync_target already performs.
- Archival of a quarantined FIFO is best-effort: `copy_artifact` refuses
  non-file objects, so an archive containing a FIFO records
  `ArchiveStatus::Failed` with a named reason; the local quarantine remains
  the recovery record, exactly as the storage boundary specifies.
- Windows behavior for every pre-existing object type is byte-for-byte
  unchanged; Windows never produces `NamedPipe`.

## 6. Testable acceptance criteria

- **AC1 (POSIX):** `scan` classifies a FIFO as `NAMED_PIPE` with its mode
  recorded, and does not open or block on it.
- **AC2 (POSIX):** an operation whose post state contains a newly created
  FIFO is captured with `NamedPipe` in the manifest; undo removes it; redo
  recreates it with the recorded mode.
- **AC3 (POSIX):** an operation that replaces a FIFO with a regular file is
  undoable — the undo recreates the FIFO with its recorded mode — where the
  same operation was refused before this change (the union-junction refusal
  test remains the witness for unsupported objects).
- **AC4 (POSIX):** a Unix domain socket remains `UNSUPPORTED` with a
  `UNIX_SOCKET` descriptor and its presence still refuses undo.
- **AC5 (all platforms):** `Fingerprint::NamedPipe` reports
  `is_supported_for_restore() == true` and kind name `NAMED_PIPE`; old
  manifests (without the variant) deserialize unchanged.
- **AC6 (all platforms):** full existing suite stays green; the Windows CI
  job exercises unchanged behavior for all pre-existing object types.
- **AC7 (POSIX):** FIFO restore is journaled and verified: the post-apply
  re-scan comparison (`compatible_after`, exact fingerprint equality
  including mode) gates success — the shared verification path that marks a
  failed step `RecoveryRequired`.
- **AC8:** the capability matrix above ships in this document and the
  scanner's refusal descriptors name the actual object class found.

## 7. Known testing gaps (disclosed, not claimed)

- The Windows refusal path for a foreign `NamedPipe` manifest is code-level
  only (the scanner can never produce the variant on Windows, and
  constructing a synthetic journal for it would test scaffolding, not
  product behavior). Disclosed as untested-on-Windows; the Linux/macOS CI
  jobs execute the real FIFO lifecycle.
- Device-node classification (`CHARACTER_DEVICE`/`BLOCK_DEVICE` descriptors)
  is implemented by file_type but not CI-tested: creating device nodes in a
  container/runner requires privileges CI does not guarantee. The socket
  test covers the same code path shape (non-FIFO, non-regular special file).
