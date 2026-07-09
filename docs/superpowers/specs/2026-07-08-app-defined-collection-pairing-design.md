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

(All `p2p_reconcile/*` and `templates.rs` paths below are under
`crates/defra-agent/src/agent/`; CLI paths under `crates/defra-agent-cli/src/`.)

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
  (`engine.rs:721–727`), but the data-plane path hardcodes the empty set —
  **and `merge_layered_desired` (`engine.rs:902–905`) then unconditionally
  `.collections.clear()`s the data-plane layer** so no data-plane subscription
  ever survives to the diff. Any subscription for a data-plane pairing must
  therefore be re-enabled at *both* the resolver and the merge.
- **Existing writers already populate `DataPlanePairingDesired.collections` with
  template expansion**, not with an app-defined set: `demo/fleet.rs:260`
  (`data_plane_collections_literal` expands `template.collections`) and
  `cli_fleet_delegation_live.rs:2126` (`CONVERSATION_COLLECTIONS`). So a
  *non-empty* `collections` is **not** a reliable signal of an explicit custom
  set — legacy rows are non-empty. The new behavior must be gated on the
  **template id** (`app-collections`), not on non-emptiness.
- `resolve_pairing_template` (`p2p/pairings.rs:173`) accepts any built-in
  template for the generic `PeerPairingDesired` path, which has no way to supply
  row collections. An `app-collections` template must be rejected there.

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

### 1. New `app-collections` policy template

Add one built-in template to `templates.rs`:

```
ScopeTemplate {
    id: "app-collections",
    collections: &[],            // bring-your-own: supplied by the row
    scope: Scope::Unscoped,      // replicate the whole custom collection
    delivery: Delivery::Replicate,
}
```

- `Unscoped` — an app-defined event document (e.g. `ChangeProposed`) is not keyed
  by the peer's DID, so the whole collection syncs (no per-peer filter).
  **Blast-radius note:** `Unscoped` means every data-plane edge for a collection
  replicates *all* of its documents to the peer — in a multi-peer fleet, each
  paired peer gets the whole collection, and `Replicate` also adds a
  node-global subscription. The network-membership gate is the only boundary;
  there is no per-peer document filtering here. Operators wanting peer-scoped
  slices need a future `PeerDid`-scoped data-plane policy (a new template —
  the catalog is the extension point), not this one.
- `Replicate` — the receiver must **observe** the merged doc for its
  `EventTrigger` to fire `created`; `Push` deliberately never subscribes.
- `collections: &[]` — the template carries no fixed set; the row supplies it.
  This relaxes the `all_builtin_templates_have_nonempty_collections` unit
  invariant, which is updated to exempt the `app-collections` "bring-your-own"
  template (its collection set is row-supplied by contract).

A `pub const APP_COLLECTIONS_TEMPLATE: &str = "app-collections";` mirrors the
existing `NETWORK_CONTROL_TEMPLATE` / `SUBAGENT_*_TEMPLATE` constants.

Two `templates.rs` unit tests must move with the new entry:
`all_builtin_templates_have_nonempty_collections` (exempt `app-collections`) and
`builtin_template_count_is_seven` (now eight).

### 2. Read `collections` in `load_desired`

Add `collections` to the `DataPlanePairingDesired` sub-query in
`GraphqlPairingStateStore::load_desired` (`engine.rs:497`) so the row's explicit
set reaches `data_plane_desired_from_pairing_row`.

### 3. Honor `row.collections` in `data_plane_desired_from_pairing_row` — gated on `template == "app-collections"`

The new behavior is gated on the **template id**, not on `collections` being
non-empty (legacy rows are non-empty — see Verified current behavior).

When `template_id == "app-collections"`:

- Use `row.collections`, after trim/dedupe of blanks, as `replicator_collections`
  and as the collection list the scope filter is built over — instead of
  `template.collections` (which is `&[]`).
- Because `app-collections` is `Delivery::Replicate`, populate the subscription
  `collections` set from the same trimmed set, so both nodes `add_collections` +
  `add_replicator` (mirroring the manual `install_one_way_replicator`).
- If the trimmed set is **empty**, **soft-skip the data-plane layer** — log a
  warn and treat the layer as `None`, leaving `base` (the control pairing)
  intact. It is **not** an `anyhow::bail!`. See "Soft-skip" below.

**Soft-skip vs hard-fail (bug fix).** `data_plane_desired_from_pairing_row`
returns `Result<Option<PairingDesired>>`. `load_desired` propagates a hard `Err`
out to `reconcile_peer_tick`, which then returns `desired_read_failed: true` and
**skips every op for the peer — including a co-existing Layer-1 control pairing
that loaded fine** (`engine.rs:61–75`). So a malformed *app* row must never
hard-fail: an empty/blank-only `app-collections` collection set yields `Ok(None)`
(warn + skip this layer only), honoring "must not disturb a co-existing control
pairing." The pre-existing hard-`Err` for a **foreign `agent_did`**
(`engine.rs:772`) is kept as-is — it is a security refusal, not a malformed-input
case — and is the documented residual that still fails the whole peer load
(unchanged behavior; unit-tested so any future softening is deliberate). Note:
`desired_read_failed` is not exposed on RuntimeSnapshot; e2e observes soft-skip
success only via co-existing `PeerPairingApplied` stability.

