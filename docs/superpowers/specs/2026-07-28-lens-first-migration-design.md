# Lens-first migration system

**Date:** 2026-07-28
**Status:** Approved design, pre-implementation
**Replaces:** `crates/gents/src/migration.rs` (5,161 lines) and both legacy lens crates

## Problem

Gents grew a parallel, ad-hoc schema-versioning system alongside a database that
already has a real one. `crates/gents/src/migration.rs` holds 26 hand-written
migrations orchestrated by a hand-maintained sequential list. Migration state is
inferred by field presence (`collection_has_field`, 74 call sites) — there is no
version model, no applied-migrations ledger, and no design doc or Lean model.
Concrete defects found in review:

- **Lens registration can be skipped forever.** The forward lens is registered
  only in the same process run that applies the schema patch. If
  `set_migration` fails after the patch succeeds, every future boot sees the
  new field present, takes the no-op path, and never registers the lens.
- The same check→patch→activate shape is copy-pasted ~40 times; six migration
  functions are byte-identical except for a collection name.
- Dead patch constants that tests validate but production never executes
  (false assurance), a live stale-cursor instance structurally identical to a
  previously fixed bug, and a backfill documented as "always fails on the
  current DefraDB pin" still wired in.
- Only 2 of 26 migrations actually use Lens; embedded wasm is unpacked to
  process-lifetime temp files because the loader was assumed to be path-only.
- 44% of the file is one test module with 16 hand-copied stale SDL constants.

Meanwhile our pinned `defradb.rs` already implements the DefraDB migration
model: content-addressed collection `VersionID`s forming a version DAG,
`patch_collection` producing new versions, `set_migration(LensConfig)`
attaching a lens transform to the edge between two versions (with adjacency
validation and placeholder versions), lazy read-time transformation with
multi-hop forward/inverse traversal, sandboxed wasmtime execution, persistence
across restarts, and P2P lens sync. `LensConfig` accepts inline wasm bytes
(`LensModule::from_bytes`), so temp files are unnecessary.

## Decisions (settled with Jack, 2026-07-28)

1. **Full lens-first redesign.** The database's native version DAG is the only
   source of truth. Gents adds minimal scaffolding around it — a declarative
   step registry, an engine, and a sweep — not a parallel versioning system.
2. **Eager sweep after upgrade**, with read-time lens migration as the
   correctness backstop.
3. **No legacy support.** The 26 legacy migrations, the field-presence
   machinery, and both existing lens crates are deleted. Databases predating
   the new baseline are rejected with a clear diagnostic (consistent with
   `docs/gents-cutover.md`: no compatibility shims).
4. **New workspace crate `crates/gents-migration`.**
5. **Lean core model included** in this PR, per the foundation flow.

## 1. Model: the version DAG is the only source of truth

Migration state lives where DefraDB already keeps it: each collection's chain
of content-addressed version IDs, with lens transforms attached as edges
(`PreviousVersion.transform`). No gents-side ledger, no field-presence checks.

A migration is authored as data — one **step** in a static registry:

```rust
MigrationStep {
    id: "2026-07-add-request-priority",   // human identifier, stable
    collection: "AgentRequest",           // exactly one collection per step
    patch: r#"[{"op":"add", ...}]"#,      // RFC 6902 collection patch
    lens: Option<LensSpec>,               // embedded wasm + args; None for purely additive changes
    expected_version: "bafy…",            // pinned CID of the resulting CollectionVersion
}
```

### The version pin

`expected_version` is the linchpin. Version IDs are content-derived, so:

- The engine locates any database's position in the chain by looking up its
  active version ID, and applies only the remaining steps.
- After each `patch_collection`, the engine asserts the produced version ID
  equals the pin. Registry/reality drift is a hard error at the exact step,
  never a silent divergence.
- A conformance test replays the full chain from baseline on a fresh node and
  asserts every pinned ID, so a wrong pin cannot merge.

Authoring workflow for a new migration: write the patch (and lens if the change
is not purely additive), run the chain-replay test, copy the printed version ID
into `expected_version`, commit. The test failure message includes the computed
ID to make this a paste, not a hunt.

