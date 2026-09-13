# Phase 1 Implementation Report

Status: HISTORICAL — superseded by Phase 1.1.

> This report describes the Phase 1 implementation as it stood at the
> independent verification, which found it **NOT VERIFIED** (see
> `.ai/PHASE_1_INDEPENDENT_VERIFICATION.md`, findings V-F01 through V-F14).
> Phase 1.1 repaired the findings documented in
> `.ai/PHASE_1_1_REPAIR_REPORT.md`. The body below is retained verbatim as
> the historical record of the pre-repair state; its claims are not evidence
> of current correctness.

Status at verification: INCOMPLETE — foundation implementation exists, but
the full Phase 1 completion gate remained open pending the complete
38-scenario matrix and native cross-platform verification.

## 1. Executive summary

A minimal Rust foundation now exists for the frozen Phase 1 contract. It has an
external workspace store, SQLite metadata catalog, typed deterministic
manifests, BLAKE3 CAS ingestion, strong `rewind run` capture, passive boundary
commands, degradation and reconciliation gates, snapshots, single-operation
undo/redo, same-filesystem transaction staging, journal recovery, path
confinement checks, diagnostics, and a thin CLI.

The available Windows GNU target passes the current real-filesystem suite,
including 13 integration/adversarial tests after the crash-recovery test was
added. This is evidence for the implemented paths, not evidence that every
platform capability or every required crash boundary is complete. The MSVC
linker is unavailable in this environment, and no Linux/macOS runtime suite
has been executed here.

## 2. Six blockers from the verifier

The implementation follows the six Phase 0.7 corrections:

- A failed capture creates `CAPTURE_FAILED`, opens an `UNKNOWN_INTERVAL`, and
  enters `RECONCILIATION_REQUIRED`. A later passive boundary is recorded as an
  untrusted boundary and cannot use the stale baseline for attribution.
- Filesystem state is a typed fingerprint with explicit `ABSENT`, regular
  file, directory, symlink, and unsupported variants.
- Critical rollback movement uses transaction-local staging beside the
  workspace. The external store is only a persistent journal/CAS/anchor store
  and a post-commit archive destination.
- Journal steps record expected before and after maps plus backup and staging
  paths. Recovery checks physical source, destination, backup, and prepared
  artifacts.
- Windows and POSIX flushing, replacement, locking, reparse, and directory
  behavior are kept platform-specific; Windows sync is explicitly
  best-effort where the API does not expose a POSIX directory equivalent.
- Documentation now uses conditional guarantees and names storage, OS,
  object-support, and external-process assumptions.

## 3. Root cause of each blocker

The original design treated the baseline pointer as proof of the live
filesystem after a failed scan, treated every object as hashable file content,
treated external trash as universally atomic, reasoned about destination state
without source/backup evidence, collapsed POSIX and Windows operations into one
abstraction, and used absolute crash-safety language. These were corrected in
the Phase 0.7 architecture and enforced in the foundation code.

## 4. Architectural changes

Implemented module boundaries:

```text
src/model.rs      typed fingerprints, manifests, conditions, operations, journals
src/paths.rs      canonical roots, pointer, confinement, local staging
src/cas.rs        streaming BLAKE3 CAS with temp-object cleanup
src/scan.rs       deterministic no-follow scanner and manifest diffs
src/db.rs         SQLite catalog and unknown/bypass/transaction records
src/journal.rs    durable external JSON journal store
src/workspace.rs init, storage, lease, capture, reconciliation, snapshots
src/rollback.rs  anchors, step planning, quarantine, undo/redo, recovery
src/cli.rs       Phase 1 command surface and fail-open hook boundary
```

The CLI is intentionally thin; filesystem safety decisions remain in the
workspace, scan, CAS, and rollback modules. No DAG, TUI, watcher daemon,
time-range feature, merge engine, package recipe, or integration feature was
added.

## 5. Final workspace state machine

