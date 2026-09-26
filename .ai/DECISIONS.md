# Architecture Decision Records

Status: Phase 0.7 synchronized set

Each ADR uses the same five fields. The Phase 0.7 finalization report is the
normative cross-document reference.

## ADR-001: Explicit workspace identity

### Context
Filesystem recovery is unsafe when the root is silently guessed or when a
workspace move is assumed to be the same root.

### Decision
Require explicit initialization and an external workspace identity. A changed
path or volume requires explicit reattachment and full reconciliation.

### Alternatives
Infer the root from the current directory, follow a path string, or search the
user profile automatically.

### Rationale
Identity and confinement must be verified before a journaled mutation.

### Consequences
Moved workspaces may require user action. Automatic discovery is not a safety
feature.

## ADR-002: Two observation tiers

### Context
Passive shell hooks have low boundary cost but cannot provide process tracing
or a complete pre-state. An explicit wrapper can perform full scans.

### Decision
Use passive hooks for lower-confidence boundary observations and rewind run for
strong pre/post capture. A passive hook failure opens degradation rather than
silently keeping normal trust.

### Alternatives
Use only passive hooks, require a daemon, or claim causal attribution from
timestamps alone.

### Rationale
The distinction represents the physical observability boundary.

### Consequences
Passive records may be inspectable but not undoable. Strong capture has higher
latency and can refuse execution when reconciliation fails.

## ADR-003: Degraded workspace condition

### Context
Leaving the baseline pointer at Sn after a failed scan does not mean disk is
still Sn.

### Decision
Use HEALTHY, DEGRADED, and RECONCILIATION_REQUIRED. A capture failure creates
CAPTURE_FAILED and UNKNOWN_INTERVAL and disables normal attribution.

### Alternatives
Advance to a partial state, keep Sn trusted, or attribute the next post-scan
to the next command.

### Rationale
The system must not fabricate an operation boundary from a stale baseline.

### Consequences
Some passive failures require an explicit or automatic full reconciliation.
History contains gaps instead of false operations.

## ADR-004: Reconciliation checkpoint

### Context
After a gap, current disk can be scanned without knowing which command caused
the transition.

### Decision
Reconciliation creates a trusted checkpoint Sx and closes the unknown interval
as an un-attributed span. It does not create S_last -> Sx as an operation.

### Alternatives
Guess a net operation, discard all history, or continue with Sn.

### Rationale
Current-state trust and command causality are separate facts.

### Consequences
Future capture resumes at Sx. Undo/redo cannot cross the interval.

## ADR-005: Typed filesystem fingerprints

### Context
A missing path has no regular-file hash, directories have no byte stream, and
symlinks must not be dereferenced.

### Decision
Represent ABSENT, REGULAR_FILE, DIRECTORY, SYMLINK, and UNSUPPORTED_OBJECT as
typed fingerprints. Use canonical child manifests for directories.

### Alternatives
Use nullable content hashes, hash directory metadata ad hoc, or follow links.

### Rationale
Recovery and conflict checks need deterministic state identity for every path.

### Consequences
Recovery plans are more verbose. Unsupported and unknown outcomes are explicit.

## ADR-006: Metadata support is explicit

### Context
Permissions, timestamps, ACLs, xattrs, ownership, sparse layout, hardlinks,
and Windows reparse details do not have one portable meaning.

### Decision
Classify each category as tracked, partially tracked, or unsupported. Do not
claim exact rollback for unsupported metadata.

### Alternatives
Ignore metadata, restore it opportunistically, or make all metadata mandatory.

### Rationale
Silent omission would make an apparent exact restore misleading.

### Consequences
Some operations are partial or refused. Platform-specific verification remains
necessary.

## ADR-007: External persistent store and local staging

### Context
Persistent recovery metadata must survive destructive workspace commands, while
critical moves must remain on the affected filesystem.

### Decision
Keep catalog, journal, anchors, and CAS outside the workspace. Use disposable
transaction-local staging/quarantine on the same filesystem as each target.
Archive local quarantine externally only after commit.

### Alternatives
Put everything inside the workspace, rename directly to external trash, or use
cross-device copy/remove as if it were atomic.

### Rationale
The two roles have incompatible locality requirements.

### Consequences
The workspace can be deleted while local staging is lost. External evidence
survives, but recovery then reports the workspace unavailable and requires
explicit recreation/reattachment.

## ADR-008: Filesystem journal separate from SQLite

### Context
SQLite ACID applies to catalog pages, not external directory entries.

### Decision
Persist a complete external filesystem journal intent and physical observations.
SQLite may index it but is not the filesystem transaction.

### Alternatives
Rely on a SQLite transaction, use an in-memory journal, or use a whole-directory
swap.

### Rationale
Recovery needs physical before/after evidence after a process or power failure.

### Consequences
Journal and catalog reconciliation are required. Multi-file atomicity is not
claimed.

## ADR-009: Source and destination state maps

### Context
A move/replacement can crash before, during, or after moving its source, and
destination-only inspection cannot distinguish all cases.

### Decision
Every step records expected BEFORE and AFTER states for every source,
destination, staging, and local-quarantine path.

### Alternatives
Inspect only the destination hash, trust the step status, or restore from a
single anchor without classification.

### Rationale
The physical namespace is the evidence needed for idempotent recovery.

### Consequences
Step plans and recovery checks are larger. Unexpected states become explicit
refusals.

## ADR-010: Journal lifecycle

### Context
A single APPLIED flag cannot say whether staging, namespace mutation, and
durability flush occurred.

### Decision
Use PLANNED, PREPARED, APPLYING, APPLIED, DURABLE, and COMMITTED, with
RECOVERY_REQUIRED for ambiguity.

### Alternatives
Use only pending/complete, or infer physical completion from SQLite status.

