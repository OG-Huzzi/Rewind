# Rewind

Rewind is a local-first workspace state and recovery tool that records command boundaries and filesystem states inside an explicitly initialized directory so captured operations can be inspected and inverted.

## Status

Phase 3 (continuous observation) is implemented and locally verified
(all gates green, 119 tests passing; CI confirmation pending push): the
new advisory watcher observes filesystem activity between commands, and
`.ai/PHASE_3_VERIFICATION_REPORT.md` carries the evidence. Phases 1
(foundation through 1.4) and 2 (dependency-aware inspection) are complete
and CI-verified on GitHub Actions for Ubuntu, macOS, and Windows (MSVC).

## What it does today

- `rewind init` establishes a workspace with an external state store
  (catalog, content-addressed storage, journals) outside the workspace.
- `rewind run -- <command>` reconciles first when required, then captures
  the command's before/after filesystem states as a strongly attributed,
  undoable operation.
- Passive shell hooks record lighter, lower-confidence boundaries; undo
  covers only strongly captured operations.
- `rewind watch start/status/stop` runs an optional advisory watcher that
  records which paths *may have changed* between commands. It is never
  authoritative: any watcher failure (overflow, crash gap, offline
  interval) degrades durably and is resolved by the existing
  reconciliation, never by the watcher itself.
- `rewind undo` / `redo` invert or reapply captured operations through
  journaled, quarantine-based rollback with conflict refusal.
- `rewind snapshot` / `restore` manage explicit named states through the
  same anchored rollback protocol.
- `rewind status` / `doctor` / `list` / `show` / `diff` expose condition,
  integrity, and history, including unknown intervals that are never
  silently converted into guessed operations.

## Build and develop

Requires Rust stable. SQLite is compiled in via `rusqlite` (bundled), so
no system packages are needed.

```sh
cargo build
cargo test
```

Install the passive hook for your shell (adjust the path):

```sh
# ~/.bashrc
source /path/to/rewind/integration/rewind.bash
# ~/.zshrc
source /path/to/rewind/integration/rewind.zsh
```

The hooks fail open: shell commands run exactly as they would without
Rewind, and bookkeeping never blocks the prompt.

## CLI examples

```sh
rewind init .
rewind run -- cargo build
rewind list
rewind undo          # invert the most recent eligible operation
rewind redo          # reapply it
rewind snapshot pre-experiment
rewind restore pre-experiment
rewind watch start   # optional advisory watcher (background, per workspace)
rewind watch status  # lifecycle, coverage, pending degradations, dirty paths
rewind watch stop
rewind status
rewind doctor
```

## Platform support

- Windows 10/11 (NTFS): reparse points other than symlinks (junctions)
  are refused as unsupported; symlink restoration uses recorded target
  kinds.
- Linux and macOS: full support of the Phase 1 feature set.
- CI: ubuntu-latest, macos-latest, windows-latest (MSVC).

## Current limitations

- Undo covers only operations captured by `rewind run` (or restorable
  snapshots); passive observations are never fabricated into operations.
- Watcher evidence is advisory only: it names paths that may have changed
  and can never attribute a change to a command or prove a state complete.
- Large rollbacks rescan the workspace per step; performance redesign is
  deferred to Phase 2.
- Unsupported objects (junctions, sockets, and similar) are recorded as
  unsupported and block rollback that would need to recreate them.
- Power-loss durability is designed for (journals, fsync) but cannot be
  fully asserted by CI; crash-injection tests are the available evidence.
