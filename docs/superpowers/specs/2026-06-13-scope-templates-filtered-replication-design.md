# Scope templates + filtered replication — coherent pairing surface

Date: 2026-06-13
Branch: `cli-normalization` (extends the registry/pairing work in
`2026-06-13-peer-registry-service-discovery-design.md`)
Status: draft for review

## Goal

Filtered replication (defradb.rs #1033) lets a replicator carry a per-collection
equality predicate, so a node can replicate *only one agent's slice* of a
collection instead of the whole thing. Use that to collapse today's pairing
complexity — five raw collection profiles, whole-collection-default semantics,
and the subscribe-*and*-replicate dual install — into **one user-facing
abstraction: the scope template.**

A scope template is a named, purpose-built pairing intent. The operator says
"pair with peer X using template `conversation`"; the template encodes which
collections, how they're scoped (filtered to the peer's DID, or unscoped), and
how they're delivered (filtered push vs. subscribe+replicate). Users make quick,
correct pairing decisions by name without understanding schemas, collection
lists, or filter fields. The catalog grows as use cases emerge.

**Dependency:** defra.rs #1033 (filtered replication). We build against its API
now; the workspace pin bumps to the merged rev when #1033 lands. The #1033 API
surface this design targets:
- `ReplicationFilters = BTreeMap<String, ReplicationFilter>` (collection →
  predicate), `ReplicationFilter { field: String, value: JsonValue }` — per-
  collection single-field equality.
- `ReplicatorInfo.filters` (serialized `Filters`, omitted when empty → byte-
  identical unfiltered records).