### One lineage for every database

Fresh installs register the **baseline SDL** (the `gents-protocol` schemas
frozen at cutover) and then replay the chain — schema-only patches on an empty
database, so it is fast, and every database in the fleet shares one version
lineage. This is what makes the pins universal.

The current-SDL constants in `gents-protocol` remain for docs and tooling. A
conformance test asserts baseline + chain ≡ current SDL, field for field, so
the two representations cannot drift.

### Pre-baseline databases fail loudly

If the active version ID is not in the known chain, `ensure_migrations` returns
`Error::UnknownLineage` with a diagnostic: the database predates the migration
baseline (or was produced by foreign patches) and requires export/import. No
silent limping, no partial application over unknown state.

## 2. Crate layout

```
crates/gents-migration/
├── build.rs          # builds lens wasm crates referenced by the registry (moved
│                     # from crates/gents/build.rs, generalized; stub fallback
│                     # becomes a hard error, not a cargo:warning)
├── src/
│   ├── lib.rs        # pub fn ensure_migrations(node) -> Result<MigrationReport>
│   ├── registry.rs   # baseline SDL reference + the static step chain
│   │                 # (ships with zero steps at cutover); the engine takes a
│   │                 # registry as input, so tests inject their own chains
│   ├── engine.rs     # locate-in-chain → patch → verify pin → set_migration
│   │                 # → set_active_collection_version; edge repair
│   ├── lens.rs       # embedded wasm via LensModule::from_bytes — no temp
│   │                 # files, no OnceLock, no startup panics
│   ├── sweep.rs      # eager materialization (§3)
│   ├── report.rs     # MigrationReport: steps applied, edges repaired,
│   │                 # sweep stats, warnings
│   └── error.rs      # typed thiserror enum; anyhow only at the caller boundary
└── tests/            # conformance tests derived from the Lean model,
                      # chain-replay/pin test, SDL-parity test, fixture-lens e2e
```

### Edge repair is a first-class check

On every run, for each already-applied step, the engine verifies the version
edge actually carries its transform (`PreviousVersion.transform` is `Some`
whenever the step declares a lens) and re-runs `set_migration` if not. A crash
between patch and lens registration heals on the next boot. This directly
eliminates the worst legacy hazard and is forced into existence by the
convergence theorem (§5).

### Wiring

`gents` depends on `gents-migration`. The six current call sites of
`ensure_all_runtime_migrations` (daemon startup, oneshot, `gents init`,
`gents server`, CLI config access, desktop bootstrap) become one call to
`gents_migration::ensure_migrations(node)`. The out-of-band
`ensure_agent_behavior_migrations` call inside
`Gents::from_default_behavior_documents` (`agent.rs:163`) is deleted — the
single-entry-point policy the old file stated in a comment becomes structural.

## 3. Eager sweep, lazy backstop

After the engine brings the schema chain current, `sweep.rs` materializes
documents:

- Paged scan (bounded page size, resumable) per collection for documents whose
  stored version differs from the active version.
- Reading them through the normal query path forces the lens transform; the
  sweep writes the migrated values back via standard update mutations, which
  persists the document at the active version. This works around the write-back
  cache stub in the pinned `defradb.rs` (see Known limitations).
- Progress is `tracing`-instrumented and summarized in `MigrationReport`.
  Interruption is safe: a re-run resumes from whatever the scan still finds
  unmigrated. All GraphQL built by the sweep goes through
  `graphql::escape_graphql_string()`.

Read-time lens migration remains the correctness backstop for documents that
arrive after the sweep via P2P from older peers.

## 4. Lens authoring convention

- One crate per lens under `crates/gents-lenses/`, one collection per lens
  (a lens edge binds one collection's version pair — the legacy multi-collection
  shape-heuristic dispatch dies with the legacy lenses).
- Standard shape, taken from the better legacy crate:
  `crate-type = ["cdylib", "rlib"]` with a default-on `lens-entry` feature, so
  `lens_sdk::define!` is gated to wasm32 builds and transform logic stays
  natively unit-testable. Unit tests are required.
