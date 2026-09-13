# Phase 1 Foundation Contract

Status: implementation contract; Phase 1 implementation under verification,
September 2026

This document defines the implementation boundary after the Phase 0.7
architecture finalization. The normative failure, fingerprint,
quarantine, durability, and platform rules are in
.ai/PHASE_0_7_ARCHITECTURE_FINALIZATION_REPORT.md.

Phase 1 may implement only the contract below. It must not silently weaken a
refusal rule in order to make a command appear successful.

## Preconditions

Before Phase 1 work begins:

1. The workspace has an explicit identity and a canonical root selected by the
   user. Root inference is not a safety mechanism.
2. The external Rewind store is writable, has a stable workspace record, and can
   persist the state marker, manifests, CAS artifacts, and recovery journal.
3. The initial full scan has completed. Every encountered path is either
   represented by a supported fingerprint or explicitly classified as
   unsupported/unreadable.
4. The initial manifest and all required CAS artifacts have been verified and
   durably recorded as state S0.
5. The selected filesystem and object types satisfy the support matrix in this
   document. An unsupported type is a refusal condition for any operation that
   would need to restore it.
6. Only one Rewind mutation or recovery transaction owns the workspace writer
   lease at a time. Passive hooks may fail open for the shell, but they do not
   bypass an active writer; a pending bypass marker forces reconciliation.
7. No unfinished recovery transaction exists. If one exists, startup recovery
   runs before normal capture or rollback.

## Supported guarantees

Phase 1 supports the following conditional guarantees:

- Strong capture through rewind run records a trusted pre-state only after a
  complete pre-scan and durable artifact verification. A complete post-scan
  records the command transition from Sx to Sy.
- A strong capture failure after execution never advances the trusted baseline
  and opens a degraded/reconciliation-required gate.
- Passive hooks record shell boundaries and may produce lower-confidence
  observations after a complete scan. They do not prove that every observed
  delta was caused by the command and do not automatically make that record
  undoable.
- A captured operation is undoable only when every required source and
  destination fingerprint, artifact, path-confinement check, and conflict
  precondition is sufficient for the requested inversion.
- Redo applies the recorded post-state. It does not execute the original
  command.
- Rollback uses a same-filesystem transaction-local staging/quarantine area for
  critical movement. The external store is not treated as an atomic trash
  destination.
- An interrupted supported rollback is classified from the physical state of
  every source, destination, staging, and local-quarantine path described in the
  journal. Recovery may complete a known plan or refuse; it does not infer
  completion from a database row alone.
- Supported regular files, directories, and non-followed symbolic links have
  deterministic state fingerprints. ABSENT is a first-class expected state.
- Successful reconciliation establishes a new trusted current state but does
  not invent operations for the interval that was not observed.

These are guarantees only under the assumptions in Explicit non-guarantees.

## Explicit non-guarantees

Phase 1 does not provide:

- operation attribution for commands executed while the workspace is
  degraded/reconciliation-required;
- recovery of bytes that were never captured before their only copy was removed;
- reversal of network, system-wide, remote-database, process-memory, or other
  external side effects;
- exact restoration of unsupported objects or unsupported metadata such as ACLs
  and extended attributes;
- atomicity across multiple filesystem entries, across volumes, or between the
  filesystem and SQLite;
- a promise that a power failure, defective storage device, filesystem bug,
  kernel failure, or external process cannot damage data;
- universal POSIX/Windows equivalence;
- preservation of hardlink topology or sparse-layout topology when the selected
  storage path cannot preserve it;
- passive-hook visibility of transient or late background-process writes;
- a claim that a recorded passive observation is a causal command delta;
- automatic following of a workspace moved to another volume. Reattachment is
  explicit and requires revalidation;
- an exact timestamp, latency, or storage-overhead SLA before benchmarks exist.

## State machine

The externally visible workspace condition is:

~~~text
HEALTHY
  trusted baseline exists and no unresolved transaction/gap is present

DEGRADED
  a capture, storage, scan, or recovery operation has failed; the last
  baseline is historical only and must not be used as the current pre-state

RECONCILIATION_REQUIRED
  the degraded condition is durably recorded as a gate; normal operation
  attribution, operation undo, and redo are disabled