```text
initial full scan + durable S0 -> HEALTHY

HEALTHY -- complete capture ----------------------> HEALTHY at Sn+1
HEALTHY -- capture/storage failure ---------------> DEGRADED
DEGRADED -- durable gap/bypass marker ------------> RECONCILIATION_REQUIRED
DEGRADED or RECONCILIATION_REQUIRED
          -- full reconciliation succeeds -------> HEALTHY at Sx
RECONCILIATION_REQUIRED -- scan failure ----------> DEGRADED/
                                                     RECONCILIATION_REQUIRED
any state -- ambiguous rollback physical state ---> RECOVERY_REQUIRED
```

`RECOVERY_REQUIRED` is a transaction lockout, not a capture confidence level.
It blocks mutation until the journal is classified or the user resolves the
condition.

## 6. Final baseline lifecycle

Initialization creates S0 only after a complete scan and CAS ingestion. A
strong operation captures Sx before execution, executes inside the selected
process boundary, captures Sy after execution, verifies the manifest and
artifacts, writes the operation/effects, and only then advances the catalog
baseline pointer.

The pointer is a metadata pointer, not a filesystem commit. A capture failure
leaves the old pointer as historical context but marks it stale for
attribution. The implementation never treats it as a trusted pre-state while
the reconciliation gate is present. Reconciliation writes a checkpoint and
advances trust without creating a guessed operation.

## 7. Degraded/reconciliation state machine

```text
trusted S1
   |
   | command or passive boundary; capture fails or is bypassed
   v
CAPTURE_FAILED + UNKNOWN_INTERVAL + RECONCILIATION_REQUIRED
   |
   | later passive boundaries -> BOUNDARY_ONLY / UNTRUSTED records
   |
   | rewind run or explicit rewind reconcile
   v
full current scan + artifact verification
   | failure -> remain gated
   v
trusted reconciliation checkpoint Sx -> HEALTHY
```

`rewind run` calls reconciliation before executing when gated and refuses to
execute if the scan cannot establish a trusted current state. A passive hook
that cannot obtain the writer lease writes a durable bypass marker; the next
writer converts that marker into the same reconciliation gate.

## 8. Unknown interval semantics

The interval is recorded as `last_trusted_state -> unknown -> reconciliation`
with a reason and no fabricated command edges. `list` exposes failed and
untrusted boundaries. Historical state diffs remain inspectable, but the gap
itself has no causal diff. Undo and redo are unavailable for records that
would cross or depend on the gap. Previous snapshots remain immutable. New
normal tracking starts only at the reconciliation checkpoint.

## 9. Filesystem state fingerprint model

The manifest is a canonical JSON representation of a sorted `BTreeMap` of
workspace-relative paths:

```text
ABSENT
REGULAR_FILE { content_hash, size, metadata }
DIRECTORY    { manifest_hash, entry_count, metadata }
SYMLINK      { literal_target, target_hash, metadata }
UNSUPPORTED_OBJECT { object_kind, descriptor }
```

Directory hashes are hashes of canonical child fingerprint maps, not directory
bytes. Symlinks are inspected with link metadata and are not followed.
Missing paths are represented by `ABSENT` during expected-state comparison.
Windows reparse points that are not positively identified as supported file
symlinks are classified as unsupported and are not traversed.

Tracked metadata is the executable/permission mode where POSIX exposes it and
the read-only bit where the platform exposes it. Timestamps, ownership, ACLs,
xattrs, alternate data streams, sparse layout, and hardlink topology are
partial or unsupported according to the locked matrix. Rollback does not claim
to restore metadata that is not in the fingerprint.

## 10. Final quarantine architecture

Persistent metadata, CAS, journal, anchors, and archive data live under the
external store, for example `~/.rewind/projects/<workspace-id>/`. Critical
rollback staging is a private sibling of the canonical workspace root:

```text
workspace volume
  workspace/
  .rewind-txn/<transaction-id>/{prepared,quarantine}/

external store (possibly another volume)
  projects/<workspace-id>/{metadata.sqlite,journals,anchors,archive}/
```

Live entries are moved only within the affected filesystem. After COMMITTED,
local quarantine entries are copied, verified, and marked archived externally.
An archive failure is represented as pending/failed archive state and does not
turn a committed local rollback into a false rollback failure.

## 11. Filesystem rollback transaction protocol

Each transaction performs:

```text
anchor and preflight
-> PLANNED journal with complete state maps
-> PREPARED CAS artifacts and staging
-> APPLYING per-step source verification
-> same-filesystem quarantine/replacement
-> APPLIED physical recheck
-> platform flush requests and DURABLE journal
-> COMMITTED catalog/baseline update
-> best-effort post-commit external archive
```

Directory-to-directory child changes preserve the parent directory and journal
child steps separately. Type replacements quarantine the old object before
installing the new type. Every supported regular-file target is materialized
from a verified CAS object; CAS never uses hardlinks for immutability.

## 12. Crash recovery state machine

```text
PLANNED -> PREPARED -> APPLYING -> APPLIED -> DURABLE -> COMMITTED
                         |             |
                         +-----------> RECOVERY_REQUIRED
```

Recovery treats the external filesystem journal as authoritative for physical
classification and SQLite as an index. It recognizes known BEFORE and AFTER
states, and can complete the specifically recognized partial case where the
expected source is verified in local quarantine and the desired artifact is
ready. Unexpected source, destination, backup, or staging states become
`RECOVERY_REQUIRED`; the recovery path does not infer completion from a row.

## 13. Windows transaction model

The implementation uses Windows namespace operations and explicit file-type
checks rather than claiming POSIX semantics. Replacement can be blocked by
open handles or sharing modes. Directory replacement and unclassified reparse
points are refusal cases. Symlink creation is capability-dependent. File
flushes are requested where possible; Windows does not provide a POSIX-style
directory-entry transaction, so durability remains conditional. Cross-volume
movement is never the critical rollback movement.

The available verification target is `x86_64-pc-windows-gnu`. Native MSVC
linking could not be run because `link.exe`/Visual C++ tools are not installed.

## 14. POSIX transaction model

The design scopes POSIX behavior to same-filesystem rename-family operations,
descriptor/component confinement, file flushes, and accepted directory fsync
requests. Linux-specific `renameat2`/`openat2` enhancements are not assumed on
macOS. macOS/APFS clone behavior and `F_FULLFSYNC` are capability-specific.
Cross-device movement is not treated as atomic; a future explicit copy
protocol would be a separate capability, not an implicit rename fallback.

## 15. Platform capability matrix

| Capability | Linux | macOS | Windows |
|---|---|---|---|
| Atomic regular-file replacement | SUPPORTED, same filesystem | SUPPORTED, same volume | SUPPORTED for eligible sharing modes |
| Same-volume rename | SUPPORTED; `EXDEV` across devices | SUPPORTED; not cross-volume | SUPPORTED namespace operation; not multi-entry transaction |
| Directory fsync | SUPPORTED where accepted | PARTIAL/BEST-EFFORT | PARTIAL/BEST-EFFORT |
| CoW cloning | PARTIAL, filesystem dependent | PARTIAL, APFS dependent | PARTIAL, ReFS capability dependent |
| Descriptor-relative confinement | SUPPORTED on available Linux APIs | PARTIAL, component checks | PARTIAL, handles/reparse checks |
| Symlink/reparse protection | SUPPORTED for no-follow paths | PARTIAL/API-scoped | PARTIAL; junction/reparse refusal |
| File locking | Mostly advisory | Mostly advisory | Sharing modes can deny replacement |
| Crash durability semantics | Conditional after flush/device behavior | API/filesystem-specific | API/filesystem-specific |

## 16. Updated guarantees

Under a supported local filesystem, supported object types, correct OS
semantics, accessible storage, durable CAS/journal writes, no external
filesystem corruption, and the single-writer lease, Phase 1 can:

- capture a strong `rewind run` transition when pre-scan, execution boundary,
  post-scan, artifact verification, and metadata publication all succeed;
- refuse unsafe undo/redo on conflicts, unsupported states, missing artifacts,
  or unknown journal physical states;
- restore captured state without rerunning the original command;
- classify supported interrupted rollback states without silently assuming that
  an unverified mutation completed; and
- keep cross-device external archival outside the critical rollback commit.

## 17. Explicit limitations

This implementation does not protect against defective hardware, filesystem or
kernel bugs, power-loss behavior outside requested flush semantics, external
writers racing at an unprotected instant, remote/system side effects, ACL/xattr
exact restoration, unsupported objects, hardlink/sparse topology changes, or
passive background writes that occur outside the observed boundary. It does
not follow moved workspaces automatically. Native MSVC, Linux, macOS, and the
full crash-injection matrix remain unverified in this environment.

