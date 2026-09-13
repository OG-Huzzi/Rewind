# RewindUndo Minimum Viable Product Specification

Status: synchronized with Phase 0.7; Phase 1 implementation under verification

## 1. MVP objective

The MVP is a conservative local workspace recovery engine. Its core value is
trusted state capture and explicit refusal when trust is lost. It is not a
promise that every shell command or filesystem object can be reversed.

## 2. Supported workflow

The intended flow is:

~~~text
init -> HEALTHY at S0
run -- command -> strong Sx -> Sy operation
undo -> anchored journaled inversion when eligible
redo -> state reapplication when eligible
capture failure -> DEGRADED/RECONCILIATION_REQUIRED
reconcile -> trusted checkpoint Sx, gap remains un-attributed
~~~

## 3. Platform and storage scope

Phase 1 targets supported local Linux, macOS, and Windows filesystems using the
capability matrix. Persistent metadata and CAS are external to the workspace.
Same-filesystem transaction staging is required for critical movement.

## 4. Command concepts

| Command concept | MVP behavior |
| --- | --- |
| init | Explicit root, identity, external store, full scan, S0 |
| status/doctor | Condition, open gap, transaction, support, and storage diagnostics |
| reconcile | Full current scan; trusted checkpoint without fabricated operation |
| run -- command | Reconcile if required, capture pre/post state, refuse if pre-reconcile fails |
| undo | Invert the latest eligible operation after typed conflict checks |
| redo | Reapply captured post-state, never rerun a command |
| list/show/diff | Show known operations and explicit unknown intervals |
| snapshot/restore | Explicit immutable state and anchored state restore |

The final command names and flags are an implementation detail. The safety
semantics are not.

## 5. Capture requirements

The strong path requires:

- trusted or freshly reconciled pre-state;
- complete supported scan;
- independent CAS artifact verification;
- command boundary;
- complete post-scan;
- durable state, effect, and operation records;
- final verification.

If post-capture fails after the command runs, the command may have changed the
workspace but has no normal reversible operation record. Rewind enters the
degradation gate.

Passive hooks may provide a low-confidence observation after a complete scan.
They do not automatically make that observation undoable.

## 6. Rollback requirements

An eligible undo creates an anchor, checks current states, and uses a journal
whose steps include source, destination, staging, and local-quarantine
fingerprints. ABSENT is explicit. The external store is not the critical
quarantine destination when it is on another filesystem.

## 7. Supported object policy

Regular files, directory topology, and non-followed supported symlinks have
deterministic fingerprints. FIFOs, sockets, device nodes, junctions, and
unclassified reparse points are unsupported. ACLs, xattrs, alternate data
streams, exact ownership, hardlink topology, and sparse layout have the partial
or unsupported scope in the Phase 1 contract.

## 8. Acceptance criteria

The MVP is acceptable only when:

1. failed capture cannot create a fake next operation;
2. degraded state and reconciliation are observable;
3. rewind run refuses to execute if required reconciliation fails;
4. typed states are used for file, directory, symlink, unsupported, and absent;
5. rollback inspects all involved endpoints after interruption;
6. SQLite is not treated as filesystem atomicity;
7. platform-specific refusal cases are surfaced;
8. the 38 adversarial architecture cases are exercised on applicable
   platforms/filesystems;
9. no absolute crash-safety claim is made.