### Rationale
Each state corresponds to a physical observation or durable intent.

### Consequences
Recovery can retry only known states. An ambiguous state blocks mutation.

## ADR-011: Platform-specific filesystem operations

### Context
POSIX rename/renameat, Linux renameat2, macOS APFS, Windows ReplaceFileW,
sharing modes, reparse points, and flush behavior differ.

### Decision
Maintain separate Linux, macOS, and Windows protocols and a capability matrix.
Use only same-filesystem/volume primitives for critical single-entry movement.

### Alternatives
Expose one universal rename abstraction or target only one operating system.

### Rationale
False equivalence would turn platform edge cases into data-loss claims.

### Consequences
Some platforms return partial/best-effort capability and more operations are
refused there.

## ADR-012: Conditional safety language

### Context
Hardware, kernel, filesystem, process, and external-storage failures cannot be
eliminated by a userspace design.

### Decision
State guarantees with supported-object, platform, storage, and external-process
assumptions. Describe refusal and recovery outcomes instead of universal
phrases.

### Alternatives
Use absolute “never corrupts” or “always recovers” product language.

### Rationale
The guarantee boundary must be testable and honest.

### Consequences
Marketing claims are narrower. Independent verification can evaluate explicit
conditions and limitations.

## ADR-013: Redo is state reapplication

### Context
Rerunning a command can repeat network calls, depend on new inputs, or produce a
different result.

### Decision
Redo applies captured post-state artifacts through the same journal protocol.

### Alternatives
Rerun the original command or synthesize a reverse shell command.

### Rationale
Recorded state is deterministic within the supported filesystem boundary.

### Consequences
Redo is unavailable when post-state artifacts or metadata are incomplete.

## ADR-014: Single writer and external actors

### Context
Two Rewind transactions or an IDE can mutate the workspace between checks.

### Decision
Use a writer lease for Rewind mutations and recheck each affected path
immediately before apply. A passive hook that cannot acquire the lease appends a
pending bypass marker when the external boundary log is available; the next
writer boundary requires reconciliation before normal tracking resumes.

### Alternatives
Allow concurrent writers, serialize only SQLite, or force external applications
to use Rewind.

### Rationale
The lease protects Rewind coordination; the conflict check handles actors that
do not hold it.

### Consequences
Busy, lock, and conflict results are normal safe outcomes.

## ADR-015: Time-range views are read-only evidence renderings

### Context
Phase 4 (time-range views) could either render recorded evidence inside a
time range or offer range-based restore. Restore-from-range would require
inferring a target state from heterogeneous evidence tiers.

### Decision
The timeline (`rewind inspect timeline`) is informational only. It merges
operations, snapshots, passive boundaries, unknown intervals, and bounded
watcher summaries with per-tier evidentiary labels, uses a half-open range
(since inclusive, until exclusive), orders deterministically, and forces an
explicit `history_complete=false` whenever an unknown interval or watcher
degradation intersects the range. It opens no lease, runs no enforcement,
and cannot initiate a mutation. RFC 3339 parsing is a small dependency-free,
exhaustively tested module (`src/humantime.rs`).

### Alternatives
Restore-from-range, trimming spans to the range, or inferring missing
history from watcher events.

### Rationale
Rendering evidence cannot turn the watcher into proof of filesystem state,
and range restore would fabricate target states that the model cannot prove.

### Consequences
Users inspect history by time but restore only through the existing
state-anchored undo/redo/restore commands. Missing history stays missing.

## ADR-016: Package-specific recipes are deferred

### Context
Phase 4 asked whether package-manager recipes (cargo, npm, pip, …) can be
integrated honestly. A package action's workspace file effects are already
recorded by generic capture; everything that makes it a package action —
registry state, lockfile regeneration from remote data, global caches,
shared stores, post-install scripts — is remote or system-wide state the
local model cannot prove or reverse.

### Decision
Defer recipes. A recipe layer would add labels, not provable evidence, and
presenting `npm install` as a reversible package transaction would claim
guarantees the state model does not have.

### Alternatives
Implement per-ecosystem recipes with best-effort undo, or record only
manifest fingerprints as extra capture metadata.

### Rationale
The project refuses to guess: a recipe without provable pre/post package
state is a presentation of uncertainty as reversibility.

### Consequences
Package-manager activity remains visible in history exactly as any other
command capture. Recipes return only when a use case adds provable evidence,
such as recording a lockfile fingerprint as part of strong capture.

## ADR-017: POSIX named pipes are first-class; special files stay refused

### Context
Every non-file, non-directory, non-symlink object was recorded as
UNSUPPORTED, and one unsupported object in a captured state made the whole
operation irreversible — so an ordinary POSIX workspace containing a FIFO
could not roll back at all.

### Decision
Support POSIX FIFOs as a first-class object type (`Fingerprint::NamedPipe`):
existence and permission mode are the entire state; there is no content
hash. The scanner classifies from file type alone (never opens or reads the
pipe), restore creates it with a private 0o600 mode before applying the
recorded mode, inside the existing journal/quarantine/confinement/
verification machinery. Unix sockets, character and block devices keep
precise refusal descriptors and stay unsupported; on Windows the scanner can
never produce the variant and a foreign manifest containing it is refused at
materialization.

### Alternatives
Leave FIFOs unsupported (rollback stays poisoned), or generalize to all
special files including sockets and device nodes (unrestorable runtime or
machine state).

### Consequences
Workspaces with FIFOs roll back deterministically on Linux and macOS; the
capability matrix (`.ai/PHASE_5_PLATFORM_EXPANSION.md` §3) is the normative
object×platform record. Extended attributes, ACLs, ownership, and sparse
files remain deferred until they can be recorded *and* restored faithfully.
