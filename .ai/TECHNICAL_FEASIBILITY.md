# Technical Feasibility Analysis

Status: Phase 0.7 synchronized; implementation verification in progress

## 1. Observation feasibility

Userspace shell hooks can delimit a foreground shell boundary but cannot
observe every filesystem call, identify every writer, or preserve a deleted
unindexed file. Watchers can improve change discovery but can overflow,
coalesce, miss late background writes, and do not by themselves provide
causality.

Therefore:

- passive hooks are lower-confidence observations;
- rewind run performs explicit pre/post scans and has the strong Phase 1
  capture contract;
- a failed scan opens a degradation gate;
- reconciliation restores current-state trust without inventing causality.

## 2. Command archetypes

| Command | Strong capture result | Passive result | Limitation |
| --- | --- | --- | --- |
| npm install | Workspace files can be captured when the pre/post scans complete | Low-confidence observation; external package caches are outside scope | Remote/cache side effects are not reversed |
| Python script modifying many paths | Full pre/post state if resources and scans complete | May time out and open reconciliation | Intermediate states are not separately attributed |
| sudo/system package mutation | Workspace effects may be observed | External/system effects remain outside scope | No system package transaction |
| curl pipe to shell | Workspace state may be captured | Boundary and workspace scan only | Network and home-directory effects are external |
| opaque binary | State effects can be captured if within workspace | Causal attribution is lower confidence | Userspace does not inspect internal calls |
| background process | Only contained process-boundary policy is strong | Late writes may appear in a later gap or observation | No universal process-tree containment |
| destructive deletion | Pre-state is strong if rewind run scanned it first | Passive undo is conditional and may be unavailable | Never-captured bytes cannot be reconstructed |

## 3. Filesystem state feasibility

Regular-file bytes can be hashed and independently stored. Directories can be
represented by canonical child manifests. Symlinks can be represented by
literal target bytes without dereference. Missing paths can be represented by
ABSENT. Special files can be classified without reading them and refused for
restore.

This model is more useful for recovery than a nullable content hash because it
distinguishes missing, present, wrong-type, unsupported, and uninspectable
states.

## 4. Rollback feasibility

Single-entry same-filesystem moves/replacements are usable building blocks on
all target families, with different APIs and restrictions. They do not make a
multi-entry rollback atomic. A journal can make an interrupted supported
sequence inspectable and resumable when source, destination, staging, and local
quarantine state maps are known.

External trash on another volume cannot be the critical move destination.
Same-filesystem local quarantine plus post-commit external archival is feasible.

## 5. Durability feasibility

SQLite can durably catalog records according to its settings. File and
directory-entry flushes can request filesystem durability, but the strength of
that request is OS-, filesystem-, mount-, device-, and failure-dependent.
Recovery therefore records requested/observed durability and avoids universal
power-loss claims.

## 6. Platform feasibility

Linux provides rename/renameat and, on suitable kernels, renameat2 and
descriptor-relative APIs. macOS provides POSIX rename and APFS clonefile on
capable volumes but is not a Linux API target. Windows provides
ReplaceFileW/MoveFile-family operations, sharing modes, and reparse-point
controls; those primitives differ from POSIX behavior.

The complete capability matrix is in the Phase 0.7 report and Phase 1
contract. Unsupported combinations are refused rather than normalized into a
false common guarantee.

## 7. Storage feasibility

Native CoW is an optimization. Linux FICLONE, APFS clonefile, and ReFS/block
clone capabilities are conditional. Independent byte copies are the correctness
fallback. Hardlinks are unsuitable for immutable CAS because an in-place
workspace write could mutate the historical object.

Sparse layout and hardlink topology may not survive a byte-copy fallback.
Content equality and exact metadata claims are kept separate.

## 8. Conflict feasibility

Before each rollback step, current typed states can be compared with expected
BEFORE states. If they differ, the step can stop before replacement. This is a
conflict gate, not a merge algorithm. Generic three-way merge remains outside
Phase 1.

## 9. Performance feasibility

Full scans and hashing scale with the workspace and dirty set. Passive hooks
must use a bounded shell-facing budget and may fail open into reconciliation.
Strong capture is an explicit command and may take longer. No fixed latency
number is a safety guarantee. Benchmarks must vary file count, file size,
cache state, filesystem, CoW capability, and failure injection.

## 10. Feasibility conclusion

The Phase 1 semantics are feasible as a conservative local filesystem system:
strong capture, typed state, explicit degradation, same-filesystem rollback
staging, and state-map recovery. They are not feasible as a universal
filesystem transaction, universal causal observer, or universal hardware
durability guarantee. The design is implementation-ready only with its
refusal cases intact.