~~~

The state transitions are:

~~~text
initial full scan + durable S0
        -> HEALTHY

HEALTHY -- complete capture --> HEALTHY, baseline Sn -> Sn+1
HEALTHY -- capture failure --> DEGRADED
DEGRADED -- gap marker durable --> RECONCILIATION_REQUIRED
DEGRADED -- marker cannot be persisted --> store/recovery error; next startup
                                  must require full reconciliation
RECONCILIATION_REQUIRED -- full reconciliation succeeds --> HEALTHY at Sx
RECONCILIATION_REQUIRED -- reconciliation fails --> DEGRADED
HEALTHY -- rollback/recovery --> transaction state machine, then HEALTHY or
                                  RECOVERY_REQUIRED
any state -- unknown physical recovery state --> RECOVERY_REQUIRED
~~~

RECOVERY_REQUIRED is a separate transaction/recovery lockout, not a fourth
capture confidence level. It blocks mutation until an explicit diagnostic or
recovery action resolves the journal.

## Observation model

### Passive hooks

The pre-command hook records only a boundary marker: session, command
description, working directory, and time. The post-command hook records exit
status and requests a bounded scan. It must fail open for the shell, but its
failure is not fail-open for Rewind trust:

1. A complete scan can create a low-confidence observation and a new observed
   state. The post-state may refresh the trusted current checkpoint, but the
   observation is not automatically an undoable operation.
2. A timeout, permission error, overflow, missing root, CAS failure, or
   incomplete classification creates a capture-gap record.
3. The baseline pointer remains the last trusted state, but it is immediately
   marked stale for attribution. The workspace enters DEGRADED and then
   RECONCILIATION_REQUIRED.
4. A later passive command is logged only as an untrusted boundary while that
   gate is present. It does not create a normal operation or advance the
   baseline.

Passive hooks are never allowed to convert a stale baseline plus a later scan
into a fabricated Sx -> Sy command operation.

When a passive post-scan is complete and durable while the workspace was
HEALTHY, its fully observed post-state may become the next trusted current
checkpoint for freshness. The history entry remains an observation with
unproven causality, and operation-level undo is available only if independent
pre-state and attribution checks make it eligible.

### Strong supervisor

For rewind run -- command:

1. Acquire the writer lease.
2. If the workspace is not HEALTHY, run full reconciliation. If it fails, do
   not execute the command.
3. Scan and persist the current pre-state Sx. This scan includes explicit
   ABSENT states for paths relevant to the planned capture and completes CAS
   ingestion for supported regular files.
4. Execute the command under the supported process-boundary policy.
5. Complete a post-scan and persist all supported post-state artifacts.
6. If all capture conditions succeed, record Sx -> Sy and advance the baseline.
7. If post-capture fails after the command ran, record CAPTURE_FAILED plus an
   UNKNOWN_INTERVAL marker, enter DEGRADED/RECONCILIATION_REQUIRED, and report
   that the command ran without a reversible operation record.

## Capture model

A normal operation record is valid only when:

~~~text
trusted pre-state
  + command boundary under the selected observation mode
  + complete supported post-state
  + all needed CAS artifacts durable
  + manifest and operation metadata durable
  + final state verification succeeds
  = captured transition Sx -> Sy
~~~

Baseline advancement is a metadata pointer change, not a filesystem commit:

~~~text
advance(Sx, Sy) only after:
  complete scan
  every encountered path classified
  artifacts verified and durable
  operation/effect records durable
  state Sy durable
  workspace marker updated
~~~

An SQLite commit may make catalog rows durable according to its configured
SQLite/OS protocol. It does not commit the workspace filesystem.

## Degradation model

When a post-scan or storage step fails after a command may have changed the
workspace:

- The operation is CAPTURE_FAILED, not COMPLETED.
- The last baseline remains a historical reference only.
- A durable UNKNOWN_INTERVAL record starts at the last trusted state.
- No subsequent passive command is attributed normally.
- list shows the failed boundary and gap marker.
- diff can show known historical states but reports the unknown interval as
  unavailable.
- operation-level undo and redo are disabled across the gap.
- explicit snapshot restore is allowed only after a successful reconciliation
  and a new anchor/preflight transaction.