For every **other** template id: current behavior exactly — `template.collections`
drives, subscription set stays empty. Fully backward compatible; the existing
network-control / subagent / any template-expanded data-plane rows are provably
undisturbed because none of them use the `app-collections` template.

The template continues to supply scope + delivery; a blank `template` still
defaults to `conversation` for backward compatibility. The custom pairing carries
`template: "app-collections"` explicitly.

### 3b. Preserve the data-plane subscription through `merge_layered_desired`

`merge_layered_desired` (`engine.rs:894–919`) currently clears the entire
data-plane subscription set (`desired.collections.clear()`) so conversation docs
never gossip unfiltered. That blanket clear must become conditional: **preserve
the data-plane layer's subscription set only when the layer is an `app-collections`
pairing** (detected via `template_ids.contains("app-collections")`). All other
data-plane layers keep the existing clear. This is the single point that lets an
`InstallCollection("ChangeProposed")` op reach the diff; without it, step 3's
resolver change is inert. A conformance test fences exactly this: an `app-collections`
layer's subscription survives the merge, a network-control-only data-plane layer's
does not.

### 3c. Reject the `app-collections` template on the control-plane path — reconciler AND CLI

The `app-collections` template is meaningless for a `PeerPairingDesired` (control-plane)
row, which has no way to supply row collections. Two layers of guard, because the
CLI is not the only writer:

- **Reconciler (authoritative).** `desired_from_pairing_row` (the base /
  `PeerPairingDesired` path, `engine.rs:671`) also returns
  `Result<Option<PairingDesired>>` and **soft-skips** (warn + `Ok(None)`) when the
  resolved template is `app-collections`. Without this, a raw-GraphQL
  `PeerPairingDesired { template: "app-collections", replicator_addresses: [...] }`
  resolves to empty `replicator_collections` yet `has_wiring()` is true (addresses
  present), so `reconcile_peer_tick` would `connect` and install an
  empty-collection replicator (`engine.rs:77`, `diff.rs:36`). The soft-skip makes
  that row produce no wiring.
- **CLI (defense-in-depth + good error).** `resolve_pairing_template`
  (`p2p/pairings.rs:173`) rejects `app-collections` with a clear "the app-collections template is for
  DataPlanePairingDesired rows only; supply an explicit `--collections` set"
  error, so the operator gets an immediate message instead of a silent skip.

The data-plane writer path (`demo/fleet.rs:234 upsert_data_plane`) currently
expands collections via `data_plane_collections_literal` (template set → `[]` for
`app-collections`). #657 rejects `app-collections` there so a mis-invocation fails
loud rather than writing a blank set that soft-skips. Full `config apply` /
explicit-collections ownership of `DataPlanePairingDesired.collections` is #607;
the e2e writes rows directly and does not exercise fleet upsert.

### 4. `@branchable` is the operator's responsibility