## 18. Updated adversarial test matrix

The following is the required matrix. The first 13 cases are implemented as
real tests in `tests/foundation.rs`; the remaining cases are explicit pending
verification items rather than claimed passes.

| # | Setup and action | Expected physical state | Expected journal/state | Expected CLI result |
|---:|---|---|---|---|
| 1 | Run modifies file; post-scan timeout | File may differ from baseline | `CAPTURE_FAILED`, gap open | nonzero capture diagnostic |
| 2 | Modify same file after #1 | New bytes remain live | no normal operation edge | passive boundary only |
| 3 | Reconcile after #2 | checkpoint matches live bytes | gap closed, `HEALTHY` | reconcile succeeds |
| 4 | Multiple commands while gated | all live changes retained | boundary-only/untrusted | undo refused |
| 5 | File -> ABSENT | missing path | `ABSENT` fingerprint | state capture succeeds |
| 6 | ABSENT -> file | new regular file | CAS object + file state | state capture succeeds |
| 7 | File -> file | new bytes | two hashes | capture/undo eligible |
| 8 | File -> directory | directory exists | type-change maps | restore or refusal by support |
| 9 | Directory -> file | regular file exists | type-change maps | restore or refusal by support |
| 10 | Symlink -> file | link not followed | literal link state then file | capability result |
| 11 | File -> symlink | literal link installed | link target recorded | capability result |
| 12 | Workspace/store on different volumes | rollback movement stays local | archive pending/archived | no cross-device rename claim |
| 13 | Undo creation | created path absent | committed step with no source | undo succeeds |
| 14 | Undo deletion | deleted object restored | source/destination verified | undo succeeds |
| 15 | Crash before quarantine move | source still before | `PLANNED`/`PREPARED` | recovery resumes |
| 16 | Crash after quarantine move | source absent, backup verified | `APPLYING` partial | recovery completes known plan |
| 17 | Crash after install | destination after, backup present | `APPLIED`/`DURABLE` | recovery commits |
| 18 | Crash before journal advancement | physical map decides | old journal state | recovery inspects, not guesses |
| 19 | Crash after journal advancement | physical map rechecked | advanced journal | recovery idempotent |
| 20 | Source/destination inconsistent | unexpected object remains | `RECOVERY_REQUIRED` | refuse mutation |
| 21 | Unknown physical state | no safe inference | recovery lockout | diagnostic/manual action |
| 22 | File type replacement | old object quarantined | per-step typed maps | deterministic restore |
| 23 | Locked Windows file | bytes remain untouched | conflict/failure | useful refusal |
| 24 | Windows cross-volume path | no critical cross-volume move | refusal/archive path | no false atomicity |
| 25 | Windows reparse point | no traversal | unsupported classification | refuse affected operation |
| 26 | Windows junction | target not traversed | unsupported classification | refuse affected operation |
| 27 | Case-insensitive collision | no ambiguous manifest | scan failure | refuse and diagnose |
| 28 | Second CLI during rollback | writer lease held | no concurrent mutation | lock refusal |
| 29 | Passive hook during rollback | hook fails open | bypass marker durable | shell remains usable |
| 30 | IDE edit before step | live bytes differ | conflict/recovery state | no overwrite |
| 31 | External process edit | live bytes differ | conflict/recovery state | no overwrite |
| 32 | Symlink swap during path access | outside target untouched | confinement failure | refuse |
| 33 | Corrupt CAS object | artifact unreadable | verification failure | rollback/capture refused |
| 34 | Interrupted CAS write | partial temp removed | no published blob | next startup safe |
| 35 | Missing CAS artifact | expected content unavailable | operation unavailable | refusal |
| 36 | Directory-child mutation | parent preserved | child steps only | undo/redo succeeds |
| 37 | Redo after external edit | post-state not assumed | conflict | redo refused |
| 38 | Crash during redo then restart | physical map classified | commit or recovery lockout | no command re-execution |

## 19. Tests executed and exact verification commands

The final verification commands were:

