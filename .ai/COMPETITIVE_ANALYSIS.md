# Competitive Analysis

Status: Phase 0.7 synchronized; product analysis, not a safety contract

## 1. Landscape

Terminal history tools record commands. Shell undo tools usually restore
selected files with limited state knowledge. Version control protects committed
history. Volume snapshots can provide broad recovery where the filesystem
supports them. IDE local history protects editor-visible content.

Rewind's intended distinction is the combination of explicit workspace state,
command-boundary metadata, strong capture through an explicit wrapper, and a
conservative rollback protocol for supported local filesystem objects.

## 2. Comparison

| Capability | Shell history | VCS | Volume snapshot | Rewind design |
| --- | --- | --- | --- | --- |
| Command metadata | Strong | Weak | Weak | Boundary metadata |
| Uncommitted/untracked files | Usually no state | Conditional | Broad | Supported when captured |
| Operation boundary | Command string | Commit | Snapshot time | Passive/strong tiers |
| Missing-path recovery | Usually no | Conditional | Filesystem-dependent | ABSENT-aware state |
| Cross-volume local rollback | Not applicable | Not applicable | Platform-dependent | Not a critical atomic primitive |
| Crash inspection | Usually absent | Repository-dependent | Snapshot-dependent | Source/destination journal maps |
| Unknown interval disclosure | Rare | Branch/status concepts | Snapshot gap | First-class UNKNOWN_INTERVAL |
| Redo | May rerun | Usually manual | Snapshot restore | Reapply captured state |

## 3. Differentiation boundaries

The design is differentiated by how it handles uncertainty:

- failed capture is a trust-state transition, not merely a missing row;
- reconciliation establishes current trust without inventing command causality;
- missing, wrong-type, symlink, directory, and unsupported states are distinct;
- external storage and same-filesystem live staging have separate roles;
- platform limitations are visible rather than hidden behind one rename claim.

These are architecture goals. Market comparison does not imply that an
implementation has been validated or that every command is reversible.

## 4. Non-comparable claims

Rewind does not claim to replace Git, a kernel audit system, a remote database
transaction, a system package manager, or a hardware-backed snapshot service.
External side effects and unsupported metadata remain outside its local
rollback boundary.
