# Rewind

Rewind records filesystem states around commands run inside an explicitly
initialized workspace, so supported, known local changes can be inspected and
conditionally undone or redone.

> **MIT License — see [LICENSE](LICENSE).** SPDX identifier: `MIT`.

## What Rewind records

- `rewind run -- <command>` scans the workspace before and after a command.
  When the scans, storage, and filesystem objects meet the contract, Rewind
  records the local filesystem state transition. It does not infer remote,
  system-wide, or other effects that are outside the workspace.
- `rewind undo` and `rewind redo` apply recorded states through the journaled
  rollback path. They can refuse on conflicts, unsupported objects, unknown
  state, or an unsafe recovery condition. Redo reapplies recorded state; it
  never reruns the command.
- Optional shell hooks record lower-confidence command boundaries and passive
  observations. These observations are not strongly captured, undoable
  operations.
- A failed or incomplete observation degrades trust and can open an unknown
  interval. Attribution, undo, and redo stay gated until reconciliation closes
  the interval with a complete scan. Rewind refuses to guess missing state.
- A single-writer lease serializes mutations. Rollback stages replaced entries
  in local quarantine; the external store keeps journals and evidence and may
  hold post-commit archives. Recovery follows the journal and available state
  evidence; it does not promise recovery from every environmental failure.
- The optional watcher is advisory. It can record paths that may have changed,
  but it cannot attribute a change to a command or prove a complete state.
- Strongly captured commands may include manifest/lockfile evidence for the
  exact probed filename set. That evidence states recorded paths and hashes;
  it does not parse package versions or reverse package-manager side effects.

The guarantees are conditional on supported local filesystem objects,
available storage, complete scans, and the safety gates in the contracts.
Rewind is not a kernel monitor, version-control replacement, remote-side-effect
reverser, or system-wide transaction manager.