```text
cargo +stable-x86_64-pc-windows-gnu fmt --all -- --check
cargo +stable-x86_64-pc-windows-gnu check --target x86_64-pc-windows-gnu --all-targets
cargo +stable-x86_64-pc-windows-gnu clippy --target x86_64-pc-windows-gnu --all-targets -- -D warnings
cargo +stable-x86_64-pc-windows-gnu test --target x86_64-pc-windows-gnu --all-targets
cargo +stable-x86_64-pc-windows-gnu run --target x86_64-pc-windows-gnu -- --help
```

Formatting, build, Clippy, and 13 integration tests passed. The CLI help
command rendered the documented Phase 1 command surface. Native MSVC could not
link because the Visual C++ linker is unavailable.

## 20. Documentation consistency audit

The following were synchronized with the frozen contract and current status:

- `.ai/PROJECT_CONTEXT.md`, `ARCHITECTURE_PROPOSAL.md`, `MVP_SPEC.md`,
  `TECHNICAL_FEASIBILITY.md`, `ROADMAP.md`, and `HANDOFF.md`;
- `phases/phase-01-foundation.md` status and implementation authority;
- `.ai/CURRENT_STATE.md`, `TODO.md`, `TEST_STATUS.md`, `CHANGELOG.md`, and
  `AGENT_LOG.md`;
- the Phase 0.7 report remains historical and is not treated as an assertion
  that the current repository has no implementation.

Searches found no active implementation path that treats SQLite as a
filesystem transaction, treats a stale baseline as a trusted new pre-state,
uses external trash as critical atomic movement, or claims universal POSIX and
Windows equivalence. Historical Phase 0 reports retain their historical
wording by design.

## 21. Hostile sequence review

For the required sequence `S0 foo=A; command 1 foo=B; post-scan fails; command
2 foo=C; command 3 delete; command 4 recreate D; rollback crash; restart;
redo crash; move workspace; IDE edit; symlink attack`, the implementation gives
the following result:

| Point | Trusted state | Live state | Rewind knowledge and decision |
|---|---|---|---|
| After command 1 failure | S0 historical only | B or partially changed | Gap is open; no normal operation is created |
| Command 2 while gated | S0 historical only | C | Boundary-only/untrusted; no `S0 -> C` attribution |
| Reconciliation | new Sx | C | Full scan ingests C and makes Sx trusted; gap remains non-causal |
| Command 3 delete | Sx | ABSENT | Strong operation Sx -> S3, if command is supervised |
| Command 4 recreate D | S3 | D | Strong operation S3 -> S4, if command is supervised |
| Undo crash after quarantine | S4 until recovery | D in local quarantine, live path ABSENT | Journal recognizes verified partial quarantine and completes target S3 |
| Restart/recovery | S3 after verified recovery | ABSENT | Operation is UNDONE; no command is rerun |
| Redo crash before/after install | S3 until recovery | ABSENT or verified D | Recovery applies or accepts the recorded D artifact and commits S4 |
| Workspace moved to another volume | unavailable | moved path | Pointer/catalog canonical-root check refuses automatic reattachment |
| IDE edits before rollback | current state no longer expected | external bytes | Preflight conflict refuses; force requires quarantine of live bytes |
| Symlink/reparse attack | healthy only if no gate | link in leaf or parent | Leaf is typed SYMLINK; parent traversal/reparse is refused; no outside target is followed |

No step requires guessing a command boundary or treating a stale state as
current. The moved-workspace case cannot safely continue in this Phase 1
implementation and is an explicit refusal, as required by the contract.

## 22. Remaining uncertainties

The major open verification items are native toolchain coverage, exact
platform-specific replacement/flush behavior, handle-sharing failures on
Windows, Linux/macOS descriptor-relative hardening, injected crashes at every
journal write boundary, external archive failure/retry, and full passive-hook
integration in real shells. These are verification and capability questions;
they must not be silently converted into stronger guarantees.

## 23. Final recommendation

Do not begin Phase 2. The Phase 1 foundation is suitable for independent
verification of the implemented Windows GNU paths, but the Phase 1 completion
gate should remain open until the pending matrix, crash-boundary tests, and
native platform runs are completed. The implementation should be accepted as
`PHASE 1 INCOMPLETE` rather than overstating its evidence.
