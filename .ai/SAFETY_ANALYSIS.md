# RewindUndo Safety and Threat Analysis

Status: Phase 0.7 synchronized; analysis only

## 1. Safety doctrine

Rewind should preserve user control by refusing an operation when the required
state, path, metadata, lock, or recovery evidence is not available. This is a
design objective under the assumptions in the Phase 1 contract, not a promise
against every filesystem, hardware, kernel, or external-process failure.

## 2. Primary defenses

| Risk | Defense | Refusal condition |
| --- | --- | --- |
| Failed capture leaves disk beyond baseline | CAPTURE_FAILED, UNKNOWN_INTERVAL, reconciliation gate | No normal attribution or undo across the gap |
| Out-of-band edit during undo | Per-step source/destination fingerprint checks | CONFLICT before overwrite |
| Process/power interruption | External journal and physical state-map recovery | RECOVERY_REQUIRED if state is unknown |
| External trash on another volume | Same-filesystem local quarantine; external post-commit archive | No critical cross-device rename |
| Symlink/junction escape | No-follow scan, descriptor/handle confinement, reparse refusal | SECURITY_CONFLICT or UNSUPPORTED_OBJECT |
| CAS mutation | Independent artifact copies; no hardlinks as CAS snapshots | Storage-corruption refusal |
| Concurrent Rewind writers | Workspace writer lease | Busy/refusal |
| Windows open handle | Sharing-aware preflight and apply | Sharing violation/refusal |
| Unsupported metadata | Explicit category classification | Partial or refusal, not exact restore |
| Missing path | ABSENT fingerprint | Type/state conflict if unexpected |

## 3. Degradation and unknown interval

The last baseline pointer is not sufficient evidence after a post-capture
failure. A failed capture records the boundary and enters DEGRADED, then
RECONCILIATION_REQUIRED once the marker is durable. Passive hooks can continue
to report boundaries for the shell, but those boundaries are untrusted and do
not advance the baseline.

Reconciliation scans the actual filesystem and creates a trusted checkpoint.
It records the interval as unknown rather than attributing it to the next
command. Undo, redo, and exact diff across that interval are unavailable.

## 4. Rollback threats

### 4.1 Before mutation

The anchor, external journal intent, desired artifacts, local staging, and
writer lease must be ready before live movement. If any preparation step fails,
the workspace is unchanged by Rewind and the transaction is not committed.

### 4.2 During movement

A step may move an old entry to local quarantine, install a prepared entry, or
perform a platform-specific replacement. The journal records APPLYING before
the mutation. Recovery inspects all endpoints; it does not trust a stale
APPLIED flag.

### 4.3 After movement

The workspace result is not committed until the complete after-map is observed,
relevant flushes are requested, and the state is rechecked. External archival
may still be pending after the live transaction commits.

### 4.4 Unknown state

If any endpoint matches neither the documented before nor after state, or
cannot be read, recovery enters RECOVERY_REQUIRED. It does not overwrite the
object merely because the intended target is known.

## 5. Path and object safety

The workspace root is explicit. Relative paths are normalized, case-collision
checked according to the volume, and confined to the selected root. Symlinks
are state objects, not traversal shortcuts. Windows junctions and unclassified
reparse points are unsupported. Linux openat/openat2 capabilities are used
where available; macOS and Windows use their own descriptor/handle checks and
refuse weaker cases.

FIFOs, sockets, and device nodes are classified without reading their contents.
Phase 1 does not create, delete, or restore them.

## 6. Metadata safety

Exact guarantees cover only categories marked TRACKED. Permissions and
read-only/executable attributes are platform-scoped partial categories.
Timestamps are not content proof. ACLs, extended attributes, alternate data
streams, and unverified ownership are unsupported for exact restore. If an
operation depends on one of those categories, the CLI reports partial
reversibility or refuses the operation.

## 7. Storage safety

CAS ingestion writes an independent temporary artifact, hashes it, flushes it,
and publishes it only after verification. Hardlinks are not used for immutable
CAS objects. A disk-full or I/O error leaves the operation uncaptured and
opens the degradation gate if the command may already have run.

The external store is not assumed to be on the same filesystem as the
workspace. Its loss or unavailability is a storage failure, not evidence that
the workspace is unchanged.

## 8. Concurrency safety

The writer lease serializes Rewind capture commits, reconciliation, rollback,
redo, restore, and startup recovery. Hooks that fail to obtain a non-blocking
lease append a pending bypass marker when the external boundary log is
available; the next writer boundary requires reconciliation. An IDE or other
process can still write; per-step checks detect target divergence.

## 9. Platform safety

Linux and macOS POSIX rename operations are single-entry same-filesystem
namespace operations, not multi-file commits. Windows ReplaceFileW is a
regular-file API with sharing and metadata rules, not a POSIX directory
primitive. Directory replacement, locks, reparse points, and flush behavior
are separately classified in the capability matrix.

