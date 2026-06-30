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

## What must cross, in which direction (the constraint that shapes everything)

Established by tracing the spawn lifecycle and the completion path
(`background_completion.rs:534-610`):

- The coordinator writes the bridge `AgentToolCall`
  (`hook/persistence/message_spawn.rs` → `tool_call_lifecycle/transition/native.rs`
  `start_running()`), owned by the coordinator's `agent_did`. Per the #377
  convergence comment, the hook deliberately does **not** create the child;
  `SubagentSource` does.
- The **host** materializes the child by reacting to the bridge replicating in
  (`subagent_source.rs` `next_fire()` → `build_intent_for_tool_call_doc`). To do
  so it loads the **parent `AgentRequest`** and bails if it is absent
  (`subagent_source.rs:502`; cross-referenced again in `subagent_request.rs:167`).
  So the host needs **both** the bridge **and** the parent request. The existing
  conformance harness encodes this — it replicates `AgentRequest` *and asserts it
  arrives before the bridge* (`r5_cross_deployment.rs:86-103`).
- On completion, projection runs **only on the node that owns the parent** —
  `background_completion.rs:549` returns `NotLocalOwner` otherwise — i.e. on the
  **coordinator**. It reads the child's *replicated-back* terminal row + final
  response (host-owned) and transitions the coordinator's **own local** bridge
  (`bridge_complete`, `:592-604`). **The bridge does not round-trip.**

So the crossing is directional and owner-scoped:

| Direction | Docs | Owner | Why |
|---|---|---|---|
| **coordinator → host** | parent `AgentRequest`; bridge `AgentToolCall` | coordinator | host materializes the child from them |
| **host → coordinator** | child `AgentRequest` (terminal), `AgentResponse`, `AgentMessage`, `AgentSession`, `AgentConversation`, `AgentToolResult`, `CompactionEntry`, and the child's own `AgentToolCall`s | host | coordinator projects completion + reconstructs the child timeline |

Two consequences drive the design:

1. The **return** path is owned entirely by the host → a uniform `agent_did == host`
   filter carries all of it (including the child's own tool calls — there is no
   bridge to special-case, since the bridge stays on the coordinator).
2. The **forward** parent request is owned by the *coordinator*. Filtered
   replication scopes by owner, and the only `agent_did` value that matches the
   parent is the coordinator's own — so `AgentRequest agent_did == coordinator`
   delivers the parent **plus the rest of the coordinator's request slice**. There
   is no immutable single-field discriminator for "the parent of *this*
   delegation" (lineage, not ownership), so this slice is the accepted cost (§2,
   Risks). The bridge itself is still scoped precisely to its host via the
   denormalized `spawn_target_did` (§1), so bridges never misroute.

## Design

### 0. Lean first

This changes *what crossings are legal* and *what invariants hold*, so it starts
in `crates/defra-agent/proofs/`, then conformance, then Rust (per CLAUDE.md).
The model extends the pairing/template layer to prove:

- **Crossing soundness** — for a `subagent` pairing between coordinator `C` and
  host `T`, the set of documents that cross is **exactly**:
  `{ AgentRequest owned by C }` ∪ `{ bridge AgentToolCall where spawn_target_did == T }`
  coordinator→host, plus `{ docs owned by T in the conversation set }`
  host→coordinator — and **no third-party** (≠ C, ≠ T) documents (the
  whole-network leak `backup` produces). The coordinator's request slice crossing
  to the host is intended, not a leak across agents.
- **Single-creator** — at most one node materializes a given child: the unique
  node whose DID equals the spawn's resolved target. Holds because the bridge is
  filtered to `spawn_target_did == T` (it only reaches `T`) *and* is defended at
  claim time (§5).
