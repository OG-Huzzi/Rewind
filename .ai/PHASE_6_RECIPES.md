# Phase 6 Contract — Ecosystem Recipes: Manifest/Lockfile Evidence Layer

Status: **IMPLEMENTED** (commits `94adeaa` + `8002933` on `main`; local
gates green, full suite 187 passed / 0 failed twice consecutively; CI
verification recorded in TEST_STATUS.md once observed). The
pre-implementation contract text below is preserved; only this status line
and §8's recorded results were added.

Baseline: `main` at `77567e9` (Phase 5 closed, close-out/CI stabilization
landed; CI green on ubuntu/macOS/windows — check-runs for `77567e9`
verified green before any change; local suite 166 passed / 0 failed).

## 0. Baseline verification and docs-vs-code reconciliation

- Verified: branch `main`, clean tree, `HEAD == origin/main == 77567e9`;
  push access confirmed by a dry-run; the public GitHub API exposes
  check-runs for this repository (all three platforms green on the
  baseline commit).
- **Discrepancy found and reconciled:** there is **no schema-migration
  mechanism** in the repository. Phase 2's contract already recorded this
  ("no schema-migration mechanism"), and Phase 4's `WorkspaceRow.created_at`
  was *not* an additive migration — the column has existed since the
  initial schema. The only precedent for extending an *existing* catalog is
  the Phase 1.4 idempotent `CREATE ... IF NOT EXISTS` DDL (the
  `passive_boundary_consume_once` trigger) executed in
  `Catalog::initialize()` on every open. This phase introduces the first
  schema addition for pre-existing catalogs and follows exactly that
  established pattern: a new `CREATE TABLE IF NOT EXISTS` statement;
  **no `ALTER TABLE` anywhere**.
- All other audited docs matched the code (strong-capture path, capture
  failure path, catalog schema, fingerprints, CLI, timeline, tests).

## 1. Selected scope

The deferral in ADR-016 / `.ai/PHASE_4_TIME_AND_ECOSYSTEM.md` §6 names the
unlock condition: a recipe is worth building only when it adds **provable
evidence**, exemplified as "recording a package manifest fingerprint as
part of strong capture". This phase delivers exactly that — a
**manifest/lockfile evidence layer** — and nothing more.

- **Recognized filename sets** (exact, workspace-root-relative, final):

  | Recipe kind | Lockfile path(s) — trigger on change | Manifest paths — recorded when present |
  |---|---|---|
  | `cargo` | `Cargo.lock` | `Cargo.toml` |
  | `npm` | `package-lock.json`, `npm-shrinkwrap.json` | `package.json` |
  | `pnpm` | `pnpm-lock.yaml` | `package.json` |
  | `go` | `go.sum` | `go.mod` |
  | `uv` | `uv.lock` | `pyproject.toml` |
  | `pip` | `requirements.txt` | `pyproject.toml`, `setup.py` |

  Every entry is justified by a *probed* real operation on the working
  host (§3). Ecosystems without probe evidence (yarn, bun, poetry,
  Pipfile, Gemfile, composer, …) are **not** recognized this phase —
  deferred until a probe host exists, the same no-guess rule this project
  has applied to xattrs, chflags, and every other unprobeable capability.
- **Depth policy: root-only.** A recognized name must be a
  root-relative path with no `/`. Justification: the npm probe showed
  `npm install` creates a *nested* `node_modules/.package-lock.json` —
  a depth-unbounded rule would emit npm evidence for commands touching
  vendored trees, fabricating claims about nested packages' dependency
  state; monorepo sub-project lockfiles are likewise deferred (no probe
  evidence of sub-project layouts, and matching any depth cannot
  distinguish a project lockfile from a vendored one). Tradeoff
  documented: monorepo sub-project lockfile changes produce no evidence
  (they remain ordinary recorded file changes).
- **Case sensitivity: exact, case-sensitive byte equality on every
  platform.** The recorded name must equal the documented name exactly.
  NTFS/APFS preserve creation casing, so the same on-disk name produces
  the same evidence on every platform; a deviating casing (`cargo.lock`,
  `CARGO.LOCK`) is recognized **nowhere** — never guessed differently per
  platform. The scanner's existing case-collision refusal already rejects
  stores where two names differ only by case, so no ambiguity exists.
- **Supported platforms:** all three (Linux, macOS, Windows). Recognition
  is a pure function of recorded manifests and is platform-independent by
  construction; the differential case test runs on all three CI platforms.

## 2. The fidelity gap closed and the honesty boundary

**Before:** a captured `npm install` / `cargo add` was visible only as
generic file effects (`package-lock.json` Create/Modify, …). The operation
record could not state, in its own evidence, that the *dependency
resolution state* changed, or between which content-addressed states.

