# PHASE 0.7 - STATE DEGRADATION & FILESYSTEM TRANSACTION FINALIZATION REPORT

Project: RewindUndo

Status: DOCUMENTATION AND ARCHITECTURE ONLY

Phase 1 status: NOT STARTED

Date: September 2026

This document is the Phase 0.7 normative authority. It supersedes any earlier
statement in the project documentation that treats an unadvanced baseline as
trusted after a failed capture, treats external trash movement as a critical
same-filesystem rename, uses only a content hash to identify a filesystem
state, or treats POSIX and Windows filesystem operations as equivalent.

## 1. Executive summary

The Phase 0.6 design correctly separated SQLite metadata durability from
filesystem mutation, but it still allowed a dangerous interpretation: after a
failed passive post-scan, the baseline pointer could remain at Sn and later
observations could be interpreted as a normal Sn -> Sn+1 operation. The pointer
is not enough to establish trust. A failed capture means that the real
workspace may have diverged from Sn and the exact command boundary is unknown.

Phase 0.7 makes that loss of trust explicit. A failed or incomplete capture
creates a CAPTURE_FAILED record and UNKNOWN_INTERVAL, enters DEGRADED, and
persists a RECONCILIATION_REQUIRED gate. Passive commands during the gate are
not normal operations. Reconciliation creates a trusted current checkpoint but
does not fabricate an operation for the interval.

The filesystem model is also made explicit. Every expected path in a capture
or rollback plan has a typed fingerprint: ABSENT, REGULAR_FILE, DIRECTORY,
SYMLINK, or UNSUPPORTED_OBJECT. Directory identity is a canonical child
manifest, not a hash of directory bytes. Recovery examines source, destination,
staging, and quarantine paths together.

Critical rollback movement is now transaction-local and same-filesystem. The
external Rewind store remains the location for persistent journals, catalog
metadata, CAS artifacts, and optional post-commit archival copies. It is not
used as the atomic destination for a live workspace move. Platform sections
state the different semantics of Linux, macOS, and Windows.

The hostile review at the end of this report finds no remaining design
ambiguity that requires guessing. It does find explicit refusal cases:
unknown physical recovery states, unsupported metadata, workspace identity
loss after a move, and failed reconciliation block normal mutation.

## 2. The six verifier blockers

| Blocker | Root cause in the previous design | Final correction |
| --- | --- | --- |
| A. Stale baseline after failed passive capture | “Baseline did not advance” was treated as if it remained a valid current pre-state. | A failed capture marks the baseline historical, creates CAPTURE_FAILED and UNKNOWN_INTERVAL, and enters DEGRADED/RECONCILIATION_REQUIRED. No normal attribution follows. |
| B. Incomplete filesystem hashing | Recovery treated a missing path as a null hash and treated directories as if they had regular-file bytes. | Typed fingerprints include ABSENT, regular-file content, deterministic directory manifests, symlink targets, and explicit unsupported objects. |
| C. Cross-filesystem external quarantine | The external trash path under the user profile was used as if rename were universally atomic. | Critical movement uses a private transaction-local quarantine on the affected filesystem. External trash is post-commit archival only. |
| D. Source/destination recovery verification | Recovery often inspected only the destination path and inferred the rest. | Every step journal records and verifies complete before/after maps for source, destination, staging, and local-quarantine paths. |
| E. Windows/POSIX equivalence | Rename, replacement, directory, locking, and flush semantics were presented as one abstraction. | Dedicated platform models and capability matrix expose supported, partial, unavailable, and best-effort behavior. |
| F. Absolute crash guarantees | Conditional design goals were written as universal promises. | Guarantees now state required assumptions and explicitly exclude hardware, kernel, filesystem, environmental, and unsupported-object failures. |

## 3. Architectural changes

The following are design changes, not implementation tasks:

1. Add the workspace condition model HEALTHY, DEGRADED, and
   RECONCILIATION_REQUIRED. Keep RECOVERY_REQUIRED as a separate transaction
   lockout.
2. Add CAPTURE_FAILED, CAPTURE_GAP, UNKNOWN_INTERVAL, and RECONCILIATION
   checkpoint records. A failed capture has no resulting operation state.
3. Separate a trusted baseline pointer from the historical last-known state.
4. Make reconciliation a full scan and CAS-ingestion event. It establishes
   trust but never assigns causality to the unknown interval.
5. Make ABSENT a typed path state and define the canonical fingerprint for every
   supported object.
6. Define metadata support per category instead of silently omitting it.
7. Move live rollback quarantine to a same-filesystem transaction-local area
   outside the normal workspace tree. Keep persistent recovery metadata external.
8. Replace hash-only recovery with source/destination state-map inspection.
9. Use a journal lifecycle with physically meaningful PLANNED, PREPARED,
   APPLYING, APPLIED, DURABLE, and COMMITTED states.
10. Document POSIX and Windows operations separately and constrain claims to
    the platform capability matrix.
11. Replace absolute safety language with conditional guarantees and refusal
    behavior.
12. Rewrite the Phase 1 contract so degraded-state behavior is part of the
    core, not a future phase.

## 4. Final workspace state machine

### 4.1 Workspace conditions

HEALTHY means:

- a trusted baseline state Sx is durably identified;
- the last accepted capture or reconciliation scanned all required paths;
- supported artifacts needed to reason about Sx are durable;
- no capture gap, unresolved rollback, or recovery lockout is open;
- normal strong capture and eligible operation-level undo/redo may be
  considered.

HEALTHY does not mean the user or an external process cannot modify the
    workspace immediately afterward. It means Rewind has no known unaccounted
    divergence at its last verified boundary.

DEGRADED means:

- a scan, CAS ingestion, metadata, boundary, or recovery action failed or was
  incomplete after the filesystem may have changed;
- the last trusted state remains useful historical evidence but is not a
  trusted current pre-state;
- normal operation attribution is suspended.

RECONCILIATION_REQUIRED means:

- the degraded condition and the reason have been persisted as a durable gate;
- passive hooks may log boundaries but may not create normal operation records;
- operation-level undo and redo are disabled across the unresolved interval;
- full reconciliation is required before strong execution or new normal
  tracking.