- **Cancel propagation** — the full delegation *lifecycle*, not just
  materialization + completion, must survive the filters. Cross-deployment cancel
  writes `cancel_cascade_intent_at` onto the coordinator's bridge in the same
  update that terminalizes the call (`bridge.rs:269`); the host's mirror reads the
  replicated bridge and interrupts the host-owned child
  (`cross_deployment_cancel_mirror.rs:127`); the coordinator observes the ack via
  the returned child request (`background_completion.rs:202`). The existing legs
  already carry this — the cancel signal is a *bridge update* that, because
  `spawn_target_did` is `@immutable`, stays inside the forward `spawn_target_did == host`
  filter and re-replicates C→host; the child ack is host-owned and returns under
  `agent_did == host`. The Lean crossing set must therefore include the bridge
  cancel-update (C→host) and the child cancel-ack (host→C), and conformance must
  exercise it (§6) so it cannot regress.

Conformance mirrors the obligation in `tests/conformance/` per the existing
Proofs/ ↔ conformance structure fence.

### 1. Schema: one denormalized routing field

- **`AgentToolCall.spawn_target_did: String @index @immutable`** — denormalized
  from `SpawnArgs.agent_did` when the bridge is written. Makes the coordinator-owned
  bridge routable to its intended host. Empty/absent for non-spawn tool calls.
  `@immutable` is mandatory (filter-field requirement from #1033: a doc cannot
  drift in/out of a filter) — and it is also what lets a *bridge update* (e.g. the
  cancel-cascade signal, §0) stay in-filter and re-replicate to the host.

**No new field on `PeerPairingDesired`.** An earlier draft added a
`subagent_target_did` DID column to name the host on both rows. It isn't needed:
a node already has both relevant DIDs — `local_did` (itself) and `agent_did`
(the peer). The only thing genuinely missing from a *symmetric* pairing row is
**which role this node plays** (coordinator or host), and that is captured by the
template id (§2), not by a DID field. Given the role, every filter value the leg
needs is one of those two DIDs (e.g. the coordinator leg keys its bridge filter
on `peer_did` and its request filter on `local_did`; the host leg keys everything
on `local_did`).

(Why the role can't simply reuse `agent_did`: that field means "the peer's DID"
and feeds `paired_peer_dids` trust, which deliberately drops the local node's own
DID. On the host's row the peer is the coordinator but the host is itself, so
writing the host DID into `agent_did` would both destroy the trust entry and be
discarded by the loader. `agent_did` stays = peer; role lives in the template id.)

### 2. Two directional templates: `subagent-coordinator` and `subagent-host`

Extend the catalog (`agent/p2p_reconcile/templates.rs`). The existing model is
`Scope = PeerDid{field} | Unscoped` with a single uniform per-collection field.
The subagent topology is inherently **directional** (one coordinator, one host)
and **asymmetric** (the two legs carry different collections with different
filters), so the role is encoded in the **template id**, and each side installs
exactly its own outbound leg. The two legs:

```
subagent-coordinator  (installed on the coordinator; peer = host)
  outbound C -> host, Delivery::Push, no subscription:
    AgentRequest    where agent_did      == local_did   (the parent + C's slice)
    AgentToolCall   where spawn_target_did == peer_did   (bridges for THIS host)

subagent-host  (installed on the host; peer = coordinator)
  outbound host -> C, Delivery::Push, no subscription:
    AgentRequest, AgentResponse, AgentMessage, AgentSession,
    AgentConversation, AgentToolResult, CompactionEntry, AgentToolCall
                    where agent_did      == local_did   (everything the host owns)
```

| id | role | installed on | outbound leg carries |
|---|---|---|---|
| `subagent-coordinator` | coordinator | the delegating node | parent + coordinator's `AgentRequest` slice (`agent_did == local`); bridges for this host (`AgentToolCall.spawn_target_did == peer`) |
| `subagent-host` | host | the executing node | the host's entire owned delegation set (`agent_did == local` across the conversation collections **incl. the child's own `AgentToolCall`s**) |

The operator pairs the coordinator side with `subagent-coordinator` and the host
side with `subagent-host`. Each reconciler reads its role off the template id and
installs only its outbound leg; nothing needs to be agreed between the rows beyond
the complementary roles (§4a). `Delivery::Push` (no gossip subscription) is what
eliminates the `backup` authorization/direction errors and the whole-network leak.