**After:** each captured operation carries derived, provable lockfile
evidence. The entire claim is:

> "In this captured command, lockfile `X` (recipe kind `M`) changed from
> CAS hash `A` (or `absent`) to CAS hash `B` (or `absent`); these
> recognized manifest paths were present: `[…]`."

**Honesty boundary (enforced by the data model, not by prose):** the
evidence contains only paths and already-recorded CAS ids. It never
parses lockfile contents, so it can never claim *which* packages or
versions changed; it never speaks about registry state, global caches,
shared stores, or post-install scripts; it never claims a package action
is reversible as a package action. Undo/redo continue to apply recorded
filesystem state only (ADR-013), and this phase does not overturn
ADR-016's rejection of package-transaction semantics — it implements
ADR-016's stated unlock condition narrowly.

## 3. Probe evidence (executed on the working host)

Host: Windows 11 (build 26200), NTFS, `x86_64-pc-windows-gnu`.
Network: reachable (crates.io index, npm registry, proxy.golang.org,
pypi.org all answered); all probes below were executed online unless
marked offline. Offline-safe variants were probed anyway so the test
plan does not depend on network.

Toolchains: `cargo 1.98.1` / `rustc 1.98.1`; `npm 11.6.2` (node
v24.11.1); `pnpm 11.3.0`; `go 1.26.5 windows/amd64`; `python 3.14.4` /
`pip 26.2.1`; `uv 0.11.26`. Not installed: yarn, bun, poetry, pipenv.

Observed file effects (each re-observed inside a Rewind capture; every
lockfile appears as an ordinary `Fingerprint::RegularFile` with its CAS
`content_hash` in the post-scan manifest):

| Manager | Operation | Files created/modified |
|---|---|---|
| cargo | `cargo generate-lockfile` (online, with a dependency) | `Cargo.lock` (Create; captured as RegularFile) |
| cargo | `cargo generate-lockfile --offline` (no deps; run twice with a renamed package) | `Cargo.lock` only; deterministic bytes, two distinct hashes across runs |
| npm | `npm install is-number` | `package-lock.json` (Create), `package.json` (Modify), `node_modules/**` including a **nested `node_modules/.package-lock.json`** |
| npm | `npm install --package-lock-only --ignore-scripts` (cold cache + blackholed registry) | `package-lock.json` only, exit 0 — proves the offline-safe test variant |
| npm | `npm shrinkwrap` | renames `package-lock.json` → `npm-shrinkwrap.json` |
| pnpm | `pnpm install --lockfile-only --ignore-scripts` (online and `--offline`) | `pnpm-lock.yaml` |
| go | `go get github.com/google/uuid@latest` | `go.mod` (Modify), `go.sum` (Create) |
| go | `go mod tidy` with a local `replace` | no `go.sum` (sums exist only for fetched modules) → a real-manager go *evidence* test would require network |
| uv | `uv lock` (online, with a dep; and `--offline` no-deps) | `uv.lock` |
| pip | `pip install --target ./vendor -r requirements.txt --no-deps` | `vendor/**` only; `python -m pip freeze` writes stdout (no file) — `requirements.txt` is a pin record |

Offline strategy recorded: cargo/pnpm/uv have genuine offline modes
(probed); npm's lock-only variant works with a blackholed registry and a
cold cache when there is nothing to resolve; go's evidence-producing
operation cannot be made offline-safe locally.

## 4. Canonical representation

New module `src/recipes.rs` (pure) plus shared types in `src/model.rs`
next to `Effect`/`OperationRecord`:

```rust
pub enum RecipeKind { Cargo, Npm, Pnpm, Go, Uv, Pip }   // serde snake_case

pub struct RecipeEvidence {
    pub kind: RecipeKind,
    pub lockfile: String,          // exact root-relative path
    pub pre_hash: Option<String>,  // recorded CAS id, or None = absent
    pub post_hash: Option<String>, // recorded CAS id, or None = absent
    pub manifests: Vec<String>,    // recognized manifest paths present in either state
}
```

- **Derivation is a pure function** `derive_evidence(before: &Manifest,
  after: &Manifest) -> Vec<RecipeEvidence>`: for each documented lockfile
  name it reads `before.get(name)` / `after.get(name)` only — a handful of
  O(1) map lookups, independent of workspace size and tree shape. No
  filesystem access, no scans, no CAS reads/writes, no hashing: the hashes
  are the manifests' existing CAS ids (`RegularFile.content_hash`).