Evidence: the [foundation contract](https://github.com/OG-Huzzi/Rewind/blob/main/phases/phase-01-foundation.md)
and [conditional-safety report](https://github.com/OG-Huzzi/Rewind/blob/main/.ai/PHASE_0_7_ARCHITECTURE_FINALIZATION_REPORT.md)
define the guarantees. Command capture, undo, redo, quarantine, and recovery
are exercised by the [foundation](https://github.com/OG-Huzzi/Rewind/blob/main/tests/foundation.rs),
[hardening](https://github.com/OG-Huzzi/Rewind/blob/main/tests/hardening.rs),
and [rollback](https://github.com/OG-Huzzi/Rewind/blob/main/tests/rollback_tree.rs)
tests. Passive boundaries and watcher limits are specified by the
[boundary report](https://github.com/OG-Huzzi/Rewind/blob/main/.ai/PHASE_1_4_BOUNDARY_CORRELATION_REPORT.md)
and [watcher contract](https://github.com/OG-Huzzi/Rewind/blob/main/.ai/PHASE_3_CONTINUOUS_OBSERVATION.md),
with coverage in the [boundary](https://github.com/OG-Huzzi/Rewind/blob/main/tests/boundary_correlation.rs)
and [watcher](https://github.com/OG-Huzzi/Rewind/blob/main/tests/phase3_watcher.rs)
tests. Package evidence is specified in the [Phase 6 contract](https://github.com/OG-Huzzi/Rewind/blob/main/.ai/PHASE_6_RECIPES.md)
and tested in [phase6_recipes](https://github.com/OG-Huzzi/Rewind/blob/main/tests/phase6_recipes.rs).

## Platform support

“Supported” means the object is captured, restored, and verified on the
platform that produces it, subject to the conditions and refusal gates above.
The matrix matches the [Phase 5 verification report](https://github.com/OG-Huzzi/Rewind/blob/main/.ai/PHASE_5_VERIFICATION_REPORT.md)
and [platform contract](https://github.com/OG-Huzzi/Rewind/blob/main/.ai/PHASE_5_PLATFORM_EXPANSION.md).

| Object | Linux | macOS | Windows | Conditions and refusals |
|---|---|---|---|---|
| Regular file | Supported | Supported | Supported | Content is stored in the CAS; POSIX permission modes are restored on Linux/macOS. Named NTFS streams and explicit DACL ACEs plus the protected flag are captured/restored on Windows regular files. |
| Directory | Supported | Supported | Supported | |
| Symlink with inside-root target | Supported | Supported | Supported | Target must be creatable; Windows target kind comes from reparse data. |
| Symlink with escaping or unknown target | Recorded; restore refused | Recorded; restore refused | Recorded; restore refused | Target is never followed. |
| Windows junction | N/A | N/A | Supported | Literal reparse data and readonly state; target is never followed. |
| Other reparse point | N/A | N/A | Restore refused | Tag value is named when readable. |
| POSIX named pipe (FIFO) | Supported | Supported | Restore refused | Existence and mode only; FIFO content is not captured. |
| Unix domain socket | Restore refused | Restore refused | N/A | Named `UNIX_SOCKET` descriptor. |
| Character or block device | Restore refused | Restore refused | N/A | Named unsupported-object descriptor. |
| Non-Unicode filename | Scan refused | Scan refused | Scan refused | Names are never guessed. |

Foreign platform fingerprints are refused at materialization and never treated
as matching local artifacts. NULL DACLs and non-simple DACL ACE types produce
named scan errors rather than approximations.

Deferred metadata and object classes include directory-attached NTFS streams
and directory DACLs; POSIX xattrs (no POSIX probe host was available); uid/gid,
Windows owner/group, and SACL state (privilege-bound); sparse-file layout; and
macOS `chflags` (no macOS probe host). Package-transaction semantics and
range-based restore remain excluded. The blockers and scope are recorded in
the [Phase 5 report](https://github.com/OG-Huzzi/Rewind/blob/main/.ai/PHASE_5_VERIFICATION_REPORT.md),
[DACL contract](https://github.com/OG-Huzzi/Rewind/blob/main/.ai/PHASE_5_NTFS_DACL.md),
and [ADR-016](https://github.com/OG-Huzzi/Rewind/blob/main/.ai/DECISIONS.md#adr-016-package-specific-recipes-are-deferred).

## Install

### From a source clone

This command requires Rust 1.98.1 or newer and the platform's native build
toolchain. SQLite is built through the bundled `rusqlite` feature.

```sh
git clone https://github.com/OG-Huzzi/Rewind.git
cd Rewind
cargo install --path . --locked
```

The source-install path was smoke-tested from a clean temporary Cargo install
root. On Windows, the local validation used the installed GNU Rust toolchain;
GitHub CI also builds the Windows MSVC target.

### Tagged release binaries

The [Rewind v0.1.0 release](https://github.com/OG-Huzzi/Rewind/releases/tag/v0.1.0)
provides these platform builds:

- Windows x86_64 MSVC: [rewind-v0.1.0-x86_64-pc-windows-msvc.zip](https://github.com/OG-Huzzi/Rewind/releases/download/v0.1.0/rewind-v0.1.0-x86_64-pc-windows-msvc.zip)
- macOS ARM64: [rewind-v0.1.0-aarch64-apple-darwin.tar.gz](https://github.com/OG-Huzzi/Rewind/releases/download/v0.1.0/rewind-v0.1.0-aarch64-apple-darwin.tar.gz)
- Linux x86_64 GNU: [rewind-v0.1.0-x86_64-unknown-linux-gnu.tar.gz](https://github.com/OG-Huzzi/Rewind/releases/download/v0.1.0/rewind-v0.1.0-x86_64-unknown-linux-gnu.tar.gz)

Choose the archive whose Rust target matches your platform. Each contains
the binary and an `INSTALL.txt` with extraction and PATH instructions.

## Quick start

Start in an empty directory with `rewind` on your PATH. This Windows
PowerShell sequence was run against the installed binary on the validation
host; a fresh workspace assigns operation 1 to the first captured command.

```powershell
New-Item -ItemType Directory rewind-demo
Set-Location rewind-demo
rewind init .
rewind run -- cmd /c 'echo captured>hello.txt'
rewind status
rewind undo
rewind redo
rewind list
rewind show 1
rewind recover
```

`undo` removes `hello.txt`; `redo` restores the recorded file state. Use
`rewind --help` and `rewind <command> --help` for the current command syntax,
including `inspect`, `plan`, `apply`, and the advisory `watch` commands.

## License

MIT License — see [LICENSE](LICENSE). SPDX identifier: `MIT`.
Crates.io publication remains deferred until the owner provides a publishing
token.