Why the asymmetry: the host needs only the **parent request + bridge** to
materialize (everything else it creates locally), and the completion runs on the
coordinator against the child docs the host pushes back — so the bridge never
round-trips and the return leg is the single uniform `agent_did == host` rule.
The one looseness is the forward `AgentRequest agent_did == coordinator` filter,
which carries the coordinator's whole request slice (not just the one parent),
because ownership is the only thing `agent_did` can key on — see Risks.

### 3. Reconciler: each role installs its own outbound leg

Each side computes its filters locally from its template id and installs its
**outbound** filtered replicator leg(s) (`InstallReplicator`), with no gossip
subscription — `subagent-coordinator` installs the C→host leg (two collections),
`subagent-host` installs the host→C leg (the conversation set). The bridge filter
field differs by role (`spawn_target_did` outbound from the coordinator; the host
sends its own docs by `agent_did`), so `scope_filter` is extended to emit a
**per-collection (field, value) map** keyed by role rather than a single uniform
field. No change to the `diff.rs`/`engine.rs` op model beyond that.

### 4. Bridge write path — stamp at the lifecycle layer

Populate `spawn_target_did` from the resolved target DID at the **lifecycle
layer**, not in a single caller — `ToolCallLifecycle::new_subagent(...)` /
`start_running()` (`tool_call_lifecycle/transition/native.rs`) is the common
producer for **every** subagent bridge. There are (at least) two callers: the
normal spawn hook (`hook/persistence/message_spawn.rs`) and **workflow fan-out**
(`hook/persistence/orchestration.rs:690`). Stamping in the lifecycle constructor
guarantees both are covered; the field is immutable thereafter. Tests must cover
**both** producers (normal spawn and workflow fan-out).

### 4a. Role provisioning — expand one operator intent into complementary rows

The two rows must carry complementary roles, and today's surfaces propagate a
single template as-is: invite tokens sign one template that `join` writes directly
(`pairing_token.rs:48`) and registry discovery stamps the offered template
(`discovery.rs:632`). So "set up an A→B delegation" must expand into
`subagent-coordinator` on A and `subagent-host` on B. Design:

- **CLI (primary):** the existing `p2p pairings set --template <id>` flag already
  validates against the catalog (`resolve_pairing_template`), so once
  `subagent-coordinator`/`subagent-host` are catalog entries the operator
  provisions each side explicitly: `--template subagent-coordinator` on the
  coordinator, `--template subagent-host` on the host. **No new `--subagent-role`
  flag** — that would duplicate what `--template` already does (YAGNI).
- **Invite/join:** the invite token already carries a single `template`
  (`pairing_token.rs:48`); **no token-struct change** (and no signed-payload
  version bump). The inviter (coordinator) issues with `--template
  subagent-coordinator`; `join` maps a subagent role to its **complement** before
  writing its row (`subagent-coordinator` → `subagent-host` and vice versa) via a
  `complement_subagent_template()` helper, so the joiner lands the host role. Plain
  (non-subagent) templates pass through unchanged.
- **Registry/discovery — excluded for this cut.** Registry discovery stamps a
  peer's *offered* template verbatim (`discovery.rs:205` `chosen_template` →
  `:667` `upsert_registry_desired_mutation`). That is correct for *symmetric*
  templates (`conversation`, `agent-config`, `discovery`) but **wrong** for the
  directional subagent roles: if peer B advertises `subagent-host`, A must stamp
  the *complement* `subagent-coordinator`, never a verbatim `subagent-host`. Rather
  than build complement-mapping into the registry now, `chosen_template` **skips
  any `subagent-*` offered id** (treats it as unhonorable, like an unknown id), so
  registry never auto-materializes a subagent pairing. Subagent topology is
  provisioned only by the explicit invite/join and CLI paths above. Complement-aware
  registry materialization is a possible future enhancement, explicitly out of
  scope here.
- **Fence (conformance + unit):** a pairing whose two ends are **not**
  complementary (two `subagent-coordinator`, two `subagent-host`, or a `subagent-*`
  paired against a non-subagent template) yields no working channel and must be
  flagged — surfaced as a reconcile-time `tracing::warn!` and asserted in a
  conformance case.