DefraDB only P2P-syncs `@branchable` collections. This is enforced at schema
registration time by the app (#435), not by the reconciler. Documented here; the
e2e uses a `@branchable` schema. No runtime gate (keeps the change minimal).

**Failure mode for an invalid / non-`@branchable` collection name.** The reconciler
does not pre-validate the name. An unknown or non-branchable collection surfaces as
an `add_p2p_collections` / `add_replicator` error inside `apply_op`, which fails
the tick (`engine.rs:113`) and retries next sweep — a sticky, self-repeating warn
until the operator fixes the schema or the row. Acceptable for minimality; a unit
test asserts the error string names the offending collection so the sticky log is
diagnosable. No runtime branchable gate.

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

The change introduces a **safety-relevant exception** to the data-plane
subscription rule ("data-plane layers must not extend the subscription set").
Per the foundation flow (CLAUDE.md), that rule change starts in Lean. Scope is
deliberately narrow:

- **`ScopeTemplates` catalog.** Add `appCollectionsTemplate` (empty collections,
  `Unscoped`, `Replicate`) as the eighth `builtinCatalog` entry, plus
  `appCollections_in_catalog` / empty-collections / unscoped-no-filter theorems.
  Row-supplied collections do **not** belong in the static template — the
  template stays pure policy.
- **`PairingReconcile.Layering` (new pure derivation, not a new machine).** Model
  `clampDataPlane` + `mergeLayered`: drop a data-plane layer's subscriptions
  unless `isAppCollections`; always union replicator collections; prove
  (a) app-collections subscriptions survive, (b) non-app data-plane
  subscriptions clear, (c) control-plane base is preserved. Existing
  PairingReconcile machine proofs (idempotence, filter-change-forces-reinstall,
  …) are untouched — the layering change is additive beside the machine.
- **Not modeled in Lean** (input-validation plumbing below the machine boundary;
  fenced by Rust unit/conformance instead): the *source* of a data-plane
  collection set (row vs template), soft-skip of malformed rows, and control-plane
  rejection of `app-collections`. Those do not change what the reconcile machine
  may do once it has a desired state.
- **Rust fencing:**
  - Conformance (`tests/conformance/pairing_reconcile.rs`): (ii) app-collections
    subscription survives `merge_layered_desired` while network-control data-plane
    still clears; (iii) co-existence with a control base (no filter/subscription
    cross-contamination).
  - In-crate unit tests (`engine.rs` `#[cfg(test)]`): (i) row collections resolve
    as both replicator + subscription sets; (iv) empty/blank-only soft-skips;
    (v) control-plane path soft-skips `app-collections`; plus a documented residual
    that foreign `agent_did` still hard-fails the whole peer load.
- Zero `sorry`s.

## Acceptance e2e (final gate; after engine tasks)

New test in `tests/e2e_triggers/` (sibling of `event_trigger_p2p_e2e.rs`),
modeled on `p2p_replicated_doc_fires_event_trigger` but driving replication
through **reconcile**, not manual admin:

1. Two agent nodes A and B, P2P enabled, **each running `DefraAgent::run`** (both
   need the pairing reconciler — unlike the bare-writer existing P2P trigger e2e).
2. Register an app-defined `@branchable` `ChangeProposed` schema on both.
3. Materialize the peers through the real membership gate (signed
   `AgentNetwork` + active `NetworkMembership` + fresh `PeerEndpoint` written
   **locally on each node** with real P2P `node_id` + listen address), so
   `data_plane_materialized_entry` returns `Some` on both — B-on-A and A-on-B.
4. On B: create a `Task` + `EventTrigger` watching `ChangeProposed` for
   `created`; wait for **document** reconcile into the active snapshot
   (`RuntimeSnapshot.last_reconcile_result == "applied"`) — ordering invariant
   (trigger before replicator).
5. **Co-existing control pairing**: write `PeerPairingDesired` with template
   **`network-control`** (not subagent) on the same A↔B peer; wait until
   `PeerPairingApplied` shows control collections; capture that snapshot.
6. Via **desired-state config**, write a `DataPlanePairingDesired` row on **both**
   nodes — mirroring the manual `install_one_way_replicator`, which does
   `add_collections` + `add_replicator` on both sides. Both rows carry the
   **same** `template: "app-collections"` and the **same**
   `collections: ["ChangeProposed"]`; `agent_did` = local self DID; `peer_id` =
   remote node_id:
   - Both rows are written **after** B's trigger reconciles (step 4).
   - Wait for pairing readiness via **`PeerPairingApplied`** (non-empty
     `replicator_addresses` + `collections` contains `ChangeProposed`) — **not**
     RuntimeSnapshot. Assert control collections from step 5 still present.
   No `add_replicator` / `add_collections` call in the test.
7. Write a `ChangeProposed` doc on A; assert it replicates to B and fires B's
   `EventTrigger` as `created` — exactly one `AgentRequest` with the rendered
   prompt and matching trigger lineage, `last_status="fired"`, `fire_count=1`,
   `last_fired_source_doc_id` = the replicated doc id.
8. Assert pairing idempotence (`PeerPairingApplied` stable across another sweep)
   and the control pairing collections are untouched.
9. **Malformed path** (guards the soft-skip fix): write a `DataPlanePairingDesired`
   row with a blank-only `collections` set `["   "]` and assert the co-existing
   control `PeerPairingApplied` is unchanged (data-plane layer skipped, `base`
   intact). Soft-skip is **not** observable as `desired_read_failed` on
   RuntimeSnapshot — observe applied-state stability only.

The membership-materialization step is the heavy part of the harness. If an
in-process materialization path proves prohibitively large, that is a signal to
reconsider harness shape (e.g. CLI-subprocess like `cli_p2p_network.rs`) — but
the product change (b) is unaffected.

## Scope / non-goals

- **In:** honor `DataPlanePairingDesired.collections`; `app-collections` template;
  Lean + conformance; the reconcile-path e2e.
- **Out (#660):** document-defined scope templates.
- **Out:** `config apply` ownership of `DataPlanePairingDesired` (#607 territory);
  the e2e writes rows directly.
- **Out:** any weakening of the membership gate or a raw-admin bypass (c).

## Reversibility

The change is additive and gated on the `app-collections` template id: one new
template entry, one query field, a `template == "app-collections"` branch in
`data_plane_desired_from_pairing_row` (which now returns `Result<Option<..>>` to
support soft-skip), an `app-collections` soft-skip in `desired_from_pairing_row`, one
conditional in `merge_layered_desired`, and one CLI guard. Reverting restores
exact prior behavior. Because every branch keys on
`template_ids`/`template == "app-collections"` — a template no existing row uses —
rows on all other templates behave byte-for-byte as today, regardless of whether
their `collections` field is populated. (The two resolver functions' signatures
change to `Result<Option<PairingDesired>>`; `load_desired`'s two call sites
flatten accordingly.)
