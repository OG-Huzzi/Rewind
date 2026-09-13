# RewindUndo System Architecture Proposal

Status: Phase 0.7 normative architecture; Phase 1 implementation under
verification

The detailed final contract is in
.ai/PHASE_0_7_ARCHITECTURE_FINALIZATION_REPORT.md. This document is the
component-level view and must use the same terms.

## 1. Topology

~~~text
shell boundary markers --------\
                                -> observation/state engine
rewind run supervisor --------/
                                      |
                         typed manifest + CAS artifacts
                                      |
              external catalog/journal/anchor store
                                      |
                 rollback planner and filesystem executor
                                      |
                    same-filesystem staging/quarantine
~~~

Persistent metadata is external to the workspace. The local staging area is
filesystem-local transaction material, not authoritative state.

## 2. Workspace conditions

~~~text
HEALTHY
  trusted baseline and no unresolved capture or transaction gap
    |
    | failed/incomplete capture
    v
DEGRADED
    |
    | durable gap marker
    v
RECONCILIATION_REQUIRED
    |
    | complete scan and durable checkpoint
    v
HEALTHY
~~~

RECOVERY_REQUIRED is entered when a rollback step cannot be classified from
its complete physical state map. It blocks mutation independently of capture
health.

The old baseline pointer is retained for historical linkage after a capture
failure, but it is not a trusted pre-state until reconciliation succeeds.

## 3. State and baseline model

A state Sx is:

~~~text
Sx = canonical_manifest + schema_version + references to immutable CAS data
~~~

The manifest maps normalized workspace-relative paths to typed fingerprints.
State identity is the digest of a canonical serialization. Baseline advancement
is allowed only after a complete scan, artifact verification, durable manifest,
durable operation metadata, and final state verification.

Successful strong capture:

~~~text
trusted Sx
  -> full pre-state verification
  -> command
  -> full post-state verification
  -> trusted Sy
~~~

Failed capture:

~~~text
trusted Sx
  -> command may have changed disk
  -> CAPTURE_FAILED
  -> UNKNOWN_INTERVAL
  -> RECONCILIATION_REQUIRED
~~~

It is not valid to use Sx plus a later post-scan as a normal Sx -> Sz command
operation.

## 4. Observation modes

### Passive hooks

The pre-hook records a command boundary and the post-hook asks for a bounded
scan. A complete scan may produce a low-confidence observation. It does not
prove exact command causality when out-of-band edits could have occurred, so
the resulting record is not automatically eligible for undo. When the complete
post-state is durable, it may become the new trusted current observation
checkpoint for future comparisons; it is labeled as an observation rather than
an ordinary reversible operation.

On timeout, permission error, overflow, missing root, CAS failure, or incomplete
classification, the hook remains non-blocking for the shell but opens the
Rewind degradation gate. Later passive boundaries are untrusted markers only.

### Strong supervisor

rewind run acquires the writer lease. If the workspace is degraded, it first
performs full reconciliation and refuses to execute if reconciliation fails.
It then captures the current pre-state, supervises the command boundary, scans
the post-state, and records a normal transition only if all artifacts and
metadata are durable.

Background children, detached daemons, and out-of-workspace side effects remain
outside the strong guarantee unless the process-boundary policy explicitly
contains them.

## 5. Typed filesystem fingerprints

~~~text
ABSENT
REGULAR_FILE(content_hash, size, tracked_metadata)
DIRECTORY(manifest_hash, entry_count, tracked_metadata)
SYMLINK(target_bytes, target_hash, tracked_metadata)
UNSUPPORTED_OBJECT(kind, diagnostic_descriptor)
~~~

ABSENT is explicit in every expected source/destination map. A directory
fingerprint is a canonical sorted manifest of child names and child
fingerprints. It is not a hash of directory bytes. Symlinks are leaves; they
are not traversed. UNKNOWN describes an inspection failure and is not an
object kind.

