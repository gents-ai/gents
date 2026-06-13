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

| id | collections | scope | delivery | use case |
|---|---|---|---|---|
| `conversation` | AgentRequest, AgentResponse, AgentMessage, AgentToolCall, AgentToolResult, AgentSession, AgentConversation, CompactionEntry, CodexThreadProjection | `PeerDid{agent_did}` | Push | **default** — a peer agent's conversation slice (subagent delegation) |
| `agent-config` | AgentPrincipal, AgentBehavior, ToolSelection, InferenceBackend, InferenceProfile | Unscoped | Replicate | share config so a node can run a behavior defined elsewhere |
| `backup` | conversation collections | Unscoped | Replicate | a backup server holding ALL agents' logs (admin) |

`conversation` is the default for `pairings invite`/`join`/`set`. `backup` is
the explicit "I want everything" path (and `p2p admin` raw commands remain for
ad-hoc surgery). The catalog is data, not hardcoded call sites — adding a
template is adding a row, not threading a new flag.

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

## Schema: `@immutable` scope keys

Filtering the conversation collections on `agent_did` requires `agent_did` to be
`@immutable` on each. These fields are logically write-once (a document's owning
agent never changes), so the constraint is correct — but it is a real invariant
and a migration concern.

- **Task 0 (longest pole, do first):** audit that `agent_did` is never mutated
  after create on AgentRequest/Response/Message/ToolCall/ToolResult/Session/
  Conversation (grep every writer; the runtime sets it at create and the
  lifecycle never rewrites it). Confirm the same for any other field a built-in
  template scopes on.
- **Migration feasibility:** determine whether `@immutable` can be ADDED to an
  existing field via defradb migration, or whether it requires a collection
  version bump / new field. #1033's `FieldDescription::as_immutable()` +
  `@immutable` SDL is the declaration; the open question is the *migration* of an
  existing collection. If additive migration isn't supported, fall back to: mark
  immutable at collection definition for fresh homes + a documented
  reindex/migration path. **This gates the filtered path and must be settled
  before reconciler work.**

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

0. **`@immutable` audit + migration feasibility** (gating; no #1033 dep) — verify
   `agent_did` write-once; settle how immutability is applied to the conversation
   collections. Mark the fields `@immutable`.
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

## Open questions

- Templates as a hardcoded catalog vs. a `ScopeTemplate` DefraDB collection
  (operator-defined templates, replicated like everything else). Proposed: start
  with a built-in catalog (code), design the collection shape so it can become
  document-driven later without surface change. (Jack's "we'll build and deploy
  more of these" suggests document-driven templates are the end state —
  worth deciding now whether to start there.)
- Whether `agent-config` delivery should be `Replicate` (bidirectional config
  sync) or a scoped push; depends on whether config needs to flow both ways.
- Multi-field scope (e.g. `agent_did` AND `behavior_id`) — #1033 filter is
  single-field per collection; defer multi-field until needed.