- A replication filter field **must be `@immutable`** — enforced on local write
  AND remote merge (an immutable-field change is a terminal merge skip). This is
  the DAG-completeness guarantee (a doc can't drift in/out of a filter).
- Filtered peers receive matching full-document DAGs by **direct push** and are
  **not** subscribed to collection-wide gossip.

## The scope template

```
ScopeTemplate {
  id: String,                     // "conversation", "agent-config", ...
  collections: [String],          // the collection set
  scope: Scope,                   // PeerDid | Unscoped
  delivery: Delivery,             // Push | Replicate
}

Scope    = PeerDid { field }      // filter <field> == the peer's agent_did
         | Unscoped               // no filter
Delivery = Push                   // filtered replicator only (no subscription)
         | Replicate              // subscribe + replicator (whole-collection)
```

`scope` resolves to a `ReplicationFilters` entry per collection at reconcile
time: `PeerDid { field: "agent_did" }` against peer `did:key:bob` →
`{ <collection>: { field: "agent_did", value: "did:key:bob" } }` for each
collection in the template. `Unscoped` → no filter for those collections.

`delivery` decides the reconciler action: `Push` installs a filtered replicator
and does NOT subscribe; `Replicate` subscribes + installs an unfiltered
replicator (today's behavior, retained for whole-collection cases).

### Built-in templates (initial catalog)

Hardcoded in Rust for the prototype (a small `&[ScopeTemplate]` catalog), not a
DefraDB collection — document-driven operator-defined templates are the eventual
end state but out of scope now (keep the data shapes clean so it can migrate
later without a surface change).

| id | collections | scope | delivery | use case |
|---|---|---|---|---|
| `conversation` | AgentRequest, AgentResponse, AgentMessage, AgentToolCall, AgentToolResult, AgentSession, AgentConversation, CompactionEntry (8) | `PeerDid{agent_did}` | Push | **default** — a peer agent's conversation slice (subagent delegation) |
| `agent-config` | AgentBehavior, ToolSelection, InferenceBackend, InferenceProfile, ToolServiceRegistry, Skill | Unscoped | Replicate | share a runnable behavior + its inference/tool config + tool services/skills so another node can run it |
| `backup` | the `conversation` collections | Unscoped | Replicate | a backup/observability node holding ALL agents' logs (admin) |

`conversation` is the default for `pairings invite`/`join`/`set`. `backup` is
the explicit "I want everything" path (and `p2p admin` raw commands remain for
ad-hoc surgery). `agent-config` deliberately **excludes** `AgentPrincipal`
(identity/DID is per-deployment, not shared); it includes `AgentBehavior` so the
shared inference/tool config is actually runnable on the receiving node.

`CodexThreadProjection` is intentionally **excluded** from `conversation`: it is
Codex-TUI-local session state (cwd/name/archived/goal/git/rollback), not data a
delegating peer needs. It is also a refactor candidate (derive from loaded
tools/runtime state instead of a dedicated collection) — tracked in
**issue #494**, separate from this work.

### What this retires

- **Raw collection profiles** (`runtime`/`agent`/`desktop-config`/
  `chat-requests`/`tool-services` as the pairing knob) → folded into templates.
  `chat-requests`'s collection set becomes the `conversation` template (now
  scoped). The `P2pCollectionProfile` enum stays only where `p2p admin` /
  internal callers still need raw collection bundles.
- **Whole-collection-as-default** → the default is scoped; unscoped is the
  explicit `backup` template / `--all`.
- **Subscribe+replicate in the pairing path** → the pairing reconciler installs
  filtered push for `Push` templates and never blindly subscribes. `Replicate`
  delivery (config/backup) is the only path that still subscribes, and raw
  subscription stays in `p2p admin`.

## Schema: `@immutable agent_did` scope key (Task 0, gating)

The `conversation` template scopes every collection by `agent_did`, so
`agent_did` must exist and be `@immutable` on all 8. Decided (2026-06-13): mark
the existing `agent_did` immutable directly (it is logically write-once; no new
`owner_did` field, in-place is acceptable). But **5 of the 8 conversation
collections key on `session_id` and have no `agent_did` at all** — so the scope
key has to be added there. Inventory:

- **Already have `agent_did` → mark `@immutable`:** AgentRequest, AgentResponse,
  AgentToolResult, AgentConversation.
- **Lack `agent_did` → add it as a new `@immutable` field, stamped at create:**
  AgentMessage, AgentToolCall, AgentSession, CompactionEntry. These are all
  created inside a session that belongs to exactly one agent, so the owning
  `agent_did` is known and fixed at create — denormalizing it is correct and the
  add is an ordinary additive migration (a brand-new field has no prior values to
  violate immutability).

Task 0 work:
1. Audit that `agent_did` is never rewritten after create on the 4 that have it
   (grep every writer; lifecycle must not mutate it).
2. Mark `@immutable` on those 4.
3. Add `agent_did @immutable` to the 4 session-keyed collections; stamp the
   owning agent at every create site (the session owner is in scope there).
4. One guard test: a scoped collection rejects an `agent_did` rewrite.

This gates the filtered reconciler path and is the longest pole — do it first.
(`@immutable` enforcement itself — local-write + remote-merge — is #1033's;
we only declare it and stamp correctly.)

## PeerPairingDesired / registry

- `PeerPairingDesired` gains `template: String` (the template id) and a resolved
  `filters` representation (per-collection `{field, value}`) materialized at
  write/reconcile time. `collections` stays (resolved from the template) so the
  pairing reconciler's existing read is unchanged; `source` (operator/registry)
  is unchanged.
- The registry offer (`PeerRegistry.profiles`) becomes `templates` — a node
  advertises which templates it offers (e.g. "I offer `conversation` scoped to
  your DID"). Discovery materializes a scoped `PeerPairingDesired` from the
  offered template.

## Reconciler

- Resolve `template` → `(collections, scope, delivery)`.
- `scope = PeerDid{f}` → build `ReplicationFilters { c: {field:f, value: peer_did} }`
  for each collection `c`.
- `delivery = Push` → `add_replicator(addresses, collections, filters)`, NO
  `add_p2p_collections`. The filter is part of the replicator's desired identity:
  a changed filter ⇒ reinstall.
- `delivery = Replicate` → today's path (`add_p2p_collections` + unfiltered
  `add_replicator`).
- `RemoteP2pAdmin::add_replicator` gains a `filters: ReplicationFilters`
  parameter; `EmbeddedRemoteP2pAdmin` + `HttpRemoteP2pAdmin` pass it through to
  the #1033 adapter surface. `delete_replicator` keys unchanged.
- Ownership (operator vs registry `source`) and the applied-set teardown are
  unchanged.

## Lean

- Extend `PairingReconcile` replicator dimension: a replicator's identity
  becomes `(address, collections, filter)` — `reconcileInstallReplicator` /
  `Teardown` carry the filter, and a filter change is an install-then-teardown
  (reinstall), preserving convergence/idempotence/no-flap over the enriched
  identity.
- Template resolution is a pure function `template → (collections, scope,
  delivery)` modeled below the reconciler (like the registry derivation): prove
  it's deterministic and total over the catalog. The scope→filter step
  (`PeerDid + peer_did → filter`) is pure.
- `@immutable`/DAG-completeness is upstream's proof obligation (#1033), not ours;
  we cite it.

## CLI surface

- `p2p pairings invite|join|set --template <id>` (default `conversation`).
  `--template` replaces `--profile` as the front door; `--profile`/`--collection`
  remain hidden/admin for raw bundles. `set` gains nothing else — scope is
  derived from the template + the peer DID.
- `p2p network` template offers surfaced in `list`.
- `p2p templates list` (new) — show the catalog (id, collections, scope,
  delivery) so users can see what each does without reading schemas.
- `p2p admin` unchanged — raw `connect`/`collections`/`replicators`/`documents`,
  now `replicators add` gains an optional `--filter <collection>:<field>=<value>`
  for power users / backup wiring.

## Testing

- Lean: enriched replicator-with-filter dimension (reinstall-on-filter-change,
  convergence retained); template resolution determinism. Zero `sorry`.
- Conformance: filter is part of replicator identity (change ⇒ reinstall);
  template→filter resolution. Call the real resolution/diff fns, not reimpls.
- Integration: two paired nodes on `conversation` template — only the scoped
  agent's docs replicate (write docs for two DIDs on the source, assert only the
  pairing-DID's docs appear on the peer); `backup`/admin replicate-all still
  works; filter change reinstalls the replicator.
- The `@immutable` enforcement itself is upstream-tested; we add one test that a
  scoped collection rejects an `agent_did` rewrite (guards our invariant).

## Sequencing (stacked commits; #1033 pin bump lands at merge)

0. **`@immutable agent_did` scope key** (gating; no #1033 dep) — audit
   `agent_did` write-once on the 4 that have it + mark `@immutable`; add
   `agent_did @immutable` to the 4 session-keyed conversation collections and
   stamp the owning agent at every create site; guard test for rejected rewrite.
1. Scope-template model + built-in catalog (data + resolution fn) + Lean
   resolution proof + `p2p templates list`.
2. `RemoteP2pAdmin::add_replicator` filter param + adapter passthrough (against
   #1033 API; compiles once pin bumps).
3. Lean replicator-with-filter dimension + conformance.
4. Reconciler: template resolution → Push (filtered, no subscribe) / Replicate;
   `PeerPairingDesired.template` + resolved filters.
5. Registry offers `templates`; discovery materializes scoped pairings.
6. CLI `--template` front door (retire `--profile` to admin); `admin replicators
   add --filter`; demo + docs (Part 3 update: "pair by intent, not by schema").
7. Integration tests (scoped replication, backup, filter-change reinstall).

## Resolved (2026-06-13)

- **Catalog location:** hardcoded Rust catalog for the prototype (3 templates).
  Document-driven operator-defined templates are the end state; keep the shapes
  clean to migrate later without a surface change. Not now.
- **`agent-config` membership:** includes `AgentBehavior` (so shared config is
  runnable); excludes `AgentPrincipal` (identity is per-deployment).
- **Scope key:** mark existing `agent_did` `@immutable` + denormalize it onto the
  4 session-keyed conversation collections (no `owner_did`); in-place is fine.
- **`CodexThreadProjection`:** excluded from `conversation`; refactor tracked in
  issue #494.

## Open questions (deferred, non-blocking)

- `agent-config` delivery `Replicate` (whole-collection) is assumed; revisit if
  config needs scoped/bidirectional treatment.
- Multi-field scope (`agent_did` AND `behavior_id`) — #1033 filter is single-
  field per collection; defer until a use case needs it.