Tracked metadata includes type, file content, directory topology, and literal
symlink target. Permissions, executable/read-only bits, ownership, timestamps,
hardlink topology, sparse layout, xattrs, ACLs, alternate data streams, and
reparse details have the partial/unsupported scope documented in the Phase 0.7
report.

## 6. Unknown intervals

An unknown interval records its trusted start state, detection reason, and
eventual reconciliation checkpoint. It contains no operations. List shows it,
diff reports its causal delta as unknown, and undo/redo cannot cross it.
Reconciliation creates a current state without inventing command attribution.
Future strong operations begin from that checkpoint.

## 7. Rollback architecture

Undo and redo create an anchor, obtain a writer lease, perform a complete
conflict check, and plan steps with source and destination states. Critical
movement uses a transaction-local area on the same filesystem as each affected
path:

~~~text
external journal intent
  -> same-filesystem prepared desired artifact
  -> same-filesystem local quarantine for old entry
  -> platform-specific namespace replacement
  -> flush and inspect every endpoint
  -> durable step
  -> catalog commit
  -> optional external archival copy
~~~

The external store may be on another volume. A cross-device archival copy is
copy/verify/flush and is not part of live rollback atomicity.

## 8. Journal state model

~~~text
PLANNED -> PREPARED -> APPLYING -> APPLIED -> DURABLE -> COMMITTED
                             \-> RECOVERY_REQUIRED
~~~

PLANNED has a durable complete plan and no intended live change. PREPARED has
verified staging and local-quarantine material. APPLYING means physical state
must be inspected. APPLIED means the complete after-map is observed. DURABLE
means the platform flush protocol and reinspection succeeded. COMMITTED means
all steps and catalog pointers are complete.

Recovery compares all source, destination, staging, and quarantine paths with
BEFORE and AFTER maps:

~~~text
BEFORE | AFTER | PARTIAL | CONFLICT | UNKNOWN
~~~

Only known BEFORE, known AFTER, or unambiguous PARTIAL states can continue.

## 9. Platform model

| Concern | Linux | macOS | Windows |
| --- | --- | --- | --- |
| File replacement | same-filesystem rename family | same-volume POSIX rename | ReplaceFileW/eligible namespace operation |
| Directory replacement | separate steps; no multi-entry atomicity | separate steps; no multi-entry atomicity | separate steps; ReplaceFileW is not a directory primitive |
| Cross-volume movement | EXDEV; not critical atomic movement | not critical atomic movement | copy/remove or failure; not critical atomic movement |
| Durability | file and accepted directory fsync requests | filesystem/API-specific fsync/F_FULLFSYNC | FlushFileBuffers and API-specific behavior |
| Confinement | descriptor-relative APIs, openat2 where present | component-wise descriptor checks | handles, sharing, reparse-point checks |
| CoW | FICLONE capability-dependent | APFS clonefile capability-dependent | ReFS/block-clone capability-dependent |
| Locks | mostly advisory | mostly advisory | sharing modes can prevent replacement |

No row means that the three platforms are interchangeable.

## 10. SQLite and durability

SQLite catalogs operation, state, and journal metadata. SQLite transaction
durability does not include live workspace directory entries. The external
filesystem journal is the recovery evidence, while SQLite is a catalog that
must be reconciled with physical inspection after interruption.

## 11. Security and concurrency

One Rewind writer owns reconciliation, capture commit, rollback, redo, restore,
and recovery. External editors do not hold that lease; every affected path is
rechecked immediately before mutation. Paths are normalized and confined.
Symlinks, junctions, reparse points, and case collisions are explicitly
handled or refused. A passive hook that cannot acquire the lease leaves a
pending bypass marker when the external boundary log is available; the next
writer boundary requires reconciliation before normal tracking resumes.

## 12. Architectural limits

The design does not recover bytes never captured, reverse external side
effects, preserve unsupported metadata exactly, provide multi-file atomicity,
or protect against arbitrary hardware/kernel/filesystem failure. It provides
strong behavior only under the supported platform/object and storage
assumptions in the Phase 1 contract.