- future operations resume only from the reconciled state Sx.

No code path may use the stale baseline as a trusted pre-state simply because
the state pointer was not advanced.

## Reconciliation model

Reconciliation is a full, explicit current-state scan:

1. Stop normal capture under the writer lease.
2. Inspect every eligible path without following symlinks or junctions.
3. Build canonical fingerprints and ingest supported file bytes.
4. If any required path or artifact is unreadable, remain
   RECONCILIATION_REQUIRED and do not execute a requested command.
5. Persist a reconciliation checkpoint Sx.
6. Close the UNKNOWN_INTERVAL as S_last_trusted -> Sx without creating an
   operation or assigning causality.
7. Clear the gate and make Sx the trusted baseline.

Reconciliation can be invoked explicitly and is mandatory at the start of
rewind run when the gate is present. It may not be silently substituted by a
partial post-scan.

## Unknown interval semantics

~~~text
S1 -- unobserved commands/capture failure -- UNKNOWN_INTERVAL -- reconcile -->
Sx
~~~

The interval may contain zero, one, or many commands and may include external
processes. Rewind records its endpoints and reason, not a guessed delta.

- Undo: unavailable for records that would cross or depend on the gap.
- Redo: unavailable for the same range; it never re-executes commands.
- List: displays the gap as a first-class boundary.
- Diff: historical endpoint diffs remain inspectable; the gap diff is unknown.
- Restore: requires successful reconciliation and a new journaled state
  restore; it does not retroactively label the gap.
- Snapshots: prior immutable snapshots remain valid; the reconciliation
  checkpoint is not an operation snapshot.
- Future operations: start at Sx and can be captured normally.

## Rollback model

Undo first creates a pre-rollback anchor, validates the current state against
the operation's expected post-state, and plans a transaction whose steps carry
both source and destination expected states. A step is not reduced to a
single destination hash.

The transaction-local quarantine is on the same filesystem as the affected
path. Existing entries are moved there before replacement when needed. After
all steps are durable and committed, their local quarantine entries may be
copied and verified into the external archival store, which may be on another
volume. That archival copy is not part of the critical rollback commit.

If a source or destination differs from every state documented in the journal,
rollback stops with CONFLICT or RECOVERY_REQUIRED. It does not overwrite the
unknown object.

## Redo model

Redo applies the captured post-state artifacts and metadata for an undone
operation. It first verifies the current workspace against the expected
pre-state of the redo transaction. It uses the same source/destination
journal protocol and same-filesystem quarantine. It never reruns the original
command and never assumes a missing path is a regular file.

An operation with an unknown interval, unsupported object, incomplete metadata,
or unresolved conflict is not redoable.

## Crash recovery

The persistent journal is outside the workspace. SQLite may index it, but the
filesystem journal record and its durability protocol are authoritative for
physical recovery.

Transaction states:

~~~text
PLANNED -> PREPARED -> APPLYING -> APPLIED -> DURABLE -> COMMITTED
                         |             |
                         +-> RECOVERY_REQUIRED
~~~

- PLANNED: complete step plan and expected before/after state maps are durable;
  no live path was intentionally changed.
- PREPARED: desired artifacts, staging paths, and local-quarantine paths exist,
  are hashed, and are durable; live paths are still expected-before.
- APPLYING: a mutation may have occurred; recovery must inspect all endpoints.
- APPLIED: the step's complete after-state map is observed.
- DURABLE: the after-state was rechecked after the platform-specific flush
  protocol; the journal records that fact.
- COMMITTED: all steps are durable and the catalog points at the target state.

Recovery classifies each step as BEFORE, AFTER, PARTIAL, CONFLICT, or UNKNOWN
using the expected state of every source, destination, staging, and local
quarantine path. It may retry only a step whose current physical state is
known to be BEFORE or AFTER, or whose partial state can be completed without
guessing. UNKNOWN and CONFLICT stop mutation.

## Filesystem support

