# Market Differentiation and Value Proposition

Status: Phase 0.7 synchronized

Rewind is a proposed local workspace recovery layer between shell history and
version-control snapshots. Its strongest differentiator is explicit
uncertainty handling, not a universal promise of undo.

## Core distinction

| Dimension | Typical history/snapshot tool | Rewind design |
| --- | --- | --- |
| Command context | String/time record or snapshot time | Passive boundary or supervised command |
| State model | Often file-content focused | Typed path states including ABSENT and unsupported |
| Failed observation | Missing/partial history | DEGRADED and RECONCILIATION_REQUIRED |
| Unknown changes | Implicit | UNKNOWN_INTERVAL in visible history |
| Rollback storage | Tool-specific | External persistent evidence plus same-filesystem local quarantine |
| Interrupted rollback | Tool-specific | Physical source/destination state-map classification |
| Redo | May re-execute or be absent | Reapply recorded post-state |
| Platform scope | Often one platform | Explicit Linux/macOS/Windows capability matrix |

## Positioning

Rewind is intended for developers and automation agents that need a local
operational safety net for supported workspace state while retaining the
ability to say “this transition is unknown” and stop.

The system is deterministic in its state comparison and journal planning, but
its results remain conditional on supported objects, storage, operating-system
behavior, and external writers.

## Boundaries

The design does not reverse remote requests, system-wide package changes,
process memory, or bytes that were never captured. It does not claim
multi-file filesystem atomicity or exact restoration of unsupported metadata.
