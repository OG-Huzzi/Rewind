# Phase 4 Contract — Time and Ecosystem Integrations

Status: CONTRACT (implementation follows this document). Baseline for this
phase: `main` at `c4aeef7` (Phase 3 verified; Phase 3 audit fixes merged via
PR #1; local suite 129 passed / 0 failed before any Phase 4 change).

## 1. Scope

Phase 4 delivers **one safe vertical slice** and **one documented
evaluation**:

1. **Time-range history view (implemented):** `rewind inspect timeline
   --since <RFC3339> [--until <RFC3339>] [--json]` — a read-only, merged,
   evidence-tier-aware view of everything Rewind *recorded* inside a
   selected time range, with uncertainty exposed, never filled.
2. **Package-specific recipes (evaluated, deferred):** an analysis of what
   the core state model can honestly represent about package-manager
   actions, with the deferral rationale recorded here and in DECISIONS.md.

### Explicitly excluded from Phase 4

- Any restore, undo, or redo entry point from the timeline. The timeline is
  **informational only**; it opens no lease, runs no enforcement, writes
  nothing, and cannot initiate a mutation. Existing commands remain the only
  mutation paths.
- Causal attribution of filesystem events to commands (attribution stays
  with strong capture and the Phase 1.4 boundary protocol).
- Remote, system-wide, and shared side effects (registries, global caches,
  system services) — permanently outside local rollback guarantees.
- Localized/timezone display: timestamps are parsed from RFC 3339 input
  (UTC or explicit offset) and always rendered as RFC 3339 UTC. No
  locale-dependent formatting.
- Any new persistence, schema change, or watcher behavior change.

## 2. What a time-range view is

The timeline merges **recorded evidence rows** whose timestamp falls inside
the range, from five sources, each labeled with its evidentiary value:

| Tier | Source | Timestamp used | Evidentiary value |
|---|---|---|---|
| `OPERATION` | catalog `operations` (strong capture) | `created_at` | The only tier that is ever undo/redo-eligible; keeps its recorded `confidence` and `reversibility`. |
| `SNAPSHOT` | catalog `snapshots` + `states` | `created_at` | Explicitly named, durable states. |
| `PASSIVE_BOUNDARY` | catalog `passive_boundaries` | `started_at` | **Low-confidence** shell observation; command text may be a fragment; never undoable. |
| `UNKNOWN_INTERVAL` | catalog `unknown_intervals` | `created_at` (open intervals extend to "now" at query time) | A span whose causal and state evidence is not trustworthy. Rendered as uncertainty, never filled. |
| `WATCH` | watcher `degraded.json` records and `events.log` summary | record `recorded_at` / event `timestamp` | Advisory observation evidence. The watcher is not proof of filesystem state; the raw log rotates, so its coverage is bounded and stated. |

The watcher contributes two bounded things, never a raw event dump: the
degradation records inside the range, and a per-kind event count summary
with the first/last observed timestamps inside the range.

### Range semantics

- The range is **half-open: `since` inclusive, `until` exclusive**.
  Adjacent ranges tile the timeline with no overlap and no duplication.
- `--since` defaults to the workspace's recorded creation time; `--until`
  defaults to the invocation's current time. Both defaults are deterministic
  per invocation and are echoed in the output `range` object.
- `--since >= --until` is a usage error (exit 2). Malformed timestamps are
  usage errors (exit 2). Nothing is written and no state is touched on
  validation failure.
- An entry belongs to the range when `since <= entry_ts < until`. Span
  records (unknown intervals, passive boundaries) are included when they
  **intersect** the range and are shown whole — never trimmed to the range
  (trimming would fabricate bounds).

### Ordering

Entries are ordered by `(timestamp, tier_rank, id)` where `tier_rank` is a
fixed documented precedence (OPERATION < SNAPSHOT < UNKNOWN_INTERVAL <
PASSIVE_BOUNDARY < WATCH) and `id` is the source record's identifier. The
same store contents always produce byte-identical JSON.

### Completeness and uncertainty

- The output carries a `coverage` object: per-tier counts, watcher log
  generations read, the oldest timestamp actually present in the watcher
  log, and the number of open unknown intervals intersecting the range.
- `history_complete` is `true` **only** when no unknown interval intersects
  the range and no watcher degradation record intersects it. Any
  uncertainty anywhere in the range makes it `false`. The human output
  states this in words.
- A watcher log whose oldest observed record postdates `since` is reported
  as possibly incomplete for the range's earlier portion — the absence of
  watcher events before that point is never presented as proof that nothing
  happened.
- Missing history is rendered as missing: an empty range is an empty
  timeline, not an inference.

## 3. Time parsing (new module `src/humantime.rs`)

- Input: RFC 3339 timestamps — `YYYY-MM-DDTHH:MM:SS(.fraction+)?(Z|±HH:MM)`;
  lowercase `t`/`z` accepted; a space may replace `T`. Fractional seconds are
  accepted and truncated to microseconds.
- Conversion uses the days-from-civil algorithm (proleptic Gregorian), no
  external dependency. Leap years, month lengths, and offsets are covered by
  exhaustive unit tests and round-trip with the formatter.
- Output formatting is always `YYYY-MM-DDTHH:MM:SS.ffffffZ` (UTC).

## 4. User-facing workflow

```
rewind inspect timeline --since 2026-09-26T10:00:00Z --until 2026-09-26T11:00:00Z
rewind inspect timeline --since 2026-09-26T12:00:00+02:00 --json
```

Human output: a header with the resolved range and `history_complete`
verdict, one line per entry (timestamp, tier, identifier, summary), then a
coverage block. JSON output is the interface of record for machine
consumers and includes `schema_version: 1`.

Exit codes: `0` success (including an empty timeline), `2` usage/validation
errors.

## 5. Safety invariants preserved

- Read-only: the timeline opens the workspace through the same diagnostic
  path as `inspect history` and performs no lease acquisition, no
  enforcement, no gate transitions, and no writes anywhere (proven by a
  byte-identical `metadata.sqlite` + workspace fingerprint test).
- The watcher's advisory artifacts are read, never modified, and never
  treated as proof of filesystem state.
- Unknown intervals keep their plan-time meaning: they are reported as
  uncertainty spans, and the timeline's rendering of them does not close,
  open, or alter them.

## 6. Package-specific recipes — evaluation outcome (deferred)

Evaluated against the core model: a package-manager action (e.g.
`cargo add`, `npm install`, `pip install`) is observable by Rewind only
through its **workspace file effects**, which the existing generic capture
already records; everything that makes it a "package action" — registry
state, lockfile regeneration from remote data, global caches, shared
stores, post-install scripts — is remote or system-wide state the local
model cannot prove or reverse. A "recipe" that presented
`npm install` as a reversible package transaction would claim guarantees
the state model does not have. The honest representation reduces to the
existing generic command capture, so a recipe layer adds presentation, not
capability. **Deferred** until a use case is identified where a recipe can
add provable evidence (e.g. recording a package manifest fingerprint as
part of strong capture) rather than cosmetic labels. See DECISIONS.md
ADR-011.

## 7. Testable acceptance criteria

- **AC1 Empty history:** a freshly initialized workspace produces a valid,
  empty timeline (exit 0, `history_complete: true`, zero entries).
- **AC2 Boundaries:** a record exactly at `since` is included; a record
  exactly at `until` is excluded. Two adjacent ranges tile with no
  duplication.
- **AC3 Ordering and determinism:** entries sorted by
  `(timestamp, tier_rank, id)`; two invocations over an unchanged store
  produce byte-identical JSON.
- **AC4 Uncertainty:** an unknown interval intersecting the range appears
  as an `UNKNOWN_INTERVAL` entry with its open flag; one entirely outside
  the range does not appear; any intersecting interval forces
  `history_complete: false`.
- **AC5 Tier honesty:** passive boundaries are labeled low-confidence;
  operations carry their recorded confidence/reversibility; watcher
  evidence is labeled advisory.
- **AC6 Watcher evidence:** degradation records in range are listed; the
  event summary reports per-kind counts and first/last observation; a
  rotated or truncated log yields an explicit possibly-incomplete coverage
  statement.
- **AC7 Read-only:** `metadata.sqlite` and the workspace tree are
  byte-identical before and after a timeline invocation, including with a
  pending degradation marker present (no enforcement is triggered).
- **AC8 Invalid input:** `since >= until`, malformed timestamps, and
  impossible dates exit 2 with a diagnostic and mutate nothing.
- **AC9 Restart/persistence:** a timeline built from a catalog reopened in
  a new process equals one built in-process (persistence via the real
  SQLite catalog, not memory).
- **AC10 No mutation via CLI:** the CLI timeline command on a real
  workspace with captured operations returns exit 0 and prints the
  recorded entries (end-to-end through `main` argument parsing).
