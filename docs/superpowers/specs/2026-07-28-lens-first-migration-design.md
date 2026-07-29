# Lens-first migration system

**Date:** 2026-07-28 (revised 2026-07-29 after design review)
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
   step registry, an engine, and a materialization driver — not a parallel
   versioning system.
2. **Eager materialization after upgrade**, with read-time lens migration as
   the correctness backstop. (Revised 2026-07-29: materialization is an
   upstream defradb.rs primitive, not a gents GraphQL sweep — see §3.)
3. **No legacy support.** The 26 legacy migrations, the field-presence
   machinery, and both existing lens crates are deleted. Databases predating
   the new baseline are rejected with a clear diagnostic (consistent with
   `docs/gents-cutover.md`: no compatibility shims).
4. **New workspace crate `crates/gents-migration`.**
5. **Lean core model included** in this PR, per the foundation flow.

## Design-review findings (2026-07-29)

Four blockers were raised against the first draft; all were verified against
the defradb.rs pin (`8eba3d5`) and cross-checked against Go DefraDB
(`8b961abe`). Verdicts, which drive the revisions below:

1. **Fresh bootstrap bypasses the chain — confirmed.** Every gents bootstrap
   path registers the *current* SDL via `add_schema` before migrations run
   (`crates/gents/src/schema.rs:40-82` and the desktop's byte-identical copy).
   Version CIDs incorporate parent heads and chain depth
   (`defradb.rs crates/db/src/patch/version_id.rs`,
   `crates/schema/src/cid.rs`), so a fresh database registered at the current
   SDL and a migrated database end at **different CIDs for the same logical
   schema** — the lineages can never converge. §2 restructures bootstrap.
2. **`expected_version` does not verify complete state — confirmed.** The
   version CID hashes only new-field lineage (name/kind/CRDT of *added*
   fields, plus priority/heads). Transform attachment, `IsActive`, indexes,
   per-field defaults/immutability, and metadata are all invisible to it —
   transforms are attached as an in-place update that keeps the version ID
   (`patch/mod.rs:163-168`). §1 adds full post-apply state verification and a
   pinned transform ID per lens step.
3. **Default patch/activation sequence is unsafe — confirmed; the remedy
   exists in public API.** `patch_collection` auto-activates
   (`patch/store.rs:268-280`), and any read served in the
   patch→`set_migration` window additionally **poisons a per-process
   migration cache that is never invalidated**
   (`lensed_auto_commit_fetcher/migration.rs:34-51`), leaving reads
   unmigrated even after the lens registers. But an `IsActive: false` entry in
   the patch stores a fully addressable *inactive* version while the old one
   stays active (`patch/apply.rs:68-71`, `store.rs:271-280,336-341`), and
   registering the migration *first* creates a placeholder at the pinned
   destination CID whose transform is adopted atomically when the patch lands
   (`patch/store.rs:192-243`). §1's step sequence uses both; activation
   changes the cache key, which defuses the poisoning hazard.
4. **Eager sweep lacks the required API — confirmed; Go shows the correct
   fix.** No public surface can select documents by stored version (the `/v/`
   datastore key has no reverse index; `_commits.collectionVersionId` requires
   a full DAG walk). A GraphQL-update sweep *would* materialize (updates read
   through the lens fetcher) but emits one ordinary CRDT commit per document
   and gossips each to peers — rejected. Go's lens fetcher instead writes
   migrated values back as **datastore-only writes with no new commits and no
   P2P traffic** (`defradb internal/lens/fetcher.go:296-386`), and Go's
   reindex thereby physically materializes whole collections. The Rust port
   stubbed this out. Materialization is therefore an **upstream defradb.rs
   port of existing Go behavior**, not gents scaffolding — see §3.

The Rust/Go cross-check also found: Rust errors on documents whose stored
version is unknown to the history where Go passes them through unchanged; the
Rust reindex leaves index entries and datastore values disagreeing; and Rust's
head/priority reconstruction after restart diverges from Go when the active
version is not the latest. All are tracked in §7 and shape the engine's
crash-resume discipline.

## 1. Model: the version DAG is the only source of truth

Migration state lives where DefraDB already keeps it: each collection's chain
of content-addressed version IDs, with lens transforms attached as edges
(`PreviousVersion.transform`). No gents-side ledger, no field-presence checks.

A migration is authored as data — one **step** in a static registry. Not every
schema change produces a new version CID (DefraDB applies index, embedding,
and similar metadata changes in place), and freezing the baseline must not
make new collections unaddable — so a step is an operation enum, each variant
with its own pre/post-state expectations:

```rust
enum MigrationStep {
    // Register a collection that did not exist at the baseline. Starts a new
    // lineage root for that collection (depth 1, no heads).
    AddCollection {
        id: &'static str,
        sdl: &'static str,
        expected_version: &'static str,        // pinned CID of the created version
        expected_state: CollectionExpectation, // full post-state (below)
    },
    // A versioned change: field additions/renames — anything that mints a
    // new version CID.
    PatchVersioned {
        id: &'static str,                      // e.g. "2026-07-add-request-priority"
        collection: &'static str,              // exactly one collection per step
        patch: &'static str,                   // RFC 6902; always includes
                                               // {"op":"replace","path":"/IsActive","value":false}
        lens: Option<LensSpec>,                // embedded wasm + args; None if purely additive
        expected_version: &'static str,        // pinned CID of the resulting version
        expected_transform: Option<&'static str>, // pinned TransformId when lens is Some
        expected_state: CollectionExpectation,
    },
    // An in-place change: indexes, embeddings, other metadata DefraDB applies
    // without a new version CID. Must be idempotent; "applied" is decided by
    // the expectation predicate alone, since no CID moves.
    PatchInPlace {
        id: &'static str,
        collection: &'static str,
        patch: &'static str,
        expected_state: CollectionExpectation,
    },
}
```

(View support — `AddView` — is added when gents first needs a view; the enum
makes that a new variant, not a redesign.)

### Pins verify lineage; verification covers everything else

`expected_version` is content-derived, so the engine can locate any database's
position in the chain by its version IDs and detect drift at the exact step.
But the CID hashes only added-field lineage — so every step carries a
`CollectionExpectation`: a **normalized digest of the complete persisted
collection descriptor** — fields with kinds, CRDTs, relations, defaults,
sizes, and immutability flags; indexes (all classes); policy; embeddings and
downsample settings; branchable/materialized/embedded-only flags; and the
`PreviousVersion` edge including its transform ID — excluding only documented
runtime-derived values. The SDL-parity test compares this same normalized
representation, not a bare field list.

Verification checks the stored descriptor against the expectation and, for
lens steps, that `PreviousVersion.transform` equals `expected_transform`
(transform IDs are content-derived from the lens modules, so this pin is also
stable). Verification failure is a hard, step-attributed error — never a
warning.

### The safe step sequence, with derived crash position

Each pending `PatchVersioned` step applies as:

1. **Attach first** (lens steps only): `set_migration(src=prev pin,
   dst=this pin, lens)`. This creates a placeholder at the pinned destination
   CID; the transform is adopted atomically when the patch stores the version.
2. **Patch inactive**: `patch_collection` with `IsActive: false` in the patch.
   The new version is stored, addressable, and *inactive*; the old version
   keeps serving readers. No reader ever observes a version whose lens is
   missing, and the migration cache cannot be poisoned for the new version.
3. **Verify**: the state verification above, against the stored (still
   inactive) version.
4. **Activate**: `set_active_collection_version(pin)` — a single-transaction
   flip that also changes the read path's cache key and triggers reindexing.

**Crash position is derived from observable database state, never stored** —
there is no gents ledger, so the engine cannot know what a previous run
verified; it re-verifies. A CID being present is also not proof the patch
ran: `set_migration` creates a *placeholder* at that same pinned CID. The
observable phases per step:

- destination CID absent → attach (lens steps), then patch;
- destination present but a **placeholder** → patch inactive;
- destination **complete and inactive** → verify, then activate;
- destination **complete and active** → verify, and repair its edge
  (`set_migration` is an in-place update) if the transform is missing.

Verification is a predicate over *current* state and runs on every pass
through a step, including for steps applied long ago (cheap descriptor
comparisons). This subsumes the legacy system's worst hazard — lens
registration silently skipped forever — and the engine never re-derives a CID
over a half-applied state, which also sidesteps the known head-reconstruction
divergence after restarts (§7).

`PatchInPlace` steps have no CID to observe: they are required idempotent,
and "applied" is exactly "the expectation predicate holds"; the engine
re-applies the patch when it does not.

**Post-activation repair state.** Activation commits before reindexing
(`set_active_collection_version` commits its transaction, then reindexes), so
a reindex failure means "activation durable, post-activation work pending."
The engine treats this as a distinct recoverable phase: on the next pass the
step verifies as complete-and-active and the engine re-runs reindex /
materialization rather than treating the failure as a step failure.

**Serialization.** `ensure_migrations` is serialized per node: a process-wide
lock guards re-entry (desktop and runtime paths can race within one process),
and cross-process exclusion rides on the store's single-open lock.

Note: `IsActive: false` combined with field additions is exercised in Go's
integration suite but not in defradb.rs's; a conformance test locking this
behavior lands upstream (or in gents e2e) before the engine relies on it.

### Authoring workflow

Write the patch (and lens if the change is not purely additive), run the
chain-replay test, copy the printed version ID and transform ID into the pins,
commit. The test failure message includes the computed IDs to make this a
paste, not a hunt.

### One lineage for every database

Fresh installs register the **baseline SDL** (the `gents-protocol` schemas
frozen at cutover) and then replay the chain — schema-only patches on an empty
database, so it is fast, and every database in the fleet shares one version
lineage. This is what makes the pins universal. The baseline is
**feature-invariant**: all collections register regardless of feature flags
(the agent-memory flag currently filters the registered set in
`schema.rs:74-82`; under pinned chains that filter must move out of schema
registration, or pins would become feature-dependent).

The current-SDL constants in `gents-protocol` remain for docs, desktop
collection resolution, and the SelfConfig conformance fence. A conformance
test asserts baseline + chain ≡ current SDL, field for field, so the two
representations cannot drift.

**Network join semantics.** Because every node — fresh or long-lived — arrives
at the same version CIDs, new nodes join the P2P network with an identical
version DAG: replicated documents' version stamps always resolve in the
receiver's history, and lens edges migrate them in either direction. The only
mixed-version caveat is a node running an *older binary* (shorter chain)
receiving documents from a newer peer: the stamp is beyond its known chain,
which Go passes through unchanged but the Rust port currently fails on
(upstream issue 2, §7). Until that lands, rolling upgrades should promote
older nodes promptly; this is an upgrade-window concern, not a join barrier.

### Unknown state fails loudly — and completely

The engine polices the **entire version DAG** of every managed collection,
not just the presence of pins. Two rejection classes:

- `Error::UnknownLineage` — none of a collection's versions match a known pin:
  the database predates the migration baseline (or was produced by foreign
  patches) and requires export/import.
- `Error::ForeignVersion` — the lineage is recognized but the DAG contains a
  version or edge that is neither a pin nor an expected placeholder. A foreign
  version — even inactive — is not harmless: head/priority reconstruction
  counts stored versions, so it can change the CIDs derived for every
  subsequent patch. Rejected before any step applies.

No silent limping, no partial application over unknown state.

## 2. Crate layout and bootstrap restructure

```
crates/gents-migration/
├── build.rs          # builds lens wasm crates referenced by the registry (moved
│                     # from crates/gents/build.rs, generalized; stub fallback
│                     # becomes a hard error, not a cargo:warning)
├── src/
│   ├── lib.rs        # pub fn ensure_migrations(node) -> Result<MigrationReport>
│   │                 # — THE single schema entry point: baseline + chain + verify
│   ├── registry.rs   # baseline SDL reference + the static step chain
│   │                 # (ships with zero steps at cutover); the engine takes a
│   │                 # registry as input, so tests inject their own chains
│   ├── engine.rs     # locate-in-chain → attach → patch-inactive → verify →
│   │                 # activate; positional crash resume; edge repair
│   ├── lens.rs       # embedded wasm via LensModule::from_bytes — no temp
│   │                 # files, no OnceLock, no startup panics
│   ├── materialize.rs# thin driver over the upstream materialization API (§3)
│   ├── report.rs     # MigrationReport: steps applied, edges repaired,
│   │                 # materialization stats, warnings
│   └── error.rs      # typed thiserror enum; anyhow only at the caller boundary
└── tests/            # conformance tests derived from the Lean model,
                      # chain-replay/pin test, SDL-parity test, fixture-lens e2e
```

### Bootstrap becomes unbypassable

Registering schemas and replaying the chain are one operation. There is no
public "register schemas" function left to call on its own — the review showed
that any bypass creates a divergent lineage:

- `ensure_migrations(node)` registers the frozen baseline SDL (swallowing
  "already exists" exactly as today), replays pending steps, verifies, and
  materializes. It is idempotent, resumable, and cheap when current, because
  every host calls it on every start — six production call sites replace
  `ensure_all_runtime_migrations`.
- `crates/gents/src/schema.rs` shrinks to a thin re-export of the baseline
  used by `gents-migration`; the byte-identical desktop copy
  (`gents-desktop-core/src/client/schema.rs`) is **deleted** and desktop
  bootstrap calls the same entry point.
- The two existing bypasses are converted: `gents session fork`
  (`session.rs:116`, registers schemas with no migrations at all) and the
  out-of-band `ensure_agent_behavior_migrations` inside
  `Gents::from_default_behavior_documents` (`agent.rs:163`).
- The entry point tolerates databases where target collections don't yet
  exist (the CLI runs migrations even when `ensure_local_schemas` is false) —
  absent collections are bootstrapped, never `UnknownLineage`.
- Test helpers (~246 `ensure_*schemas` call sites, mostly `#[cfg(test)]`)
  route through the same entry point via one shared test-support function.
- `gents init`'s six-collection `CONFIG_BOOTSTRAP` subset is replaced by the
  full baseline — partial registration would fork the lineage per subset.

## 3. Eager materialization, lazy backstop

**The materialization primitive lives upstream in defradb.rs**, as a port of
behavior Go already has — not as gents scaffolding. Rationale from review: no
public API can enumerate documents by stored version; a GraphQL-update sweep
would emit one ordinary CRDT commit per document and gossip each to peers,
rewriting history fleet-wide; and Go's reference implementation already solves
this with datastore-only write-back (migrated field values + the `/v/` doc
version key, no new commits, nothing replicated —
`internal/lens/fetcher.go:296-386`), which the Rust port stubbed out.

Upstream work (small, reference implementation exists in Go):

1. Port the lens fetcher write-back: migrated values and the doc version key
   are persisted datastore-only on first lensed read.
2. Expose `materialize_collection(collection)` on `EmbeddedNode`: iterate the
   collection through the lensed fetcher in a write transaction (exactly what
   Go's reindex-after-migration already does), so a caller can eagerly
   materialize instead of waiting for organic reads. This also fixes the
   Rust-only bug where reindex leaves index entries and datastore values
   disagreeing.
3. **Identity materialization for transform-less paths.** Both Go and the
   Rust pin skip the lensed path entirely when the targeted history contains
   no transforms (`hasMigrations == false`), so a purely additive chain
   (`lens: None`) never advances any document's stored version key by
   iteration alone. `materialize_collection` must therefore also re-stamp the
   version key for documents whose stored version differs from the active one
   even when no transform exists on the path — otherwise "all documents at
   the active version" is unreachable for additive migrations.

Gents' `materialize.rs` is then a thin driver: call the API per collection
after the chain is current, time-box and resume, and surface progress in
`MigrationReport` via `tracing`. Read-time lens migration remains the
correctness backstop for documents arriving later via P2P from older peers.

Until the pin advances to include the upstream API, `ensure_migrations` runs
chain + verification only, and reads pay the (correct) lazy-transform cost;
the driver activates when the API is present. No GraphQL fallback sweep ships.

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
`sorry`s. State: per-collection version set (each version complete or
placeholder, with active flag and descriptor), per-document versions, edge
set with optional transforms. **Verification is a predicate over current
state, not a stored fact** — the model has no persisted verification status,
mirroring the ledger-free engine. Transitions: `attach_transform`,
`patch_inactive`, `activate` (guarded by the verification predicate),
`materialize_step`, `patch_in_place`. Theorems:

- **Idempotence** — `ensure` on an ensured state is a no-op.
- **Convergence** — from any reachable state, including every crash window
  (placeholder attached but unpatched; version stored but not yet
  activatable; complete but inactive; transform missing on an applied edge;
  activation durable but post-activation work pending), repeated `ensure`
  reaches the target: pinned version active, transform attached, expectation
  predicate holding. This theorem forces derived-position resume and edge
  repair into existence.
- **No unverified activation** — `activate` is enabled only in states where
  the verification predicate holds for that version; readers therefore never
  observe an active version whose declared lens is unattached. (This is the
  ordering blocker, stated as an invariant.)
- **Pin soundness** — a step applies only when the prior state matches its
  expected predecessor; unknown or foreign versions are rejected, never
  patched over.
- **Materialization termination** — over a **quiescent document snapshot**
  (no arrivals during the run), the count of documents not at the active
  version strictly decreases per materialization step, including for
  transform-less (identity) steps; interrupt + resume still terminates at
  "all snapshot documents at active version." Later P2P arrivals legitimately
  create new work and are outside the theorem's scope by construction.

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
- `crates/gents-desktop-core/src/client/schema.rs` (duplicate bootstrap).
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
  version ID, transform ID, and `CollectionExpectation` digest (this is also
  the authoring tool for new pins).
- SDL-parity test: baseline + chain ≡ current `gents-protocol` SDL, compared
  over the same normalized descriptor representation the expectations use.
- Inactive-patch behavior lock: field additions + `IsActive: false` stores an
  inactive version while the old stays active (the upstream-untested
  combination §1 relies on).
- Conformance tests mirroring the five theorems, including crash-injection at
  each window boundary (post-attach, post-patch, post-activate-pre-reindex)
  to exercise derived-position resume, edge repair, the post-activation
  repair state, and the no-unverified-activation invariant.
- Foreign-state rejection: a hand-injected extra version (inactive included)
  in a managed collection's DAG yields `ForeignVersion` before any step
  applies.
- Lensless additive migration test: a `lens: None` step followed by
  materialization leaves every document re-stamped at the active version
  (exercises upstream identity materialization).
- Fixture-lens e2e: seed docs at baseline, apply a lens step through the full
  sequence, assert transformed reads; materialize when the upstream API is
  present; re-open and re-run for idempotence; P2P-arrival backstop test.
- Gate with `cargo test -p gents -p gents-migration` and
  `cargo check --workspace --all-targets` per CLAUDE.md.

## 7. Upstream defradb.rs issues to file

Grounded by the 2026-07-29 verification; first two block full functionality,
the rest are correctness hazards gents designs around:

1. **Lens write-back / materialization parity with Go** (§3): port
   `updateDataStore` (datastore-only persistence of migrated values + doc
   version key) and expose `materialize_collection`, including identity
   (version-key-only) materialization for transform-less paths, which neither
   implementation performs today. Includes fixing reindex
   leaving datastore and index values disagreeing, and removing the stub that
   writes malformed keys under the real `/v/` prefix
   (`lensed_fetcher/migration.rs:437-445`).
2. **Unknown-version reads error instead of passing through**: Go emits docs
   of unknown versions unchanged (`internal/lens/lens.go:139-149`); Rust
   fails the whole query (`lensed_auto_commit_fetcher/migration.rs:287-292`).
   P2P docs from newer peers can brick reads on older nodes.
3. **Migration cache never invalidated on `set_migration`**
   (`lensed_auto_commit_fetcher/migration.rs:34-51`): a read in the
   patch→set_migration window poisons `(false, None)` for the process
   lifetime. Gents' ordering avoids the window; the cache still needs
   invalidation upstream.
4. **Head/priority reconstruction after restart diverges from Go** when the
   active version is not the latest (`collection_ops/mod.rs:206-240`),
   changing CIDs computed for subsequent patches. Gents' engine never
   re-patches over a half-applied state, but cross-implementation ID parity
   needs the persistent-headstore semantics.
5. **`patch_collection` lacks Go's inline `migration` parameter**
   (`client/db.go:201-205`): the placeholder pre-attach path works but
   requires predicting the destination CID; the inline parameter would remove
   that coupling.
6. **Non-wasmtime builds silently skip migration**: `MemoryTransformStore` is
   a pass-through substituted on wasm32 (and any build without
   `wasmtime-runtime`, plausibly including future iOS targets). Such a node
   reads old-version documents unmigrated with no error.
7. Minor: two coexisting transform-ID schemes (`set_migration`'s sha256
   pseudo-CID vs `add_lens`'s real IPLD CID); txn-path history cache keyed by
   collection ID only (stale after version switch).