## 10. User-facing safety outcomes

The safe outcomes are explicit:

- success with a committed supported transition;
- successful command with CAPTURE_FAILED and reconciliation required;
- conflict/refusal before live mutation;
- committed live rollback with ARCHIVE_PENDING external archival;
- RECOVERY_REQUIRED with no further mutation;
- unsupported/partial result with the exact unsupported category.

The system must not translate these outcomes into a generic “completed”
message.

## 11. Phase 4 additions (time-range views)

The timeline (`rewind inspect timeline`) adds a read-only rendering path and
was analyzed against the existing hazard classes:

- **No new mutation path.** The timeline opens the workspace through the
  same diagnostic path as the existing inspect commands, acquires no lease,
  runs no enforcement, and writes nothing. Proven by a test asserting the
  catalog is byte-identical across an invocation even with a pending
  degradation marker present (no gate transition, marker not consumed).
- **Uncertainty is not laundered.** Unknown intervals and watcher
  degradations inside the requested range force `history_complete=false` in
  the machine output and an explicit "Rewind does not claim a complete or
  trustworthy history here" statement in the human output. They are rendered
  whole, never trimmed, closed, or reinterpreted.
- **Absence of watcher events is not evidence of absence.** When the watcher
  log's oldest observation postdates the requested `since`, the output states
  that the log proves nothing about the earlier portion (watcher evidence is
  advisory and rotates; it is never proof of filesystem state).
- **Deterministic presentation.** Ordering is `(timestamp, tier, id)` with
  published tier ranks; identical store contents produce byte-identical JSON,
  so the view cannot be used to launder nondeterministic claims.
- **Validation is airtight.** Malformed or inverted ranges exit 2 with a
  diagnostic and mutate nothing; the RFC 3339 parser is byte-driven (no
  slicing panics on non-UTF-8 input) and rejects impossible dates, including
  leap seconds outside 23:59.

Range-based restore remains excluded (ADR-015): a range does not identify a
provable target state, and rendering evidence must not invite restore
semantics the model cannot support.

## 12. Phase 5 additions (named pipes)

- **No new write primitives.** FIFO restore reuses the journaled step
  machinery end-to-end: quarantine-by-rename of the replaced object, parent
  confinement, post-mutation confinement verification, per-step durability,
  and post-apply re-scan comparison. The only new filesystem call is
  `mkfifo` on a path already confinement-checked.
- **No privilege window.** The pipe is created with 0o600 and then set to
  the recorded mode, so a manifest-unspecified world-accessible pipe never
  exists, even briefly.
- **No content claims.** A FIFO's in-flight bytes are kernel state; the
  fingerprint holds existence and mode only. Nothing in any output suggests
  pipe *content* was captured or restored.
- **Scan safety.** Classification reads `file_type` only — the scanner
  cannot block on or consume from a FIFO, and the pre-existing regular-file
  read path is unchanged.
- **Refusals stay refusals.** Sockets and device nodes keep
  `is_supported_for_restore() == false` and block rollback at plan time with
  descriptors naming the object class. Windows refuses a foreign
  `NamedPipe` manifest at materialization instead of guessing.
- **Compatibility.** The serde variant is additive; every previously readable
  manifest stays readable. An old binary reading a new manifest fails with
  an explicit unknown-variant error — honest refusal, not corruption.

## 13. Rollback performance phase (path-scoped step verification)

The optimization replaced the two per-step full workspace scans in rollback
with a fingerprint scan of exactly the affected path
(`scan::scan_fingerprint_at`, ADR-018). Safety analysis:

- **No safety-relevant behavior change.** The per-step scans' results were
  consumed only as `manifest.get(path)` before the change; the path-scoped
  scan computes the identical fingerprint through the scanner's shared
  per-entry classification, so every decision input (idempotent
  short-circuit, pre-conflict comparison, post-verification
  `compatible_after`) receives the same value it always did. This equivalence
  is asserted directly by a test comparing both observation paths across
  every supported object type on each platform.
- **Global deviation detection unchanged.** The final full-scan state
  comparison (`state_id` must equal the target) remains the authoritative
  check for any external interference anywhere in the workspace; the entry
  scans and the planner are untouched. Unrelated-path scan errors now
  surface at that final scan instead of aborting a step earlier — the same
  `RecoveryRequired` abort class, later timing.
- **Durability, quarantine, confinement untouched.** Journal writes happen
  at exactly the same points under `synchronous = FULL`; quarantine-by-rename
  and confinement verification are byte-for-byte the same code. Recovery
  paths keep their full scans.
- **Nothing is assumed unchanged between steps.** Each step re-observes its
  path from the live filesystem; there is no caching, no watcher input, and
  no trust of stale evidence.