| Object or property | Phase 1 status | Semantics |
| --- | --- | --- |
| Regular file bytes | SUPPORTED | BLAKE3 content hash and independent CAS artifact |
| Directory topology | SUPPORTED | Deterministic child-manifest fingerprint |
| Non-followed symlink | SUPPORTED where platform APIs identify it | Literal target bytes; never dereferenced |
| ABSENT path | SUPPORTED | Explicit fingerprint variant in every expected state map |
| Permissions/executable/read-only bit | PARTIALLY SUPPORTED | Tracked where the platform exposes it; exact restore is platform-scoped |
| Timestamps | PARTIALLY TRACKED | Diagnostic/change heuristic; not equality proof |
| Ownership | PARTIALLY TRACKED or unsupported without privilege | Not claimed as restored unless verified |
| xattrs and ACLs | UNSUPPORTED for exact rollback | Affected operations are not fully reversible |
| Hardlink topology | PARTIALLY SUPPORTED | Paths may be restored as independent files |
| Sparse layout | PARTIALLY SUPPORTED | Byte content can be restored; hole layout may change |
| FIFO, socket, device node | UNSUPPORTED | Never read, created, or deleted by rollback |
| Windows junction/reparse object not proven to be a file symlink | UNSUPPORTED | No traversal or replacement |

## Platform differences

| Capability | Linux | macOS | Windows |
| --- | --- | --- | --- |
| Atomic regular-file replacement | SUPPORTED on same filesystem with rename-family APIs | SUPPORTED on same volume with POSIX rename semantics | SUPPORTED for eligible files with ReplaceFileW/rename semantics when sharing permits |
| Same-volume rename | SUPPORTED; cross-device returns EXDEV | SUPPORTED; cross-volume is not atomic | SUPPORTED as a namespace operation on one volume; not a multi-file transaction |
| Directory fsync | SUPPORTED where filesystem accepts fsync on directory | PARTIAL/BEST-EFFORT; use available fsync/F_FULLFSYNC protocol | PARTIAL/BEST-EFFORT; FlushFileBuffers does not create a multi-entry transaction |
| CoW cloning | PARTIAL, filesystem-dependent FICLONE | PARTIAL, APFS clonefile where supported | PARTIAL, ReFS/block-clone capabilities only |
| Descriptor-relative confinement | SUPPORTED on Linux APIs, including openat2 where available | PARTIAL; component-wise descriptor checks required | PARTIAL; handle/reparse-point APIs are not POSIX equivalents |
| Symlink/reparse protection | SUPPORTED for supported API paths | PARTIAL and API-scoped | PARTIAL; junctions/reparse points require explicit refusal rules |
| File locking | Mostly advisory | Mostly advisory | Sharing modes can deny replacement and behave as mandatory for callers |
| Crash durability | OS flush requests are available; device/filesystem behavior remains conditional | Flush behavior is filesystem/API-specific | Flush and ReplaceFileW behavior is API/filesystem-specific |

## Concurrency

The workspace writer lease covers reconciliation, strong capture metadata
commit, rollback, redo, restore, and recovery. A passive hook that cannot get a
non-blocking lease records a pending bypass marker in the external boundary log
when possible. After the writer releases, that marker forces reconciliation
before normal tracking resumes. If the boundary log is unavailable, the next
Rewind invocation treats store state as uncertain and requires diagnostics plus
full reconciliation. The hook does not block the shell or create a false
operation.
An IDE or external process has no Rewind lease; preflight rechecks fingerprints
immediately before each filesystem step and treats divergence as a conflict.

## Security

All workspace-relative paths are normalized and confined. Symlinks are recorded
as objects, not followed. Linux descriptor-relative operations are preferred;
macOS and Windows use their platform-specific handle/reparse protections with
more refusal cases. Case-insensitive collisions are detected before mutation.
The external store is not trusted as permission to write outside the selected
workspace root.

## Testing

Phase 1 must implement the 38-scenario matrix in the Phase 0.7 report. Each
scenario specifies setup, action, expected physical state, expected journal
state, and CLI result. Passing only the happy path is not a completion signal.

The implementation completion gate is behavioral: all applicable matrix cases
must pass on the supported platform/filesystem combinations, with failures
reported as refusals or explicit degraded/recovery states. The Phase 0.7
restriction on production code was historical; this contract is the authority
for the Phase 1 implementation and verification gate.