- CI builds every lens crate for `wasm32-unknown-unknown` by glob, not by
  hand-list.
- The PR ships one **fixture lens** used by conformance/e2e tests, which doubles
  as the authoring template (the registry itself starts with zero real steps).

## 5. Lean model

A small `Migration` model in `crates/gents/proofs/`, house style, zero
`sorry`s. State: active version per collection, per-document versions, edge set
with optional transforms. Transitions: `patch`, `attach_transform`, `activate`,
`sweep_step`. Theorems:

- **Idempotence** — `ensure` on an ensured state is a no-op.
- **Convergence** — from any reachable state, including crash-interrupted ones
  (patched but transform unattached, activated but unswept), repeated `ensure`
  reaches the target version with all declared transforms attached.
- **Pin soundness** — a step applies only when the prior state matches its
  expected predecessor; unknown versions are rejected, never patched over.
- **Sweep termination** — the count of documents not at the active version
  strictly decreases per sweep step; interrupt + resume still terminates at
  "all documents at active version."

Conformance tests in `crates/gents-migration/tests` mirror these
theorem-by-theorem, per the foundation flow (Lean → conformance tests →
implementation).

## 6. Deletions and test plan

Deleted:

- `crates/gents/src/migration.rs` in its entirety.
- `crates/gents-lenses/agent_tool_call_lifecycle_v1_to_v2` and
  `crates/gents-lenses/agent_subagent_v2_to_v3`.
- The lens-build portion of `crates/gents/build.rs` (moves to
  `gents-migration/build.rs`).
- Legacy migration e2e tests (`agent_behavior_migration.rs`,
  `tool_call_migration.rs`; the unrelated `SubagentTarget::parse` regression
  test parked in the former moves to an appropriate home).
- The hand-enumerated migration subset in
  `crates/gents-cli/tests/cli_subagent_cancel.rs` (the anti-pattern the old
  file's policy comment prohibited).

Kept and re-pointed:

- `defradb_v0612_store_upgrade.rs` is store-format-level and stays; against the
  new engine its fixture predates the baseline, so it now asserts the
  `UnknownLineage` diagnostic.

New coverage:

- Chain-replay test: fresh node, baseline + all steps, assert every pinned
  version ID (this is also the authoring tool for new pins).
- SDL-parity test: baseline + chain ≡ current `gents-protocol` SDL.
- Conformance tests mirroring the four theorems, including crash-injection
  between patch / set_migration / activate to exercise edge repair and
  convergence.
- Fixture-lens e2e: seed docs at baseline, apply a step with a lens, sweep,
  assert transformed data and version; re-open and re-run for idempotence;
  P2P-arrival backstop test (old-version doc lands post-sweep, reads migrated).
- Gate with `cargo test -p gents -p gents-migration` and
  `cargo check --workspace --all-targets` per CLAUDE.md.

## Known limitations and upstream issues to file

Both worked around here, neither fixed here — file against `defradb.rs`:

1. **Write-back cache is a stub** (`lensed_fetcher/migration.rs:371-457` logs
   "Would cache migrated field value"; the live non-txn fetcher has no
   write-back at all), so lazy reads recompute the wasm transform on every
   fetch. The eager sweep makes this cost a one-time upgrade cost for local
   documents; P2P-arriving old documents pay it per read until swept again.
2. **Non-wasmtime builds silently skip migration**: `MemoryTransformStore` is a
   pass-through, substituted on wasm32 (and any build without the
   `wasmtime-runtime` feature, plausibly including future iOS targets where JIT
   is restricted). Such a node receiving old-version documents over P2P would
   read them unmigrated with no error. Gents does not currently ship such a
   node, but this must be resolved upstream before one exists.

Also noted upstream (no action needed from gents): two coexisting transform-ID
schemes (`set_migration`'s sha256 pseudo-CID vs `add_lens`'s real IPLD CID),
and the txn-path history cache keyed by collection ID only (stale after a
version switch).
