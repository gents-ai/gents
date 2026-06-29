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

### 1. Schema: two denormalized routing fields

- **`AgentToolCall.spawn_target_did: String @index`** — denormalized from
  `SpawnArgs.agent_did` when the bridge is written. Makes the coordinator-owned
  bridge routable to its intended host. Empty/absent for non-spawn tool calls.
  Must be `@immutable` (filter-field requirement from #1033: a doc cannot drift
  in/out of a filter).
- **`PeerPairingDesired.subagent_target_did: String`** (nullable; required when
  `template == "subagent"`) — the delegation **host** DID. Both ends of the
  pairing set it to the *same* value (the host), which is why it cannot reuse
  `agent_did`: that field feeds `paired_peer_dids` trust and the loader filters
  out the local node's own DID, so the host's own row would lose its trust entry.
  Keeping `agent_did` = the peer (for trust) and `subagent_target_did` = the host
  (for the filter) keeps both intact.

### 2. The `subagent` scope template

Extend the catalog (`agent/p2p_reconcile/templates.rs`). The existing model is
`Scope = PeerDid{field} | Unscoped` with a single uniform per-collection field.
`subagent` needs **per-collection** filters keyed to the host DID, so the `Scope`
enum gains a variant that resolves a *map* of (collection → field) against a
supplied host DID rather than the peer DID:

```
Scope::HostScoped {
  // collection -> field that must equal the host DID
  // AgentToolCall      -> "spawn_target_did"
  // AgentRequest, AgentResponse, AgentMessage, AgentSession,
  // AgentConversation, AgentToolResult, CompactionEntry -> "agent_did"
}
```

| id | collections | scope | delivery | filter value |
|---|---|---|---|---|
| `subagent` | AgentToolCall, AgentRequest, AgentResponse, AgentMessage, AgentSession, AgentConversation, AgentToolResult, CompactionEntry | `HostScoped` (per-collection field above) | `Push` | the row's `subagent_target_did` |

`Delivery::Push` (no gossip subscription) is what eliminates the `backup`
authorization/direction errors and the foreign-row leak. `scope_filter` is
extended to take the **host DID** (from `subagent_target_did`) instead of the
peer DID, and to emit the per-collection field map for `HostScoped`.

### 3. Reconciler: bidirectional, same filter value both ends

Both pairing rows (coordinator's `{peer: host, subagent_target_did: host}` and
the host's `{peer: coordinator, subagent_target_did: host}`) resolve to filters
on the **same** host DID. Each side installs its **outbound** filtered replicator
leg (`InstallReplicator`) — coordinator→host carries the bridge, host→coordinator
carries the child docs and the completed bridge — and neither subscribes to
gossip. The result is convergence of exactly the host-scoped delegation set, in
both directions, with no whole-collection exposure. No change to the
`diff.rs`/`engine.rs` op model beyond plumbing the host DID into `scope_filter`.

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
  Write `PeerPairingDesired{ template: "subagent", subagent_target_did: <host> }`
  on both nodes and let the **reconciler** install the replicators. Assert: (a)
  cross-node delegation completes (child runs on host, result projects back to
  parent); (b) **no foreign `AgentRequest` rows** land on the host (the property
  `backup` violated). This converts the test from "proves the runtime" to "proves
  the declarative surface."
- **Unit test**: the `subagent` template resolves to the expected per-collection
  filter map (host-DID-keyed) and the expected replicator legs / no-subscription,
  for a concrete (coordinator, host) pair.

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
- **Both-rows-agree invariant**: correctness depends on both pairing rows carrying
  the same `subagent_target_did`. The join/invite flow that provisions the host's
  symmetric row must set it; a unit/conformance check should fence this.
- **Filter-field availability across the conversation set**: resolved — every
  collection in the host-scoped set (`AgentRequest`, `AgentResponse`,
  `AgentMessage`, `AgentSession`, `AgentConversation`, `AgentToolResult`,
  `CompactionEntry`, and `AgentToolCall`) already declares
  `agent_did: String @index @immutable`, so the `agent_did == host` filter works
  across all of them and the #1033 immutable-filter-field requirement is already
  met. Only the new `AgentToolCall.spawn_target_did` needs the `@index @immutable`
  annotation added.