RECOVERY_REQUIRED means a filesystem transaction cannot be classified from
known physical states. It is a stronger mutation lockout than a capture gap.
Only recovery/diagnostic procedures may proceed until the ambiguity is resolved.

### 4.2 Transitions

~~~text
initial scan + durable manifest S0
        -> HEALTHY

HEALTHY -- complete strong/passive observation --> HEALTHY
           trusted Sn -> trusted Sn+1

HEALTHY -- capture failure after possible mutation --> DEGRADED
HEALTHY -- capture bypass/untracked boundary --> DEGRADED
DEGRADED -- durable CAPTURE_GAP marker --> RECONCILIATION_REQUIRED
RECONCILIATION_REQUIRED -- full reconciliation succeeds --> HEALTHY at Sx
RECONCILIATION_REQUIRED -- reconciliation fails --> DEGRADED

HEALTHY -- rollback begins --> transaction states
transaction states -- all steps durable and catalog committed --> HEALTHY
transaction states -- unknown physical state --> RECOVERY_REQUIRED
RECOVERY_REQUIRED -- explicit recovery classification succeeds --> HEALTHY
RECOVERY_REQUIRED -- workspace identity/path cannot be confirmed --> refusal
~~~

If the external store cannot persist the gap marker, Rewind must not assume
that the old condition remains healthy. The next invocation must require store
diagnostics and a full reconciliation before mutation.

## 5. Final baseline lifecycle

### 5.1 State and baseline definitions

A state Sx is an immutable manifest plus references to immutable CAS artifacts.
The baseline pointer names the last state that Rewind has accepted as trusted
for the current workspace boundary. A historical state can remain readable
without being the current trusted baseline.

The manifest is a canonical, sorted mapping:

~~~text
relative_path -> filesystem_state_fingerprint
~~~

It records supported metadata according to Section 9. The state identity is a
digest of the canonical serialized manifest and metadata schema version.

### 5.2 Initial baseline

rewind init performs a full scan. It must classify every encountered path,
ingest required regular-file bytes, write the manifest, and durably record S0.
If the scan is incomplete, there is no trusted S0 and normal capture does not
start.

### 5.3 Successful capture

A capture is successful only when:

1. the pre-state is trusted or was established by a complete pre-scan;
2. the command boundary is recorded according to the observation mode;
3. the post-scan completes for all required paths;
4. supported file artifacts are fully read, hashed, verified, and durably
   stored;
5. every path is classified as supported, unsupported, or unreadable;
6. the post-state manifest is canonical and durable;
7. operation/effect metadata and the new baseline pointer are durably recorded;
8. a final verification does not reveal a conflicting change.

Only then does Sn advance to Sn+1.

For passive hooks, a complete post-scan may establish a new trusted current
observation checkpoint because the filesystem has now been fully measured at
the post-boundary. That checkpoint is tagged LOW_CONFIDENCE_OBSERVATION and
does not prove that its delta was caused by the command. It may refresh the
baseline used for future current-state comparison, but it is not an ordinary
undoable operation unless the required pre-state and causality preconditions
are separately verified. This distinction lets passive observation keep the
current-state index fresh without fabricating command attribution.

### 5.4 Capture failure

Suppose the trusted baseline is Sn, a command runs, and the post-capture fails.
The correct record is:

~~~text
trusted baseline: Sn, now historical for attribution
operation: CAPTURE_FAILED
interval: UNKNOWN_INTERVAL starting at Sn
workspace condition: DEGRADED -> RECONCILIATION_REQUIRED
trusted resulting state: none
~~~

The statement “baseline did not advance” is therefore incomplete unless it is
followed by “and is no longer trusted as the current state.” A later scan must
not form Sn -> S2 as a command operation.

### 5.5 Failed-capture example

~~~text
S0: foo=A
Command 1: foo=B
post-scan fails
  records: CAPTURE_FAILED, UNKNOWN_INTERVAL(S0, unknown)
  state: RECONCILIATION_REQUIRED

Command 2: foo=C while the gate is present
  records: untrusted boundary only
  no normal operation

reconciliation:
  scans foo=C and creates trusted checkpoint S2
  records UNKNOWN_INTERVAL(S0 -> S2)
  does not create S0 -> C

Command 3: delete foo under rewind run
  pre-state: S2 (foo=C)
  post-state: S3 (foo=ABSENT)
  records: normal strong operation S2 -> S3
~~~

## 6. Degraded and reconciliation state machine

### 6.1 What passive hooks do while degraded

Passive hooks are deliberately fail-open with respect to the interactive shell,
but not with respect to trust:

- preexec records a boundary marker tagged UNTRUSTED_WHILE_DEGRADED;
- the command is not blocked by the passive hook;
- precmd records exit status and may emit diagnostics;
- no normal operation, effect attribution, or baseline advancement occurs;
- the condition remains RECONCILIATION_REQUIRED.

If a passive hook cannot acquire the writer lease because rollback, redo,
restore, or recovery owns it, the hook records a pending bypass marker in the
external boundary log if that append path is available. The command still
continues in the shell, but the next writer/recovery boundary consumes the
marker and requires reconciliation before normal tracking. If even the
boundary log is unavailable, the next Rewind invocation treats store state as
uncertain and requires diagnostics plus full reconciliation.

This means the user can continue using the shell, but Rewind will not offer
false operation-level undo for that activity.

### 6.2 What rewind run does while degraded

rewind run -- command:

1. acquires the writer lease;
2. checks for unfinished filesystem recovery;
3. runs full reconciliation;
4. refuses to execute if reconciliation cannot establish a complete trusted
   state;
5. after successful reconciliation, captures a new pre-state Sx;
6. executes the command;
7. requires a complete post-capture to record Sx -> Sy;
8. returns the command result plus a degradation diagnostic if post-capture
   fails.

It never executes a destructive command while treating the stale baseline as
the pre-state.

### 6.3 Reconciliation

Reconciliation is not an inferred diff. It is a new state observation:

~~~text
last trusted state S1
       |
       | unknown activity and failed capture
       v
UNKNOWN_INTERVAL
       |
       | complete current scan and durable artifacts
       v
