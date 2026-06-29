# Declarative subagent delegation pairing topology — #575

Date: 2026-06-29
Branch: `fix/declarative-pairings-575`
Status: draft for review
Extends: `2026-06-13-scope-templates-filtered-replication-design.md` (the scope-template catalog this adds to)
Related: upstream defradb.rs#1074 (the trust-refresh half, split out below)

## Problem

Fleet config's declarative pairing templates cannot reproduce the P2P topology
that cross-deployment subagent delegation needs. Observed on v0.6.0 with Amy as
coordinator and `workstation-1-host` as target: a background `spawn_subagent`
created an Amy-side bridge row that expired as `no_peer_claimed_spawn`; switching
to `backup` made rows flow but tripped `access denied: peer ... not authorized
for collection ...` and `Dropping GossipSub message outside accepted replication
direction ...` and leaked foreign `AgentRequest` rows onto the target.

The live test `crates/defra-agent/tests/e2e_live/subagent_delegation_live.rs`
*works* — but it hand-wires the topology in harness code (`install_one_way_replicator`
in both directions + `write_pairing` trust rows, all before agent boot) and
therefore exercises **none** of the declarative pairing path. It proves the
runtime mechanism; it tests zero of the control-plane surface that turns a fleet
`PeerPairingDesired` document into replicators.

The two existing templates that *should* express the topology both fail:

- **`conversation`** (`Delivery::Push`, `Scope::PeerDid{agent_did}` filtered to the
  peer's DID): correctly filtered and safe, but the filter keys on the *peer's*
  DID. The subagent **bridge `AgentToolCall` is authored by the coordinator** and
  carries the coordinator's `agent_did`, so the peer-DID filter excludes it; it
  never replicates → the target never materializes the child → `no_peer_claimed_spawn`.
- **`backup`** (`Delivery::Replicate`, `Scope::Unscoped`): drops the filter *and*
  gossip-subscribes the whole collection set both ways → foreign-row leak +
  the authorization/direction errors (those come from the `Replicate` gossip
  subscription, not the filter).

What the live test does is a **third mode** no template encodes: directional,
collection-scoped, no peer-DID push filter, no full gossip subscription.

## Two problems, one of them upstream

1. **Declarative topology** — irreducibly a defra-agent change. defradb.rs
   already exposes filtered `add_replicator` (#1033); what's missing is *our*
   template + routing model. This document.
2. **Trust refresh / restart** — the target needed a service restart before
   pairing trust changes took effect. Root-caused to defradb.rs: `paired_peer_dids`
   reloads live (runtime snapshot), but the `Controlled`-mode replicator-authorization
   gate is rehydrated only at boot, and `get_replicators` reports the *persisted*
   set (`iroh.rs:324`) while `get_collections` reports the *live* set (`iroh.rs:606`),
   so a reconciler's diff comes up empty and never re-issues `add_replicator`.
   Rust-only divergence from Go DefraDB. **Filed upstream as defradb.rs#1074.**
   In scope here: only make the restart **explicit and observable** as a stopgap
   (see §7); drop the stopgap once #1074 lands.

## The bridge is a required two-way crossing (the constraint that shapes everything)

Established by tracing the spawn lifecycle:

- The coordinator writes **only** the bridge `AgentToolCall`
  (`hook/persistence/message_spawn.rs` → `tool_call_lifecycle/transition/native.rs`
  `start_running()`), owned by the coordinator's `agent_did`. Per the #377
  convergence comment in `message_spawn.rs`, the hook deliberately does **not**
  create the child; `SubagentSource` does.
- The **target** materializes the child `AgentRequest` by reacting to that bridge
  row replicating in (`trigger_engine/subagent_source.rs` `next_fire()` →
  `build_intent_for_tool_call_doc`). No bridge on the target → no child → silent
  failure.
- On completion the **target** transitions the bridge `running → completed`
  (`tool_call_lifecycle/transition/bridge.rs` `bridge_complete()`), which must
  replicate **back** to the coordinator for the parent tool call to resolve.

So the docs that must converge are owned by **both** ends:

| Doc | Owner (`agent_did`) | Must reach |
|---|---|---|
| Bridge `AgentToolCall` | coordinator | target (create child) **and** back to coordinator (completion) |
| Child `AgentRequest` | target (host) | coordinator (completion projection) |
| Child `AgentResponse` / `AgentMessage` / `AgentSession` / `AgentConversation` / `AgentToolResult` / `CompactionEntry` | target (host) | coordinator |

A filter of `agent_did == host` carries the host-owned half but **drops the
coordinator-owned bridge**. The bridge's intended target lives only inside
`SpawnArgs.agent_did` in the bridge's `args` JSON — not a filterable top-level
field. That is the crux this design resolves.

## Design

### 0. Lean first

This changes *what crossings are legal* and *what invariants hold*, so it starts
in `crates/defra-agent/proofs/`, then conformance, then Rust (per CLAUDE.md).
The model extends the pairing/template layer to prove:

- **Crossing soundness** — for a `subagent` pairing toward host `T`, the set of
  documents that cross is **exactly** `{ bridge AgentToolCall where spawn_target_did == T }`
  ∪ `{ docs owned by T in the conversation set }`, and nothing else (no foreign
  rows — the property `backup` violates).
- **Single-creator** — at most one node materializes a given child: the unique
  node whose DID equals the spawn's resolved target. Holds under the filter (the
  bridge only reaches `T`) *and* is defended at claim time (§5).

Conformance mirrors the obligation in `tests/conformance/` per the existing
Proofs/ ↔ conformance structure fence.

### 1. Schema: one denormalized routing field

- **`AgentToolCall.spawn_target_did: String @index`** — denormalized from
  `SpawnArgs.agent_did` when the bridge is written. Makes the coordinator-owned
  bridge routable to its intended host. Empty/absent for non-spawn tool calls.
  Must be `@immutable` (filter-field requirement from #1033: a doc cannot drift
  in/out of a filter).

**No new field on `PeerPairingDesired`.** An earlier draft added a
`subagent_target_did` DID column to name the host on both rows. It isn't needed:
a node already has both relevant DIDs — `local_did` (itself) and `agent_did`
(the peer) — and the host is always one of those two. The only thing genuinely
missing from a *symmetric* pairing row is **which role this node plays**
(coordinator or host), and that is captured by the template id (§2), not by a
DID field. The host DID is then *derived*: `peer_did` on the coordinator side,
`local_did` on the host side.

(Why the role can't simply reuse `agent_did`: that field means "the peer's DID"
and feeds `paired_peer_dids` trust, which deliberately drops the local node's own
DID. On the host's row the peer is the coordinator but the host is itself, so
writing the host DID into `agent_did` would both destroy the trust entry and be
discarded by the loader. `agent_did` stays = peer; role lives in the template id.)

### 2. Two directional templates: `subagent-coordinator` and `subagent-host`

Extend the catalog (`agent/p2p_reconcile/templates.rs`). The existing model is
`Scope = PeerDid{field} | Unscoped` with a single uniform per-collection field.
The subagent topology is inherently **directional** (one coordinator, one host),
and the bridge must round-trip while always filtering on the host DID, so the
role is encoded in the **template id** and the `Scope` gains a variant that
resolves a per-collection (collection → field) map against a host DID *derived
from the role*:

One uniform rule, two fields:

```
Scope::HostScoped { host: HostSource }     // host == peer (coordinator) | local (host)
  // AgentToolCall                                  -> "spawn_target_did" == host
  // AgentRequest, AgentResponse, AgentMessage,
  // AgentSession, AgentConversation, AgentToolResult,
  // CompactionEntry                                -> "agent_did" == host
```

| id | role | host DID | delivery | outbound leg carries |
|---|---|---|---|---|
| `subagent-coordinator` | coordinator | `peer_did` | `Push` | `AgentToolCall` where `spawn_target_did == peer` (the bridge → host) |
| `subagent-host` | host | `local_did` | `Push` | everything `agent_did == local` (the child request/response/messages/etc.) **and** `AgentToolCall` where `spawn_target_did == local` (the completed bridge → coordinator) |

The operator pairs the coordinator side with `subagent-coordinator` and the host
side with `subagent-host`. The reconciler reads the role off the template id and
derives the host DID from the two DIDs it already has. `Delivery::Push` (no gossip
subscription) is what eliminates the `backup` authorization/direction errors and
the foreign-row leak.

Why the asymmetry: the **only** coordinator-owned doc the host needs is the
bridge, so the coordinator's outbound leg carries just that (scoped to bridges
for *this* host via `spawn_target_did`, so a coordinator paired with several hosts
doesn't fan every bridge out to all of them). Everything else the host needs it
creates locally; everything the coordinator needs (terminal child, response,
completed bridge) the host pushes back under the single `agent_did == host` rule.

### 3. Reconciler: each role installs its own outbound leg

No shared "filter value" needs to be agreed between the rows — each side computes
the host DID locally from its template id (`peer_did` for `subagent-coordinator`,
`local_did` for `subagent-host`) and installs its **outbound** filtered replicator
leg (`InstallReplicator`), with no gossip subscription. The coordinator→host leg
carries the bridge; the host→coordinator leg carries the child set and the
completed bridge. Both legs filter `AgentToolCall.spawn_target_did` on the same
concrete host DID (peer on one side, local on the other), so the bridge converges
in both directions. No change to the `diff.rs`/`engine.rs` op model beyond
plumbing the role-derived host DID into `scope_filter`.

### 4. Bridge write path

Populate `spawn_target_did` from `SpawnArgs.agent_did` where the bridge row is
created (`tool_call_lifecycle/transition/native.rs` `start_running()`, fed by
`hook/persistence/message_spawn.rs`). This is the only producer; the field is
immutable thereafter.

### 5. Claim hardening (defense in depth)

The trusted-paired-peer path (`subagent_source.rs:510`) currently skips the
single-creator gate (`:637`, which checks `target_owner_did == local_did`) and
unconditionally takes local ownership. With `spawn_target_did` now a top-level
field, add a cheap gate on the trusted path: **only materialize when the spawn's
resolved target equals `local_did`.** The §2 filter already prevents a bridge
from reaching the wrong host, so this is belt-and-suspenders — but it closes the
latent multi-peer double-claim where a coordinator paired with two trusted hosts
could otherwise have a leaked bridge claimed by the wrong one.

### 6. Tests

- **E2E rewrite** (`subagent_delegation_live.rs`): drop `install_one_way_replicator`.
  Write `PeerPairingDesired{ template: "subagent-coordinator" }` on the coordinator
  and `PeerPairingDesired{ template: "subagent-host" }` on the host, and let the
  **reconciler** install the replicators. Assert: (a) cross-node delegation
  completes (child runs on host, result projects back to parent); (b) **no foreign
  `AgentRequest` rows** land on the host (the property `backup` violated). This
  converts the test from "proves the runtime" to "proves the declarative surface."
- **Unit test**: `subagent-coordinator` and `subagent-host` each resolve to the
  expected per-collection filter map (with the host DID derived from the role —
  peer vs. local) and the expected single outbound replicator leg / no
  subscription, for a concrete (coordinator, host) pair.

### 7. Restart stopgap (until defradb.rs#1074)

Until the upstream live-refresh lands, make the restart requirement **explicit
and observable**: when the pairing reconciler applies a `subagent` trust/replicator
change, emit a clear `tracing::warn!` that inbound authorization may not take
effect until the target node restarts (pointing at defradb.rs#1074), and document
it in the pairing runbook/CLI help. No attempt to work around the wire-layer gate
in-tree.

## Out of scope

- The defradb.rs wire-layer live-refresh fix (defradb.rs#1074, upstream).
- Operator-defined / document-driven templates (still hardcoded catalog, per the
  scope-templates design).
- Any change to the non-trusted single-creator delegation path beyond §5.

## Risks / open questions

- **`@immutable` on `spawn_target_did`**: required for the filter field, and the
  bridge's target never changes after creation, so this is consistent — but the
  Lean crossing proof should confirm no transition rewrites it.
- **Role-pairing invariant**: correctness depends on the two rows carrying
  *complementary* roles — one `subagent-coordinator`, one `subagent-host` — for
  the same peer pair. The join/invite flow that provisions the host's row must set
  the host role; a unit/conformance check should fence the pairing (e.g. two
  `subagent-coordinator` rows, or two `subagent-host` rows, is a misconfiguration
  that yields no working channel).
- **Return-leg scope is broad for self-hosting nodes** (deferred): the host's
  outbound leg filters response/message/session/etc. on `agent_did == host`, which
  also carries the host's *own* (non-delegation) docs to the coordinator if the
  host runs its own agents. Exact for a dedicated host (e.g. `workstation-1`).
  Precise per-delegation scoping would require denormalizing a delegation/session
  discriminator onto those collections; out of scope for #575, tracked as a
  follow-up.
- **Filter-field availability across the conversation set**: resolved — every
  collection in the host-scoped set (`AgentRequest`, `AgentResponse`,
  `AgentMessage`, `AgentSession`, `AgentConversation`, `AgentToolResult`,
  `CompactionEntry`, and `AgentToolCall`) already declares
  `agent_did: String @index @immutable`, so the `agent_did == host` filter works
  across all of them and the #1033 immutable-filter-field requirement is already
  met. Only the new `AgentToolCall.spawn_target_did` needs the `@index @immutable`
  annotation added.