- **Trigger:** a lockfile entry exists when its recognized pre/post states
  differ. States are recognized only when each side is `Absent` or
  `RegularFile`; any other object kind at a lockfile name (directory,
  symlink, unsupported) suppresses the entry — a symlinked or replaced
  lockfile is never described with a guessed hash. Same hash on both
  sides (including a byte-identical rewrite) → **no entry** (explicit
  no-change policy). A changed manifest without a changed lockfile → no
  entry (a manifest edit is not dependency-resolution evidence).
- **Determinism:** entries are ordered by lockfile path (unique across
  kinds by construction); `manifests` follows the documented static order.
  Identical recorded manifests produce byte-identical evidence.
- **Serialization/compatibility:** `OperationRecord` (and therefore
  `show`, `inspect history --json`) gains
  `#[serde(default, skip_serializing_if = "Vec::is_empty")] pub evidence:
  Vec<RecipeEvidence>` — operations without evidence serialize
  byte-identically to the pre-phase form. The evidence blob is stored as
  JSON in a new `operation_evidence(operation_id INTEGER PRIMARY KEY
  REFERENCES operations(id), evidence_json TEXT NOT NULL)` table, created
  with `CREATE TABLE IF NOT EXISTS` in `Catalog::initialize()` (the
  established idempotent pattern; no `ALTER TABLE`). Rows are written in
  the same transaction as `insert_operation` and only when non-empty.
  Pre-phase catalogs gain the table on next open; their operation rows
  read as empty evidence. A non-NULL, unparseable evidence blob is an
  explicit read error (parity with `effects` JSON) — never silently
  emptied.
- **No state-identity change:** fingerprints, manifests, state ids, and
  `STATE_SCHEMA_VERSION` are untouched; no new CAS objects.

## 5. Capture-side integration point

- In `Workspace::run_command`, immediately after `diff_manifests` and
  before `insert_operation`: `let evidence = recipes::derive_evidence(
  &before, &post_scan.manifest);` and the draft carries it. The inputs are
  exactly the recorded pre-state manifest and the recorded post-scan
  manifest.
- **Failure behavior:** derivation cannot fail by construction — it is a
  pure function over in-memory data with no I/O and no fallible
  operations (no `Result`). It runs only on the strong-capture success
  path; `record_capture_failure` drafts carry no evidence (a failed
  capture has no trustworthy post-state), and passive-hook,
  reconciliation, and restore drafts carry none either — evidence exists
  only for strongly captured commands (`OperationKind::Strong`).
- **Side-effect-free invariant:** the derivation performs no filesystem,
  CAS, catalog, journal, or watcher interaction; it cannot alter state
  ids, manifests, effects, reversibility computation, writer
  serialization, or the capture-failure paths. It is a pure read of
  already-captured data.

## 6. Presentation rules