trusted checkpoint Sx
~~~

The unknown interval remains explicit forever in the history. Reconciliation
does not claim to know which command made which change.

## 7. Unknown interval semantics

An UNKNOWN_INTERVAL has:

- start state ID: the last trusted state;
- end state ID: absent until reconciliation succeeds, then the checkpoint;
- detection time and reason;
- affected confidence: unknown;
- operation membership: none.

While open:

| Feature | Semantics |
| --- | --- |
| Undo | Refused for operations that would cross or depend on the interval. |
| Redo | Refused for operations that would cross or depend on the interval. |
| List | Shows CAPTURE_FAILED and the gap as separate history entries. |
| Diff | Known state-to-state diffs remain available; gap diff is reported unknown. |
| Restore | Requires reconciliation first, then a new anchored state-restore transaction. |
| Snapshots | Existing immutable snapshots are retained; no fake operation snapshot is made. |
| Future tracking | Resumes at the reconciled checkpoint only. |

The same rules apply if several commands occur while degraded. They are
coalesced into the single unknown interval rather than attributed individually.

## 8. Formal filesystem-state fingerprint model

### 8.1 Typed path states

For each path, the expected state is one of:

~~~text
ABSENT

REGULAR_FILE {
    content_hash: BLAKE3-256,
    size_bytes: unsigned integer,
    tracked_metadata: MetadataFingerprint
}

DIRECTORY {
    manifest_hash: BLAKE3-256,
    entry_count: unsigned integer,
    tracked_metadata: MetadataFingerprint
}

SYMLINK {
    target_bytes: byte string,
    target_hash: BLAKE3-256,
    tracked_metadata: MetadataFingerprint
}

UNSUPPORTED_OBJECT {
    object_kind: explicit platform/object classification,
    descriptor: non-dereferenced diagnostic data
}
~~~

UNKNOWN is not a filesystem object state. It is a scan or recovery result when
the expected object cannot be inspected or does not match a known state.

### 8.2 Regular files

Regular-file content is read as bytes and hashed with BLAKE3. Size is recorded
and cross-checked against the read result. The CAS artifact is independently
written and re-hashed; a workspace file and its CAS artifact are never coupled
with a hardlink.

### 8.3 Directories

Directory bytes are not hashed. A directory fingerprint is a canonical
manifest hash over sorted child names, child object kinds, child fingerprints,
and tracked directory metadata. Child names are normalized according to the
workspace path policy before serialization. Symlinks are recorded as leaves and
are not traversed. Empty directories therefore have deterministic fingerprints.

### 8.4 Symlinks and reparse objects

On POSIX, the literal link target is read without following the link. A
dangling symlink remains a SYMLINK with its literal target. On Windows, only a
reparse object positively identified as a supported file symlink may be treated
as SYMLINK. Junctions and unclassified reparse points are
UNSUPPORTED_OBJECT. They are not traversed or replaced by Phase 1.

### 8.5 Type changes

The fingerprint includes object type. A file with the same bytes as a directory
manifest is not equal. A missing path is not equal to any present path.
Recovery and conflict checks compare the complete variant, not just a hash.

## 9. Metadata semantics

| Metadata category | Classification | Phase 1 meaning |
| --- | --- | --- |
| File type | TRACKED | Part of the typed fingerprint. |
| Regular-file content | TRACKED | BLAKE3 bytes and size are verified. |
| Directory child topology | TRACKED | Canonical sorted child manifest. |
| Symlink target bytes | TRACKED | Literal bytes; no target traversal. |
| POSIX permission bits and executable bit | PARTIALLY TRACKED | Stored/restored when the API and privilege allow; exact platform scope is recorded. |
| Windows read-only/basic attributes | PARTIALLY TRACKED | Tracked where exposed; replacement behavior is platform-specific. |
| Ownership | PARTIALLY TRACKED | Observed where permitted; not claimed restored without verification/privilege. |
| Timestamps | PARTIALLY TRACKED | Diagnostic and heuristic data; not equality proof and not used for attribution alone. |
| Extended attributes | UNSUPPORTED for exact restore | Operations depending on them are not fully reversible. |
| ACLs | UNSUPPORTED for exact restore | No claim that ACL policy is restored. |
| Hardlink topology | PARTIALLY TRACKED | Content paths can be restored, sharing relationships may not be. |
| Sparse extent layout | PARTIALLY TRACKED | Byte content can be restored; hole layout depends on the storage path. |
| Windows alternate data streams | UNSUPPORTED in Phase 1 | Not silently included in regular-file equality. |
| Reparse/junction metadata | UNSUPPORTED unless positively classified | No traversal through an unclassified reparse point. |

If a rollback claim requires a metadata category classified UNSUPPORTED, the
operation is refused or explicitly marked partial; it is not described as an
exact restore.

## 10. Final quarantine architecture

### 10.1 Storage roles

Persistent recovery metadata remains outside the workspace:

~~~text
external Rewind store (for example ~/.rewind/ on POSIX-like systems, with the
platform's equivalent user-profile location on Windows)
  projects/<workspace-id>/
    catalog metadata
    durable journal records
    state manifests
    CAS references
    anchors
    optional archival trash
~~~

The exact user-profile path is platform-specific. The important invariant is
that the normal workspace tree is not the sole location of recovery metadata.

Transaction-local filesystem staging is a different role:

~~~text
same-filesystem staging area
  transaction/<txn-id>/
    prepared desired artifacts
    local quarantine entries
    step markers or references
~~~

The staging root is outside the protected workspace tree but on the same
filesystem as the affected path. A typical location is a private sibling of
the workspace under its containing filesystem. If no safe writable same-
filesystem location can be established, the transaction is refused. The
staging area is disposable transaction material, not the authoritative
recovery record.

### 10.2 Cross-device behavior

The workspace and the external Rewind store may be on different volumes.
Critical live movement never relies on a cross-device rename. After a
transaction is committed, local quarantine entries may be archived externally
using copy, hash verification, destination flush, and destination-directory
publication. If the archive copy fails, the committed workspace state remains
the result and the journal records ARCHIVE_PENDING.

If the workspace is deleted during recovery, the external journal, CAS, and
anchor may survive, but the local staging area may not. Rewind must report the
workspace as unavailable and must not claim that recovery completed. Once the
user explicitly reattaches or recreates the workspace, a new confined restore
transaction can use the external artifacts.

### 10.3 No normal-workspace transaction metadata

The normal workspace contains no authoritative journal, CAS, or lock state.
Temporary same-filesystem staging may be adjacent to it because the filesystem
requires locality, but it is identified as disposable staging and is always
backed by the external journal. It must not be confused with persistent
metadata.

## 11. Filesystem rollback transaction protocol

### 11.1 Plan contents

Each filesystem step has:

- step ID and transaction ID;
- operation direction: undo, redo, or explicit restore;
- source path and destination path, when applicable;
- expected complete BEFORE state map;
- expected complete AFTER state map;
- desired artifact references;
- local staging and local-quarantine paths;
- platform strategy;
- conflict and refusal policy.

The state maps include ABSENT entries. For a rename, both source and
destination are present in the map. For a replacement, the old object and the
new object are both represented.

### 11.2 Step lifecycle

~~~text
PLANNED
  durable intent and complete state maps exist; no live mutation intended

PREPARED
  desired artifacts and local quarantine/staging paths are materialized,
  verified, and flushed; live paths are still expected BEFORE

APPLYING
  a live mutation may be in progress; journal records the active step

APPLIED
  physical inspection shows the complete AFTER state map

DURABLE
  relevant file/artifact and directory-entry flush protocol completed and the
  AFTER state was rechecked

COMMITTED
  all steps are DURABLE and catalog/state pointers are committed
~~~

PLANNED and PREPARED are safe to abandon if no live path has changed.
APPLYING is never resolved by the status word alone. APPLIED can be repeated
as an inspection/no-op if the state map still matches. DURABLE can be
re-verified after restart. COMMITTED is terminal for that transaction.

### 11.3 Durability protocol

For each transaction:

1. Create the pre-rollback anchor and verify its artifacts.
2. Persist the complete external journal intent and flush the journal file and
   its containing directory as supported.
3. Materialize desired CAS artifacts in same-filesystem staging, verify their
   fingerprints, and flush them.
4. Prepare local-quarantine directories and any backup names.
5. For each step, recheck all live BEFORE states immediately before APPLYING.
6. Perform the platform-appropriate mutation.
7. Flush relevant file handles and directory-entry metadata using the platform
   protocol.
8. Inspect every source, destination, staging, and quarantine path and classify
   the result.
9. Persist APPLIED and then DURABLE journal records only after those checks.
10. After all steps are DURABLE, commit the catalog target state and mark the
    transaction COMMITTED.
11. Archive local quarantine externally only as a post-commit action.

SQLite WAL/commit can make catalog records durable. It does not make steps 6-8
atomic with the catalog and does not replace the external filesystem journal.

### 11.4 Recovery classification

For every step, recovery reads all paths in the state map:

| Classification | Physical finding | Recovery action |
| --- | --- | --- |
| BEFORE | Every live endpoint matches BEFORE; prepared artifacts are valid or reconstructible. | Apply or retry the known step. |
| AFTER | Every live endpoint and required local-quarantine result matches AFTER. | Reflush/reverify, mark APPLIED/DURABLE, continue. |
| PARTIAL | Some endpoints match BEFORE and some match AFTER, with no unexpected object. | Complete the known plan only if the remaining action is unambiguous. |
| CONFLICT | A live endpoint differs from both known maps or an external writer changed it. | Stop mutation; report conflict. |
| UNKNOWN | The path cannot be inspected, the staging artifact is unverifiable, or identity/root is uncertain. | Enter RECOVERY_REQUIRED; do not guess. |

### 11.5 Required state cases

#### Modification A -> B

Before:

~~~text
workspace/foo = REGULAR_FILE(hash A)
local-quarantine/foo-backup = ABSENT
staging/foo-desired = REGULAR_FILE(hash B)
~~~

After:

~~~text
workspace/foo = REGULAR_FILE(hash B)
local-quarantine/foo-backup = REGULAR_FILE(hash A)
staging/foo-desired = ABSENT or archived step artifact
~~~

Recovery checks both workspace/foo and the backup. A destination hash equal to
B with a missing or unexpected backup is not automatically treated as a
complete non-destructive step.

#### Deletion A -> ABSENT

Before:

~~~text
workspace/foo = state A
local-quarantine/foo-backup = ABSENT
~~~

After:

~~~text
workspace/foo = ABSENT
local-quarantine/foo-backup = state A
~~~

The live entry is moved to local quarantine; raw unlink is not the rollback
operation. If the path is already absent before APPLYING, it is a conflict
unless the plan's expected state says it was absent.

#### Creation removal ABSENT -> A

Before:

~~~text
workspace/foo = state A
local-quarantine/foo-backup = ABSENT
~~~

After:

~~~text
workspace/foo = ABSENT
local-quarantine/foo-backup = state A
~~~

This has the same physical shape as deletion during the inverse transaction,
but the operation record identifies that A was created by the undone operation.
The original pre-state is ABSENT and remains explicit.

#### Rename foo=A, bar=ABSENT -> foo=ABSENT, bar=A

Before:

~~~text
workspace/foo = state A
workspace/bar = ABSENT
~~~

After:

~~~text
workspace/foo = ABSENT
workspace/bar = state A
~~~

The plan includes both endpoints plus any collision staging name. Recovery must
not inspect bar alone. A state A at both paths, a different object at bar, or
both paths absent is PARTIAL, CONFLICT, or UNKNOWN according to the complete
map and external changes.

#### Type replacement

For file -> directory, directory -> file, symlink -> file, and the inverse:

- the source path's complete typed state is recorded;
- the existing object is moved to local quarantine where the platform permits;
- the desired object is prepared without following links;
- source and destination state maps are verified after each namespace change;
- Windows directory/reparse restrictions may make the step unsupported;
- an unclassified type or non-empty directory that cannot be safely moved
  causes refusal rather than recursive guessing.

## 12. Crash recovery state machine

Startup sequence:

1. Acquire the workspace recovery lease.
2. Find any external journal not COMMITTED.
3. Confirm workspace identity, root, volume, and path confinement.
4. Verify the anchor and required CAS artifacts.
5. Inspect all paths in every active step's BEFORE/AFTER map.
6. Classify each step.
7. Complete known BEFORE/PARTIAL steps only when the result is unambiguous.
8. Reflush and mark known AFTER steps DURABLE.
9. If any step is CONFLICT or UNKNOWN, stop at RECOVERY_REQUIRED.
10. Mark the transaction COMMITTED only after all steps and metadata are
    durably verified.

No recovery branch says “the database was committed, therefore the filesystem
was committed.” The journal is evidence of intent and observations; physical
state remains necessary.

## 13. Windows transaction model

Windows behavior is a first-class platform model:

- NTFS and ReFS are different filesystems with different clone and durability
  capabilities. The implementation must query or configure the selected
  capability rather than infer it from the OS name.
- ReplaceFileW is a regular-file replacement primitive with its own sharing,
  backup, and metadata behavior. It is not a POSIX rename and does not
  provide a multi-file transaction.
- Same-volume namespace moves can be atomic for an eligible entry, but
  cross-volume moves are copy/remove behavior or failure and are not used for
  critical quarantine.
- A file opened without compatible sharing flags may produce
  ERROR_SHARING_VIOLATION. Rewind refuses or retries within a bounded policy;
  it does not remove the lock or silently overwrite the file.
- Directory replacement is not treated as ReplaceFileW. Recursive directory
  replacement is planned as separate steps and may be unsupported where
  handles, ACLs, or reparse points prevent safe movement.
- Reparse points, junctions, and symbolic links are not interchangeable.
  Unclassified reparse points are unsupported and are never traversed.
- ACL preservation is not an exact Phase 1 guarantee. A successful content
  replacement must not be described as an ACL-preserving restore unless the
  ACL was separately verified.
- Case-insensitive collisions are detected before a step is applied.
- FlushFileBuffers and Windows namespace operations provide platform-specific
  durability requests, not a universal power-loss guarantee.

## 14. POSIX transaction model

### Linux

- rename and renameat provide atomic namespace replacement for an eligible
  single entry on one filesystem; they do not make a multi-entry transaction.
- Cross-device movement returns EXDEV and is never silently converted into an
  atomic operation.
- renameat2 is Linux-specific and optional; features such as exchange or
  no-replace are not generalized to macOS.
- fsync on a file and, where accepted, fsync on its parent directory are
  separate durability requests. The device and filesystem can still fail.
- openat/openat2, O_NOFOLLOW, and descriptor-relative operations can strengthen
  confinement. openat2 is not assumed on all Linux kernels/filesystems.
- mount points, bind mounts, and filesystem-specific behavior are checked
  during planning.

### macOS

- POSIX rename has same-volume namespace semantics but is not a multi-entry
  transaction. Cross-volume movement is not critical atomic movement.
- renameat2 is not a portable macOS primitive. Linux-specific flags are not
  assumed.
- APFS clonefile is a capability-dependent CoW optimization, not a requirement
  for correctness. Byte-copy fallback has different storage and sparse-layout
  consequences.
- fsync/F_FULLFSYNC behavior is API- and filesystem-dependent. Directory-entry
  durability is recorded as the strongest protocol the platform accepts, not
  as a universal directory barrier.
- Descriptor-relative component checks and no-follow APIs are used where
  available; Linux openat2 claims are not copied to macOS.
- APFS case sensitivity is volume-dependent, so collision detection is
  capability-driven.

## 15. Platform capability matrix

| Capability | Linux | macOS | Windows |
| --- | --- | --- | --- |
| Atomic regular-file replacement | SUPPORTED on same filesystem for eligible regular files | SUPPORTED on same volume for eligible regular files | SUPPORTED for eligible files using ReplaceFileW or an equivalent strategy, subject to sharing |
| Same-volume rename | SUPPORTED; one entry only | SUPPORTED; one entry only | SUPPORTED as a namespace operation for eligible entries |
| Cross-volume atomic rename | UNAVAILABLE; EXDEV | UNAVAILABLE | UNAVAILABLE for critical movement |
| Multi-file atomic transaction | UNAVAILABLE | UNAVAILABLE | UNAVAILABLE |
| Directory fsync | SUPPORTED where accepted by the filesystem | PARTIAL/BEST-EFFORT | PARTIAL/BEST-EFFORT |
| CoW cloning | PARTIAL; filesystem and ioctl dependent | PARTIAL; APFS capability dependent | PARTIAL; ReFS/block-clone capability dependent |
| Descriptor-relative confinement | SUPPORTED on supported Linux APIs | PARTIAL; component-wise handles/checks | PARTIAL; handle and reparse APIs |
| Symlink/reparse protection | SUPPORTED for supported no-follow paths | PARTIAL and API-scoped | PARTIAL; junction/reparse refusal required |
| File locking | Mostly advisory | Mostly advisory | Sharing modes can prevent replacement |
| Case collision detection | PARTIAL; filesystem policy is queried | PARTIAL; APFS volume policy is queried | PARTIAL; NTFS behavior is queried |
| Crash durability semantics | PARTIAL; flush requests plus filesystem/device assumptions | BEST-EFFORT/partial and filesystem-specific | BEST-EFFORT/partial and filesystem-specific |

SUPPORTED means the primitive exists for the stated scope; PARTIAL means
capability discovery and refusal cases are required; BEST-EFFORT means a
flush/request can be made but stronger behavior is not claimed; UNAVAILABLE
means the critical design does not depend on it.

## 16. Updated guarantees

Rewind is designed to:

- preserve a last trusted state without confusing it with the current state
  after capture failure;
- refuse normal attribution during an unknown interval;
- restore supported bytes and supported object types when pre/post states,
  artifacts, platform operations, and conflict checks are sufficient;
- recover supported interrupted rollback steps by inspecting all documented
  physical endpoints;
- avoid making the external store's volume boundary part of the critical
  rollback movement;
- expose uncertainty instead of silently assuming a state.

These statements are conditional on a supported local filesystem and object
type, correct OS API behavior, writable/durable storage, a valid external
store, intact journal/CAS artifacts, and no uncooperative external mutation
that wins the race against the final pre-step verification.

## 17. Explicit limitations

The architecture cannot reconstruct bytes that were never captured. It cannot
reverse remote or system-wide side effects. It cannot make unsupported
metadata exact. It cannot make a multi-file namespace change globally atomic.
It cannot turn a passive hook into a process-tracing kernel monitor. It cannot
promise protection from defective hardware, filesystem corruption, kernel
failure, power loss beyond the platform's durability protocol, or a user who
removes both the workspace and the external artifacts.

A workspace move to another volume is not followed implicitly. If the old root
is absent or its identity cannot be verified, recovery stops. The user must
explicitly reattach the workspace, after which a new confinement, volume, and
fingerprint check is required.

## 18. Updated adversarial test matrix

Each test below defines setup, action, expected physical state, expected
journal state, and expected CLI result. These are architecture acceptance
cases; they are not production code.

| ID | Setup and action | Expected physical state | Expected journal/state record | Expected CLI result |
| --- | --- | --- | --- | --- |
| T01 | S0 foo=A; command modifies foo=B; inject post-scan timeout. | foo=B; S0 is not asserted current. | CAPTURE_FAILED plus UNKNOWN_INTERVAL(S0, open); condition RECONCILIATION_REQUIRED. | Command result returned with capture warning; no normal operation. |
| T02 | After T01 run foo=C through a passive shell. | foo=C. | UNTRUSTED boundary only; no S0->C operation. | Shell continues; list shows gap. |
| T03 | Reconcile after T02. | foo=C; complete current manifest. | Durable checkpoint S2; gap closed as unknown S0->S2; no fabricated effect. | Reconcile succeeds; condition HEALTHY. |
| T04 | Run several commands while the gate is open. | All their physical writes remain on disk. | One coalesced UNKNOWN_INTERVAL; no normal operation records. | undo/redo refused for the interval. |
| T05 | After reconciliation run rewind run -- delete foo. | foo=ABSENT. | Strong S2->S3 with foo pre=REGULAR_FILE(C), post=ABSENT; COMMITTED. | Command and capture succeed; undo eligible. |
| T06 | After T01 attempt rewind run while still degraded; reconciliation scan fails. | Workspace unchanged by Rewind; command not started. | Gate remains RECONCILIATION_REQUIRED; no operation row for the command. | Refusal with reconciliation error. |
| T07 | Baseline has foo=ABSENT; strong command creates foo=A. | foo=A. | Before ABSENT, after REGULAR_FILE(A), captured and committed. | Success; undo removes creation by quarantine. |
| T08 | Baseline has foo=A; strong command deletes foo. | foo=ABSENT. | Before REGULAR_FILE(A), after ABSENT, captured and committed. | Success; undo restores from CAS. |
| T09 | Baseline foo=A; strong command changes to B. | foo=B. | Both typed states plus artifacts recorded. | Success; undo eligible only after conflict check. |
| T10 | Baseline foo=A; command replaces foo with a directory. | foo=DIRECTORY(manifest). | Type replacement recorded with both typed endpoint states. | Supported only if platform plan can move both objects; otherwise refusal. |
| T11 | Baseline foo=DIRECTORY; command replaces it with a regular file. | foo=REGULAR_FILE(B). | Directory manifest and file artifact both present in the operation. | Same conditional result as T10. |
| T12 | Baseline foo=SYMLINK(target); command replaces it with a file. | foo=REGULAR_FILE(B), target not traversed. | SYMLINK and REGULAR_FILE fingerprints recorded. | Success only with no-follow confinement. |
| T13 | Baseline foo=REGULAR_FILE; command replaces it with a symlink. | foo=SYMLINK(target), target not read. | File and literal symlink target recorded. | Success only with supported symlink APIs. |
| T14 | Workspace and external store are on different volumes; undo deletion. | Live restoration uses same-volume staging; external archive may be delayed. | Local step COMMITTED; archive state ARCHIVE_PENDING if copy fails. | Undo succeeds if local protocol succeeds; warning only for archive delay. |
| T15 | Crash before local quarantine move. | All live paths still BEFORE; prepared artifact may remain. | Step PLANNED/PREPARED. | Startup classifies BEFORE and may retry or clean staging. |
| T16 | Crash after local quarantine move but before desired install. | Source absent; backup present; destination not yet AFTER. | Step APPLYING/PARTIAL. | Recovery completes known plan or enters RECOVERY_REQUIRED if inconsistent. |
| T17 | Crash after desired install before external archive. | Workspace matches AFTER; local quarantine contains old object. | Step DURABLE/COMMITTED or ARCHIVE_PENDING. | Recovery keeps workspace result; archives later. |
| T18 | Crash before filesystem mutation with journal PLANNED. | All endpoints BEFORE. | PLANNED durable. | No-op/retry; no false completion. |
| T19 | Crash during a multi-step mutation. | Some steps AFTER, remaining steps BEFORE. | Transaction APPLYING; per-step states incomplete. | Recovery inspects every endpoint and completes only known steps. |
| T20 | Crash after mutation but before journal update. | Physical state may be AFTER while row says APPLYING. | Journal intent exists; step status stale. | Physical map wins; mark APPLIED/DURABLE after reflush. |
| T21 | Source and destination both contain unexpected objects. | Neither complete BEFORE nor AFTER. | CONFLICT or UNKNOWN. | Recovery stops; no overwrite. |
| T22 | Staging artifact is truncated or CAS hash fails. | Live workspace remains unchanged if not applied. | PREPARED invalid; storage error. | Abort before mutation; if live mutation occurred, RECOVERY_REQUIRED. |
| T23 | Second CLI starts during rollback. | No concurrent live mutation by second Rewind process. | Writer lease held; second attempt logged as busy. | Second CLI refuses or reports busy. |
| T24 | Passive hook runs during rollback. | Workspace transaction remains owned by rollback; command writes are not assumed covered by the rollback. | Pending bypass marker; after the writer releases, reconciliation is required. | Shell remains responsive; no false capture; normal tracking stays gated. |
| T25 | IDE edits a target between preflight and apply. | Target diverges from expected BEFORE. | Step remains PLANNED/APPLYING with conflict observation. | Rollback stops with CONFLICT; no overwrite. |
| T26 | External process edits a non-target path during rollback. | Non-target change remains. | May be recorded as external drift; transaction policy decides if whole-state verification fails. | Continue only if target invariants remain valid; otherwise refuse. |
| T27 | Windows target has an exclusive incompatible handle. | Target remains unchanged. | Step failure with sharing violation; transaction not committed. | Refusal with lock diagnostic; staging retained/cleaned per journal. |
| T28 | Workspace path is moved to another volume during recovery. | Old path absent; new path not implicitly trusted. | RECOVERY_REQUIRED with identity/volume mismatch. | Refuse until explicit reattachment and revalidation. |
| T29 | Symlink points outside workspace during scan. | Link itself is unchanged; target not read. | SYMLINK literal target or security classification. | Capture can proceed for the link; rollback never follows it. |
| T30 | Directory is swapped for a symlink during traversal. | No outside path is modified. | Scan/step marked CONFLICT or UNKNOWN. | Refuse the affected operation. |
| T31 | Reparse point is a junction on Windows. | Junction target is not traversed. | UNSUPPORTED_OBJECT. | Operation requiring it is refused or partial; no follow. |
| T32 | Case-insensitive collision foo and FOO is planned. | No live replacement occurs. | Planning conflict recorded before APPLYING. | Refusal with collision diagnostic. |
| T33 | POSIX cross-device rename returns EXDEV. | No claim of atomic movement. | Step remains planned or switches to an explicitly unsupported copy protocol. | Refuse critical step unless a documented copy/verify/remove plan applies. |
| T34 | Linux directory fsync is available and accepted. | Entry mutations are flushed by the selected protocol. | DURABLE after recheck; conditional device guarantee. | Continue; no universal hardware claim. |
| T35 | macOS directory durability call is unavailable/partial. | Namespace result is verified; stronger durability not claimed. | DURABLE_BEST_EFFORT or refusal according to policy. | Report platform limitation; never call it equivalent to Linux. |
| T36 | Windows ReplaceFileW succeeds for a regular file. | Destination and backup match the AFTER map. | APPLIED then DURABLE after flush/recheck. | Continue; directory/multi-file semantics remain separate. |
| T37 | Unsupported ACL/xattr changes with byte content unchanged. | Bytes may be readable, metadata exactness unknown. | PARTIALLY_TRACKED or capture gap; not FULLY_REVERSIBLE. | Exact undo refused or explicitly partial. |
| T38 | Unknown physical state after repeated crash/restart. | At least one endpoint matches neither map. | RECOVERY_REQUIRED; no COMMITTED state. | Refuse mutation and require explicit repair/reconciliation. |

## 19. Hostile review of the required sequence

The following review uses the exact sequence requested by the verifier. “Know”
means known from durable evidence, not inferred from a stale pointer.

| Stage | Trusted state | Actual filesystem | Rewind knows / does not know | Records | Undo | Redo | Recovery/continuation |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Initial S0 | S0 | foo=A | Knows S0 and foo=A. | S0 | No operation yet. | None. | HEALTHY; normal capture allowed. |
| Command 1 post-scan fails | S0 historical only | foo=B | Knows command boundary failed and foo may differ; does not know complete transition. | CAPTURE_FAILED; UNKNOWN_INTERVAL starts at S0. | Refused for command 1. | None for command 1. | DEGRADED then RECONCILIATION_REQUIRED. |
| Command 2 foo=C passive | None current-trusted | foo=C | Knows only an untrusted boundary and later physical observations; does not know whether C includes B or external edits. | Untrusted boundary; gap remains open. | Refused. | Refused. | Shell continues; Rewind remains gated. |
| Reconciliation | New S2 after scan | foo=C | Knows complete current S2; does not know S0->C causal steps. | Reconciliation checkpoint; UNKNOWN_INTERVAL(S0,S2). | No undo across gap. | No redo across gap. | HEALTHY at S2. |
| Command 3 delete foo | S2 | foo=ABSENT | Strong pre/post states if run through rewind run. | Operation S2->S3. | Allowed for command 3 if conflict check passes. | Not yet; becomes available after undo. | Healthy if post-capture succeeds. |
| Command 4 recreate foo=D | S3 | foo=D | Strong pre ABSENT and post file D if supervised. | Operation S3->S4. | Allowed for command 4. | Not yet. | Healthy if post-capture succeeds. |
| Undo command 4 begins | S4 | foo=D | Knows expected current file D and desired ABSENT; anchor and step maps planned. | Rollback PLANNED/PREPARED. | In progress, not yet reported complete. | Not yet. | Writer lease blocks others. |
| Crash halfway through undo | S4 is historical until txn resolves | Some endpoint combination: foo may be D or ABSENT; local backup may exist. | Knows all allowed BEFORE/AFTER maps; does not assume which physical step ran. | APPLYING or stale step record. | Blocked pending recovery. | Blocked pending recovery. | Startup classifies BEFORE/AFTER/PARTIAL/CONFLICT/UNKNOWN. |
| Restart and recover undo | Target S3 after successful recovery | foo=ABSENT, old D local/external archive as applicable | Knows AFTER only after both endpoint and durability checks. | APPLIED/DURABLE/COMMITTED; operation UNDONE. | Undo is complete. | Redo command 4 is eligible. | HEALTHY at S3, unless archive is pending (then warning only). |
| Redo command 4 starts | S3 | foo=ABSENT | Knows expected redo pre ABSENT and desired D artifact. | New redo transaction PLANNED/PREPARED. | Undo of redo pending. | In progress, not yet complete. | Writer lease held. |
| Crash during redo | S3 historical until resolved | foo may be absent or D; staging/local backup inspected. | Knows maps, not physical completion from catalog row alone. | APPLYING. | Blocked pending recovery. | Blocked pending recovery. | Recovery completes known plan or RECOVERY_REQUIRED. |
| Restart after redo recovery | S4 if successful | foo=D and backup/archives accounted for | Knows AFTER after physical inspection and reflush. | COMMITTED; redo complete. | Eligible again subject to current conflict. | Redo is no longer pending. | HEALTHY at S4. |
| Workspace moved to another volume | No trusted root until reattached | Old root absent; new root may contain foo=D | Knows external identity and old root reference; does not silently trust new volume/path. | RECOVERY_REQUIRED or ATTACH_REQUIRED. | Refused. | Refused. | Explicit reattachment and full reconciliation required. |
| IDE modifies foo after reattachment | No operation pre-state until scan | foo=IDE value | Reconciliation can know current value; it cannot attribute IDE edit to old command. | New reconciliation checkpoint or capture gap. | Refused until healthy and a new operation exists. | Refused for unresolved interval. | Reconcile; then future strong capture. |
| Rollback attempted during unresolved IDE drift | Current trusted state absent or mismatch | foo differs from expected step state | Knows conflict; does not know that overwriting is safe. | CONFLICT/RECOVERY_REQUIRED. | Refused; no mutation. | Refused. | User must reconcile/explicitly establish a new state. |
| Symlink attack attempted | Healthy only if no gate/txn | Path component replaced with symlink/reparse target | Knows confinement check failed or object is unsupported; does not follow target. | SECURITY_CONFLICT or UNSUPPORTED_OBJECT. | Refused. | Refused. | No outside write; require rescan/reconciliation. |

The sequence never requires Rewind to guess that C was caused by command 2, that
an old D backup is still available, that a moved workspace is the same root, or
that a symlink target is safe.

## 20. Documentation consistency audit

The following documents were audited and synchronized:

| Document | Synchronization result |
| --- | --- |
| .ai/PROJECT_CONTEXT.md | Terminology, conditional guarantees, external-store roles, and state degradation aligned. |
| .ai/RESEARCH.md | Passive-observation limits, fingerprint model, same-filesystem staging, and platform scope aligned. |
| .ai/TECHNICAL_FEASIBILITY.md | Case analyses now distinguish strong capture, passive observation, unknown intervals, and typed states. |
| .ai/ARCHITECTURE_PROPOSAL.md | Normative topology, state machine, fingerprint, quarantine, transaction, and platform rules aligned. |
| .ai/MVP_SPEC.md | MVP guarantees and refusal cases aligned with the Phase 1 contract. |
| .ai/ROADMAP.md | Phase 1 no longer depends on a future daemon or DAG for core correctness. |
| .ai/SAFETY_ANALYSIS.md | Threat mitigations no longer call external trash atomic or promise universal safety. |
| .ai/COMPETITIVE_ANALYSIS.md | Marketing language no longer expands the technical guarantee boundary. |
| .ai/DIFFERENTIATION.md | Differentiation claims distinguish design goals from verified guarantees. |
| .ai/DECISIONS.md | ADRs added/updated for degradation, reconciliation, fingerprints, quarantine, journal recovery, platforms, and conditional guarantees. |
| .ai/HANDOFF.md | Phase 1 handoff includes the degradation gate and no implementation authority during Phase 0.7. |
| .ai/PHASE_0_REPAIR_REPORT.md | Marked historical and superseded where its earlier baseline/quarantine wording conflicts. |
| .ai/PHASE_0_6_ARCHITECTURE_LOCK_REPORT.md | Marked historical; stale “locked/ready” claims removed from normative status. |
| phases/phase-01-foundation.md | Rewritten as the final Phase 1 implementation contract. |

The authoritative definitions are intentionally repeated in the Phase 1
contract and this report so an implementation does not need to infer behavior
from a marketing or historical document.

## 21. Remaining uncertainties

1. Exact directory-entry durability behavior differs among filesystem versions,
   mount options, and hardware. The design records the strongest supported
   protocol and keeps the guarantee conditional.
2. Windows sharing behavior depends on the other process's handle flags. The
   architecture specifies refusal/retry behavior but cannot compel another
   process to release a handle.
3. The precise API combination for component-wise no-follow confinement varies
   between macOS releases and Windows reparse configurations. Unsupported cases
   are refusal cases, not implicit follows.
4. Sparse files, hardlink topology, ACLs, xattrs, alternate data streams, and
   ownership require additional platform work for exact preservation.
5. A workspace move that removes the identity evidence requires an explicit
   reattachment UX; automatic discovery is intentionally not part of the
   safety contract.
6. Performance budgets require measurement on target filesystems. No measured
   benchmark is being treated as a correctness guarantee in Phase 0.7.

These are implementation and platform-validation uncertainties, not unresolved
semantics. Each has an explicit partial, refusal, or conditional outcome.

## 22. Final recommendation

Do not begin Phase 1 until the independent verifier accepts this document and
the synchronized Phase 1 contract. The architecture is ready for independent
verifier approval, subject to the conditional platform limitations and
explicit refusal cases documented above.

Completion gate:

- [x] No production code created in Phase 0.7
- [x] No Cargo project created in Phase 0.7
- [x] No CLI, shell hook, database, CAS, rollback, daemon, or filesystem
      driver implementation created
- [x] Trusted baseline and advancement are formally defined
- [x] Failed capture invalidates current trust and opens a reconciliation gate
- [x] DEGRADED and RECONCILIATION_REQUIRED exist
- [x] UNKNOWN_INTERVAL is explicit and not attributed
- [x] rewind run reconciles before execution when degraded and refuses on failure
- [x] ABSENT, regular file, directory, symlink, and unsupported states exist
- [x] Metadata semantics are explicit
- [x] Cross-device quarantine is not a critical rename
- [x] Source and destination recovery verification is defined
- [x] Journal states are physically meaningful
- [x] SQLite is not treated as a filesystem transaction
- [x] Windows and POSIX semantics are distinct
- [x] Absolute crash guarantees are removed
- [x] Platform capability matrix exists
- [x] The 38-case adversarial matrix is explicit
- [x] The hostile sequence was reviewed
- [x] Phase 1 scope is implementation-ready without future-phase semantics

Final status: ARCHITECTURE READY FOR INDEPENDENT VERIFIER APPROVAL.
