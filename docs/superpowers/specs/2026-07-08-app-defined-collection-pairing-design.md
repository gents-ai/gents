# Replicate an app-defined collection over a pairing (data-plane replication for custom collections)

Issue: sourcenetwork/defra-agent#657
Follow-up epic: #660 (document-defined scope templates)
Date: 2026-07-08

## Problem

The P2P pairing reconciler replicates the collections fixed by a pairing's scope
**template**, not an arbitrary app-defined collection. There is no supported,
reconcile-driven way for one agent to replicate a *custom* `@branchable`
collection (e.g. `ChangeProposed`) to a paired peer as desired-state config. The
only working path is the manual admin path in
`tests/e2e_triggers/event_trigger_p2p_e2e.rs` (`add_collections` +
`add_replicator`), which is not desired-state / manifest-managed.

The driving use case: an event-driven multi-agent flow where a host agent writes
an app-defined document that must fire an `EventTrigger` on a **paired** agent,
with the whole edge declared in config.

## Verified current behavior

- The reconciler derives `replicator_collections` from `template.collections`
  (`p2p_reconcile/engine.rs:713` for network-control rows, `:813` for data-plane
  rows), keyed off the row's `template`.
- Built-in templates carry no app-defined collection
  (`p2p_reconcile/templates.rs`): `conversation`, `agent-config`, `backup`,
  `discovery`, `network-control`, `subagent-coordinator`, `subagent-host`.
- **`DataPlanePairingDesired` already declares `collections: [String!]!`** in its
  SDL (`crates/defra-agent-schemas/schemas/agent/data_plane_pairing_desired.graphql`)
  and it is already read into `PairingStateRow.collections: Option<Vec<String>>`.
  But `load_desired`'s query (`engine.rs:497`) does **not select it**, and
  `data_plane_desired_from_pairing_row` (`engine.rs:782–827`) **ignores it**,
  deriving the collection set from `template.collections` instead.
- Data-plane rows are gated behind `data_plane_materialized_entry`
  (`engine.rs:509`): the peer must be materializable through the
  network-membership gate (signed `AgentNetwork` + active `NetworkMembership` +
  fresh member-signed `PeerEndpoint`, per `select_materializable_entries`).
- Data-plane rows currently produce `collections: BTreeSet::new()` (no gossip
  subscription) — a replicator-only channel. The subscription set for a
  `Delivery::Replicate` template comes from `replicator_collections`
  (`engine.rs:721–727`), but the data-plane path hardcodes the empty set.

## Chosen approach: (b) honor the existing `collections` field

