# RewindUndo Project Context

Status: Phase 1 implementation under verification

## Purpose

Rewind is a local-first workspace state and recovery system for terminal
workflows. Its useful boundary is supported filesystem state inside an
explicitly initialized workspace. It records command boundaries, state
manifests, and recoverable artifacts so a user can inspect and, when the
required states are known, invert or reapply a captured operation.

Rewind is not a kernel monitor, a version-control replacement, a remote-side
effect reverser, or a universal transaction layer for operating systems.

## Phase 1 user model

The Phase 1 command concepts are:

- init: establish workspace identity, external store, and initial trusted state;
- status/doctor: show HEALTHY, DEGRADED, RECONCILIATION_REQUIRED, or
  RECOVERY_REQUIRED and explain the gate;
- reconcile: perform a full current-state scan and close an unknown interval;
- run -- command: reconcile first when needed, then strongly capture a command;
- undo: invert an eligible captured operation after conflict and support checks;
- redo: reapply captured post-state artifacts, never rerun a command;
- list/show/diff: inspect known history and expose unknown intervals;
- snapshot/restore: manage explicit states through the same anchored rollback
  protocol.

## Core principles

1. Refuse to guess. If a state, path, object, or recovery step is not known,
   refuse the mutation and report why.
2. A state pointer is not proof of current disk state. A failed capture makes
   the old baseline historical for attribution and opens a reconciliation gate.
3. Passive hooks are low-confidence boundary observation. They may fail open for
   shell responsiveness, but they never fail open for Rewind trust.
4. Strong capture uses a full pre-state and post-state protocol.
5. A missing path is a state: ABSENT. Directories, symlinks, and unsupported
   objects are not regular files with special hashes.
6. Critical rollback movement stays on the affected filesystem. The external
   store holds persistent evidence and optional archival copies.
7. SQLite metadata durability and filesystem durability are separate concerns.
8. Platform differences are part of the public safety contract.
9. Undo and redo use recorded state; redo does not rerun the command.
10. Claims are conditional on supported objects, APIs, storage, and external
    behavior. The project does not promise protection from every environmental
    failure.

## Workspace states

HEALTHY means the last accepted baseline is trusted at the last verified
boundary. DEGRADED means a capture or storage failure may have left disk
different from that baseline. RECONCILIATION_REQUIRED is the durable gate that
stops normal attribution, operation undo, and redo until a full scan succeeds.
RECOVERY_REQUIRED is a separate lockout for an ambiguous filesystem
transaction.

The complete state and unknown-interval model is authoritative in
PHASE_0_7_ARCHITECTURE_FINALIZATION_REPORT.md and
phases/phase-01-foundation.md.

## Scope boundary

Phase 1 concerns explicit workspaces, supported local filesystem objects,
external metadata/CAS, strong command capture, lower-confidence passive
observation, single-writer rollback, and crash inspection. It does not include
causal DAGs, multi-select TUI behavior, a continuous watcher daemon, generic
three-way merges, remote state reversal, or system-wide package transaction
control.

## Storage boundary

Persistent journals, manifests, CAS artifacts, anchors, and catalog metadata
are outside the normal workspace, for example under ~/.rewind/ on POSIX-like
systems and the platform-equivalent user-profile store on Windows.
Same-filesystem transaction staging may be a private sibling or other local
staging area required by the filesystem, but it is disposable and is never the
sole recovery record.

## Terminology

- State: an immutable canonical manifest and artifact references at a known
  observation point.
- Trusted baseline: the state currently eligible as a pre-state for the
  selected capture mode.
- Observation: a filesystem state measurement; it is not automatically a
  causal operation.
- Operation: a command transition with sufficient pre/post evidence.
- Unknown interval: activity between a last trusted state and a later
  reconciliation checkpoint where command attribution is unavailable.
- Fingerprint: the typed state representation of one path, including ABSENT.
- Anchor: a pre-rollback state artifact created before live rollback movement.
- Local quarantine: same-filesystem transaction material holding replaced
  entries until the critical transaction commits.
- External archive: optional post-commit copy of local quarantine into the
  persistent store.

## Historical Phase 0.7 rule

During Phase 0.7 this repository contained documentation only and production
code was prohibited. Phase 1 is now authorized only within the frozen
foundation contract above; later-phase functionality remains out of scope.