### 5. Claim hardening (defense in depth)

The trusted-paired-peer path (`subagent_source.rs:510`) currently skips the
single-creator gate (`:637`, which checks `target_owner_did == local_did`) and
unconditionally takes local ownership. Add a gate on the trusted path that keys on
the **top-level `spawn_target_did`** — the field the §2 replicator filter actually
trusted to route the bridge here, *not* the `SpawnArgs.agent_did` parsed from the
`args` blob: **only materialize when `spawn_target_did == local_did`**, and
**reject any bridge whose top-level `spawn_target_did` disagrees with its in-`args`
target** (an inconsistent/forged bridge). The §2 filter already prevents a bridge
from reaching the wrong host, so this is defense in depth — but it closes the
latent multi-peer double-claim where a coordinator paired with two trusted hosts
could otherwise have a leaked bridge claimed by the wrong one, and it ensures the
claim decision is made on the same field replication authorized.

### 6. Tests

- **E2E rewrite** (`subagent_delegation_live.rs`): drop `install_one_way_replicator`.
  Write `PeerPairingDesired{ template: "subagent-coordinator" }` on the coordinator
  and `PeerPairingDesired{ template: "subagent-host" }` on the host, and let the
  **reconciler** install the replicators. Assert: (a) cross-node delegation
  completes (child runs on host, result projects back to parent); (b) **no
  third-party rows** land on the host — only the paired coordinator's slice and
  the host's own, never a *different* agent's documents (the whole-network leak
  `backup` produced). This converts the test from "proves the runtime" to "proves
  the declarative surface."
- **Unit test**: `subagent-coordinator` and `subagent-host` each resolve to the
  expected per-collection (field, value) filter map and outbound replicator
  leg(s) / no subscription, for a concrete (coordinator, host) pair — including
  that the coordinator leg is `AgentRequest@agent_did==local` + `AgentToolCall@spawn_target_did==peer`
  and the host leg is the conversation set at `agent_did==local`. Also assert
  `chosen_template` skips a `subagent-*` offered id (registry exclusion, §4a).
- **Cancel-propagation case** (conformance + e2e): after a background cross-deployment
  spawn, cancel the parent and assert the cancel rides the declarative legs —
  the bridge cancel-update reaches the host (interrupting the child) and the child
  cancel-ack returns to the coordinator. Guards the §0 cancel-propagation obligation
  against regression.

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
- **Forward leg carries the coordinator's whole `AgentRequest` slice** (accepted):
  the host receives every request the paired coordinator owns, not just the
  delegation parent, because `agent_did` keys on ownership and there is no
  immutable single-field discriminator for "the parent of *this* delegation."
  Bounded to the one paired coordinator (far narrower than `backup`'s whole-network
  leak) and functionally safe — the host's watcher only claims `agent_did == self`
  requests, so it never runs the coordinator's rows. If this slice ever needs
  tightening, the escape hatch is to denormalize the parent metadata the host
  needs (`parent_agent_did`, `parent_subagent_depth`) onto the bridge and make
  trusted materialization bridge-only — a runtime + Lean contract change, deferred
  under YAGNI.
- **Return-leg scope is broad for self-hosting nodes** (accepted, same shape): the
  host's leg carries everything it owns (`agent_did == host`), which for a
  self-hosting node includes its own non-delegation docs. Exact for a dedicated
  host (e.g. `workstation-1`). Same denormalization escape hatch if needed.
- **Filter-field availability across the conversation set**: resolved — every
  collection in the host-scoped set (`AgentRequest`, `AgentResponse`,
  `AgentMessage`, `AgentSession`, `AgentConversation`, `AgentToolResult`,
  `CompactionEntry`, and `AgentToolCall`) already declares
  `agent_did: String @index @immutable`, so the `agent_did == host` filter works
  across all of them and the #1033 immutable-filter-field requirement is already
  met. Only the new `AgentToolCall.spawn_target_did` needs the `@index @immutable`
  annotation added.