- `rewind list`: each operation line is byte-identical to before unless
  the operation carries evidence, in which case ` lockfiles=[A, B]` (the
  evidence's lockfile paths, in evidence order) is appended.
- `rewind show <id>`: pretty JSON of the operation; `evidence` appears
  only when non-empty (the array carries the full records with hashes).
- `rewind inspect timeline`: human OPERATION lines append the same
  ` lockfiles=[…]` suffix only when evidence is present; the JSON entry
  gains an `evidence` array only when non-empty. `summary` is unchanged.
  `TIMELINE_SCHEMA_VERSION` stays `1`: the field is optional, appears
  only on evidence-bearing stores, and a version bump would change output
  for evidence-free stores — which the unchanged-output guarantee
  forbids.
- **Unchanged-output guarantees:** stores without evidence render
  byte-identically to the pre-phase output on every surface (`list`
  lines, `show`/`inspect history` JSON — field skipped, timeline human +
  JSON — field skipped); the Phase 4 AC3 byte-identical-JSON determinism
  guarantee keeps holding, extended by a new evidence-bearing-store test.
- Presentation never mutates, leases, or changes exit codes; `diff` and
  `ui` history output are intentionally unchanged (evidence adds nothing
  to a path-level diff and is not part of the UI surface this phase).

## 7. Explicit exclusions

- No semantic restore, no re-running package managers, no package-action
  undo/redo; no lockfile-content parsing (no package names or versions
  are ever claimed).
- No registry, cache, or global/system-wide state; no network; no AI; no
  new dependencies.
- No generated-artifact classification or exclusion: `node_modules/`,
  `target/`, `vendor/` remain ordinary captured state.
- No watcher changes; no passive-boundary, recovery, journal, or
  quarantine changes; no timeline tier changes (evidence stays inside
  OPERATION entries).
- Not recognized this phase (deferred, no probe evidence): yarn, bun,
  poetry, Pipfile, Gemfile, composer, and every other ecosystem; nested
  or monorepo sub-project lockfiles (root-only policy); symlinked /
  non-regular lockfiles.

## 8. Acceptance criteria and test plan

- **AC1 Pure, bounded recognition:** `derive_evidence` reads only the
  two recorded manifests; its output hashes equal the recorded
  fingerprints' `content_hash` values; it performs no I/O, no hashing,
  no CAS work (unit tests, in-memory manifests).
- **AC2 Exact sets, root-only, no false positives:** nested
  `sub/Cargo.lock` / `node_modules/.package-lock.json`, similar names
  (`Cargo.lock.bak`, `package-lock.json.old`), unrecognized ecosystems,
  and non-regular lockfile objects produce no entries.
- **AC3 All three transitions:** two captured operations plus one
  deletion yield added (`None`→`Some`), changed (`Some`→`Some`,
  different), removed (`Some`→`None`), each with hashes verifying against
  the CAS; a byte-identical rewrite yields no entry.
- **AC4 Case policy:** canonical casing recognized on every platform;
  deviating casing recognized nowhere; runs on all three CI platforms.
- **AC5 Trigger boundary:** manifest-only changes, passive observations,
  reconciliation, restores, and capture failures carry no evidence.
- **AC6 Compatibility:** a pre-phase catalog (simulated by dropping the
  new table) opens, gains the table idempotently, and its legacy rows
  read/display byte-identically (field skipped); repeated initialize is
  idempotent; malformed evidence blobs error explicitly.
- **AC7 Presentation:** `list`/`show`/timeline render evidence when
  present; evidence-free stores are byte-identical to before on all
  surfaces; timeline JSON determinism holds for both evidence-bearing and
  evidence-free stores (two invocations, byte compare).
- **AC8 Undo/redo unaffected:** an evidence-bearing operation undoes and
  redoes to the exact recorded target states; evidence never enters
  plans, effects, conflicts, or reconciliation; the evidence record is
  unchanged by undo/redo.
- **AC9 Real-manager integration (where probed):** captured `cargo
  generate-lockfile --offline` and captured npm lock-only generation
  yield evidence whose pre/post hashes verify in the CAS. Executed
  locally; CI executes both where the tool exists (cargo everywhere; npm
  on all three runners). Any platform/skip is reported, never claimed.
- **AC10 Full regression and gates:** `cargo fmt --all -- --check`,
  `cargo check --all-targets`,
  `cargo clippy --all-targets --all-features -- -D warnings`,
  `cargo test --all-targets` (full suite twice consecutively), every
  existing suite unchanged and green; CI green on ubuntu/macOS/windows
  for the final commit.

## 9. Alternatives considered; consequences; rollback

- **Parse lockfiles to report versions** — version-chasing across many
  formats and a claim stronger than recorded hashes can prove; rejected
  (honesty, scope).
- **Fold evidence into fingerprints/state ids** — would change state
  identity and drift every state id; rejected.
- **Depth-unbounded matching** — probe-verified false positives from
  nested `node_modules/.package-lock.json`; rejected.
- **Case-insensitive matching** — platform-divergent evidence for the
  same recorded state; rejected.
- **`ALTER TABLE operations ADD COLUMN`** — introduces a first-of-kind
  migration mechanism when the established idempotent DDL pattern already
  extends existing catalogs; rejected.
- **A separate `rewind recipes` query surface** — evidence would be
  derived lazily from history rather than recorded with the operation;
  the recorded-evidence property is the point; deferred as unnecessary.
- **Multi-row child table** — a single JSON row per operation makes
  duplicate rows structurally impossible on retry/interleaving paths;
  chosen.
- **Consequences:** operations gain provable dependency-state evidence;
  evidence-free data serializes byte-identically; pre-phase catalogs
  self-extend idempotently; an old binary reading a new catalog ignores
  the new table (authoritative rows are untouched) — no downgrade hazard.
- **Rollback/revert plan:** revert the phase commits; reverted binaries
  ignore the `operation_evidence` table, which is harmless residual
  derived metadata; no authoritative state is affected, so no data
  migration back is needed.

## 10. Documentation pass (delivered with the phase)

`.ai/DECISIONS.md` (ADR-022), `.ai/ROADMAP.md` (phase position; ADR-016
deferral narrowed but standing), `.ai/CURRENT_STATE.md`,
`.ai/HANDOFF.md` (new do-not-regress invariants), `.ai/TEST_STATUS.md`
(newest record on top; CI filled in only after verification),
`.ai/SAFETY_ANALYSIS.md` (§17), `.ai/TODO.md`, `.ai/CHANGELOG.md`,
`.ai/AGENT_LOG.md` (resumability milestones).