Honor the `DataPlanePairingDesired.collections` field that already exists but is
silently dropped. This is the smallest, most reversible change and the most
direct expression of "replicate these collections to this peer." The app-defined
collection *name* lives in a **document** (the row) — not in the runtime's
compiled catalog — which is correct for app-owned schema (#435) and consistent
with "the database is the control plane."

Rejected alternatives:

- **(a) app-defined scope template** — would put an app's collection name into
  defra-agent's static `BUILTIN_TEMPLATES`, forcing a recompile per custom
  collection set. Wrong ownership boundary.
- **(c) raw-admin affordance** — bypasses the membership gate; a
  desired-state/security regression. Not the durable answer.

The template catalog remains the extension point for **policy** (scope +
delivery); the **row** supplies the app-specific **collection set**. A fuller
future move — templates themselves loaded from documents — is filed as #660 and
this design is forward-compatible with it (`resolve_template` simply grows a
document source later; the same row keeps working).

## Design

### 1. New `data-plane` policy template

Add one built-in template to `templates.rs`:

```
ScopeTemplate {
    id: "data-plane",
    collections: &[],            // bring-your-own: supplied by the row
    scope: Scope::Unscoped,      // replicate the whole custom collection
    delivery: Delivery::Replicate,
}
```

- `Unscoped` — an app-defined event document (e.g. `ChangeProposed`) is not keyed
  by the peer's DID, so the whole collection syncs (no per-peer filter).
- `Replicate` — the receiver must **observe** the merged doc for its
  `EventTrigger` to fire `created`; `Push` deliberately never subscribes.
- `collections: &[]` — the template carries no fixed set; the row supplies it.
  This relaxes the `all_builtin_templates_have_nonempty_collections` unit
  invariant, which is updated to exempt the `data-plane` "bring-your-own"
  template (its collection set is row-supplied by contract).

A `pub const DATA_PLANE_TEMPLATE: &str = "data-plane";` mirrors the existing
`NETWORK_CONTROL_TEMPLATE` / `SUBAGENT_*_TEMPLATE` constants.

### 2. Read `collections` in `load_desired`

Add `collections` to the `DataPlanePairingDesired` sub-query in
`GraphqlPairingStateStore::load_desired` (`engine.rs:497`) so the row's explicit
set reaches `data_plane_desired_from_pairing_row`.

### 3. Honor `row.collections` in `data_plane_desired_from_pairing_row`

When `row.collections` is **non-empty** (the explicit app-defined set):

- Use it as `replicator_collections` and as the collection list the scope filter
  is built over — instead of `template.collections`.
- For `Delivery::Replicate`, populate the subscription `collections` set from it
  as well (so both nodes `add_collections` + `add_replicator`, mirroring the
  manual `install_one_way_replicator`). For `Delivery::Push`, keep the empty
  subscription set (unchanged).

When `row.collections` is **empty**: current behavior exactly
(`template.collections`, empty subscription set). Fully backward compatible — the
existing network-control / subagent data-plane pairings are provably undisturbed.

The template continues to supply scope + delivery; a blank `template` still
defaults to `conversation` for backward compatibility, but the intended pairing
carries `template: "data-plane"`.

### 4. `@branchable` is the operator's responsibility

DefraDB only P2P-syncs `@branchable` collections. This is enforced at schema
registration time by the app (#435), not by the reconciler. Documented here; the
e2e uses a `@branchable` schema. No runtime gate (keeps the change minimal).

### 5. Membership gate is a precondition, not a bypass

The data-plane row is only honored once the peer is materializable
(`data_plane_materialized_entry`). This is intentional (per the issue): the
pairing requires network membership first. The design does not weaken the gate.
The ordering invariant (subscriber reconciles its `EventTrigger` **before** the
replicator is established, so `seed_seen_docs_for_collection` runs on an empty
collection and the merged doc is a genuine first-observation) is preserved by
establishing membership + trigger reconcile before writing the data-plane row on
the sender.

## Lean / conformance

The change alters *what collection set a data-plane pairing resolves to* — a
model concern. Per the foundation flow (CLAUDE.md), start in the Lean spec:

- `PairingReconcile` already models `ReplicatorId = (address, ReplicatorFilter,
  ReplicatorCollections)` and the merge boundary. Extend the data-plane
  resolution so the effective `ReplicatorCollections` is the explicit per-pairing
  set when present, else the template set. Prove the existing safety/liveness
  properties (idempotence, filter-change-forces-reinstall,
  co-existing-pairing-non-interference) still hold under a row-supplied set.
- Mirror into `tests/conformance/pairing_reconcile.rs`: a data-plane row with an
  explicit `collections` set resolves to that set as `replicator_collections`
  (and, for `Replicate`, as the subscription set), and merges with a co-existing
  control pairing without cross-contaminating filters.
- Zero `sorry`s.

If the resolution is a pure function over already-modeled quantities (explicit
set vs template set), the proof obligation is light; if it is not, that is
information (CLAUDE.md) and we stop to reconsider.

## Acceptance e2e (TDD — written first, must fail first)

New test in `tests/e2e_triggers/` (sibling of `event_trigger_p2p_e2e.rs`),
modeled on `p2p_replicated_doc_fires_event_trigger` but driving replication
through **reconcile**, not manual admin:

1. Two agent nodes A and B, P2P enabled, each running `DefraAgent::run`.
2. Register an app-defined `@branchable` `ChangeProposed` schema on both.
3. Materialize the peers through the real membership gate (signed
   `AgentNetwork` + active `NetworkMembership` + fresh `PeerEndpoint`), so
   `data_plane_materialized_entry` returns `Some` on both — B-on-A and A-on-B.
   (Implementation task: locate/build the in-process materialization path the
   CLI `p2p network create` / `invite` / `join` flow uses.)
4. On B: create a `Task` + `EventTrigger` watching `ChangeProposed` for
   `created`; wait for it to reconcile into the active snapshot (ordering
   invariant — trigger before replicator).
5. **Co-existing control pairing**: establish a subagent (control) pairing on the
   same A↔B peer and assert it stays intact across the data-plane reconcile.
6. Via **desired-state config**, write a `DataPlanePairingDesired` row
   (`template: "data-plane"`, `collections: ["ChangeProposed"]`) on the sender
   (and the reverse authorization row on the receiver); let the reconciler
   establish the replicator. No `add_replicator` call in the test.
7. Write a `ChangeProposed` doc on A; assert it replicates to B and fires B's
   `EventTrigger` as `created` — exactly one `AgentRequest` with the rendered
   prompt and matching trigger lineage, `last_status="fired"`, `fire_count=1`,
   `last_fired_source_doc_id` = the replicated doc id.
8. Assert reconcile idempotence (a second sweep installs nothing new) and the
   control pairing is untouched.

The membership-materialization step is the heavy part of the harness; TDD forces
it out. If an in-process materialization path proves prohibitively large, that is
a signal to reconsider harness shape (e.g. CLI-subprocess like
`cli_p2p_network.rs`) — but the product change (b) is unaffected.

## Scope / non-goals

- **In:** honor `DataPlanePairingDesired.collections`; `data-plane` template;
  Lean + conformance; the reconcile-path e2e.
- **Out (#660):** document-defined scope templates.
- **Out:** `config apply` ownership of `DataPlanePairingDesired` (#607 territory);
  the e2e writes rows directly.
- **Out:** any weakening of the membership gate or a raw-admin bypass (c).

## Reversibility

The change is additive: one new template entry, one query field, one branch in
`data_plane_desired_from_pairing_row` guarded on `row.collections` non-empty.
Reverting restores exact prior behavior; empty-`collections` rows already behave
as today.
