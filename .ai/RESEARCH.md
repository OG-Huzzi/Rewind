# Systems Research Notes

Status: Phase 0.7 synchronized research summary

## 1. Observation research

Shell hooks provide useful boundary markers but not kernel-level attribution.
inotify, FSEvents, and ReadDirectoryChangesW can report path activity while
coalescing, overflowing, or missing process identity. They are therefore
future advisory inputs, not a replacement for full reconciliation.

The selected Phase 1 model is:

~~~text
passive boundary marker -> bounded post-observation
explicit rewind run -> complete pre-state, supervised boundary, complete post-state
capture failure -> degradation gate
reconciliation -> trusted current checkpoint without causal invention
~~~

## 2. Copy and CAS research

CoW primitives can reduce copy cost but are capability-dependent:

- Linux FICLONE/reflink depends on kernel and filesystem;
- macOS clonefile is primarily an APFS capability;
- Windows block cloning is filesystem/API-dependent, especially ReFS.

Independent byte copies are the correctness fallback. Hardlinks are excluded
from immutable CAS because an in-place workspace write can mutate the shared
inode. CAS objects require independent verification and publication.

## 3. Filesystem fingerprint research

Regular files have bytes and a content hash. Directories have entries and
metadata, not directory bytes. Symlinks have literal target strings and must
not be dereferenced. Missing paths require ABSENT. FIFOs, sockets, devices,
junctions, and unclassified reparse objects need explicit unsupported policy.

The canonical directory fingerprint is a sorted child-manifest hash. A
fingerprint includes an object type tag so a file, directory, symlink, absent
path, and unsupported object cannot compare equal accidentally.

## 4. Filesystem atomicity research

POSIX rename-family calls can provide single-entry same-filesystem namespace
replacement. They do not make a multi-entry rollback atomic and cross-device
movement fails or becomes a copy/remove operation. Linux renameat2 is not a
portable macOS API.

Windows ReplaceFileW is a regular-file replacement API with sharing and
metadata behavior that differs from POSIX rename. Directory replacement,
reparse points, sharing modes, and durability require separate paths.

## 5. Corrected quarantine research

The workspace and the user-profile store may be on different volumes. A live
entry therefore moves first to a same-filesystem transaction-local quarantine.
The old entry can later be copied and verified to the external archival store.
If archival fails after the live transaction is committed, the result is
ARCHIVE_PENDING; it is not treated as a failed live rollback.

## 6. Durability research

SQLite WAL durability covers SQLite pages, not external directory entries.
File flush and directory-entry flush requests are separate and have
filesystem/device-specific semantics. The journal must persist intent before
mutation, prepare artifacts, perform the platform operation, flush what the
platform supports, re-inspect all endpoints, and only then record DURABLE.

No userspace protocol removes the possibility of defective hardware, kernel
failure, filesystem bugs, or external corruption. The architecture states its
assumptions.

## 7. Research conclusion

The feasible product is a conservative state recorder and recovery engine with
strong semantics for supported local objects and explicit uncertainty for
everything else. It is not a universal observer or transaction manager.
