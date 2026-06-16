# Fleet network-membership arc + 5-process e2e (issue #511)

**Status:** design approved, pre-implementation
**Issue:** sourcenetwork/defra-agent#511
**Branch / worktree:** `fleet-discovery-e2e` (`../defra-agent-fleet-e2e`)
**Builds on:** #490 (runtime-owned pairing, discovery, scope templates, signed invites), #510 (§9 control-plane SDL + Lean), #377 (subagent targets). Closes the validation gap behind closed #107 / #363.

## 1. Goal

Complete the #510 network-membership feature to the point where a **5-process fleet** can be brought up and exercised end to end — **named-network genesis → signed-invite, membership-gated join → scoped star pairing → real-inference fan-out subagent delegation** — across one coordinator + four subagents in five separate OS processes.

#511 was scoped as the e2e (#510 "cut 6"), but the operational chain it asserts depends on #510 cuts 3/4/5, which are not yet built. The control-plane SDL and the §9 Lean model landed; the CLI that *creates* networks, the join path that *gates on membership*, and the reconciler *fence* did not. This arc builds those (Lean-fenced, spec already leads) and caps them with the e2e. **All of it ships as one PR under #511.**

## 2. How the chain works today, and the gap

Three background loops are spawned per agent at startup (`startup.rs:275-309`), sharing the embedded node and child cancel tokens:

- **Heartbeat / self-registration** (`registry.rs:199-252`): on boot a node writes its own `PeerRegistry` row (`peer_id`, `agent_did`, `addresses`, `templates`, `status`, `network_id`, `updated_at`), then refreshes every `REGISTRY_HEARTBEAT_INTERVAL = 30s` (`registry.rs:20`). Liveness is computed by readers, not stored: live iff `status==online` AND `updated_at` within `REGISTRY_STALE_AFTER = 3×30s = 90s` (`discovery.rs:53-76`).
- **Discovery reconciler** (`discovery.rs:414-479`), gated by `DEFRA_AGENT_DISCOVERY_AUTO_PAIR`: a pure projection of the registry — `derive_registry_desired` selects every `{live, non-self}` peer and upserts `source="registry"` rows into `PeerPairingDesired`, strictly partitioned from operator-owned rows. **It only reads the already-replicated registry; it never initiates replication.**
- **Pairing reconciler** (`engine.rs:104-141`): reads `PeerPairingDesired`, connects, diffs desired vs. applied vs. actual (`diff.rs:111-158`), installs/removes replicators per the resolved template (`engine.rs:246-303`), persists `PeerPairingApplied`. Sweep `PAIRING_SWEEP_INTERVAL = 30s` (`engine.rs:21`), also reactive on DefraDB `Update`.

**Signed-invite join** (CLI). `p2p pairings invite` (`invite.rs:24-52`) mints a v4 `InviteToken` (`defra-agent-protocol/src/pairing_token.rs:32-52`): `issuer_did`, `peer_id`, `ticket`, single-use `nonce`, `network_id`, `issued_at`, `template`, Ed25519 `sig` over the CBOR payload. `p2p pairings join` (`join.rs:25-194`) runs a strict gate: decode → signature verify → freshness (`check_freshness`, 1h) → network-match (`enforce_network_match`, `join.rs:386-396`) → **admission** (`decide_join_admission`) → single-use nonce burn (`ConsumedInviteNonce`, with unique-index race backstop) → `write_pairing_desired` → reciprocal token on first join.

**Templates** (`templates.rs:124-149`). `conversation` = `Scope::PeerDid{field:"agent_did"}` + `Delivery::Push` (per-peer filtered push of the 8 conversation collections). `agent-config` = `Unscoped`/`Replicate`. `discovery` = `Unscoped`/`Replicate` over the control plane, **including `AgentNetwork` + `NetworkMembership`** (`templates.rs:112-113`). Scope filters become defradb `ReplicationFilters` via `to_defra_filters` (`embedded_impl.rs:255-268`) — the #363 tie-in — passed to `add_replicator` (`embedded_impl.rs:101-115`).

**The gap.** `p2p network` today is only `register`/`list`/`rm` against `PeerRegistry` (network.rs header; `mod.rs:35-39`) — nothing creates an `AgentNetwork` doc. `decide_join_admission` (`discovery.rs:108-144`) gates purely on the **registry-liveness arm** (its own doc-comment); `AgentNetwork`/`NetworkMembership` exist only as SDL + Lean + as replicated collections in the `discovery` template. The reconciler does not consult membership. And the existing 2-node e2e (`subagent_delegation_live.rs`) is in-process, hand-writes pairings (`write_pairing`), installs replicators directly (`install_one_way_replicator`), and points both nodes at one shared live endpoint — it skips the entire discovery/join/pair chain.

## 3. The Lean already leads

`proofs/Proofs/PeerRegistryDiscovery/NetworkMembership.lean` proves the whole arc; we build Rust to satisfy it (no new theorems anticipated):

- `validNetwork` (admin-signed network), `adminSignedMembership`, `admittedMember` (active + admin-signed) — the **cut-4 admission predicate**.
- `endpointMaterializable` / `memberSignedEndpoint` — materialization is over `NetworkMembership` + a **member-signed `PeerEndpoint`** (`binding_sig`), *not* `PeerRegistry`.
- `deriveNetworkDesired` / `decideMaterializable` + `decideMaterializable_agrees` — the **cut-5 reconciler**; the model header states it fences `agent/p2p_reconcile/discovery.rs`. **It materializes every admitted member with a fresh signed endpoint, except self → a MESH** (`NetworkMembership.lean:148`).
- `membership_growth_requires_admin_sig`, `forged_membership_not_admitted`, `unsigned_*_not_materialized` — admission/materialization safety.
- `revoke_*`, `tombstone_characterization`, `deriveNetworkDesired_tombstone_eq_revoke` — revocation as a retained `status=revoked` tombstone (≡ erase for the derived set).

**Two layers (resolves the star-vs-mesh tension).** The system has a **network-membership substrate** — the raw iroh/P2P network you must join to do anything — and an **application data plane** built on top of it.

- **Layer 1 — network substrate (mesh).** The Lean `deriveNetworkDesired` mesh: network-derived (`source="network"`) `PeerPairingDesired` rows replicate the control plane (`AgentNetwork`/`NetworkMembership`/`PeerEndpoint`/`NetworkJoinRequest`) across every admitted member with a fresh signed endpoint, except self. Faithful to the model. Carries a **narrow `network-control` template** (no agent config — see Cut 5).
- **Layer 2 — data plane (star).** The runtime conversation/delegation pairings live in a **separate, operator-owned desired collection** (e.g. `DataPlanePairingDesired`, name finalized at impl), so they never collide with Layer 1 on the unique `PeerPairingDesired.peer_id`. Operator-owned scoped star (coordinator↔each subagent, `conversation` template).
- **Membership is the master gate over both layers.** Both reconcilers consult the same Lean-fenced `decideMaterializable` predicate before installing *any* replicator: a peer that is not an active admitted member gets no edge on either layer, and a revoke retracts both. The data plane adds only *which* collections on *which* edges (the star); the membership gate is the shared, proven predicate — no new fence invented.

So "no-crosswise" is a Layer-2 + authorization property, never a Layer-1 one: subagents may know each other as members (mesh) but have no conversation/delegation edge and no authorizing subagent target.

**Model/impl boundary (genesis).** `NetworkState.network` is a single fixed field, and `NetTransition` has no `createNetwork` variant (variants: derive/grant/revoke/joinRequest/endpoint/netOperatorWrite). So the model *assumes* exactly one network and proves everything relative to it; **genesis is a bootstrap act outside the proven transition system**. We fence the singleton operationally (§5) rather than generalizing the model to a set of networks (decided: operational guard + conformance test).

## 4. Resolved decisions

| # | Decision | Choice |
|---|----------|--------|
| D1 | Network model for #511 | Build the real `AgentNetwork` control-plane (cuts 3/4/5) Lean-first, then the e2e. |
| D2 | Two layers (star-vs-mesh) | **Layer 1 (network substrate)** = network-derived **mesh** on `PeerPairingDesired` (`deriveNetworkDesired`, `source="network"`), narrow control-plane template. **Layer 2 (data plane)** = operator-owned scoped **star** in a **separate desired collection** (`DataPlanePairingDesired`, name TBD), `conversation` template — avoids the unique-`peer_id` collision. No-crosswise = Layer-2 + authz property. |
| D11 | Membership master gate | Both reconcilers gate every replicator install on the shared Lean-fenced `decideMaterializable` (active admitted member + fresh signed endpoint). Revoke retracts **both** layers. The data-plane star is membership-conditioned; a revoked subagent loses its conversation edge immediately. Reuses the proven predicate — no new fence. |
| D3 | Interval cadence | Make heartbeat/stale/sweep intervals env-overridable (Lean-neutral) so convergence is seconds, not minutes. |
| D4 | `network_id` | An **explicit deterministic value computed before create** (derive from `admin_did` + operator name). It is a **signed field** of `AgentNetwork` (`admin_sig` covers it), so it cannot be the `_docID` (circular); `_docID` is a storage detail. |
| D5 | Singleton fence | Operational create-guard + conformance test; Lean single-network assumption documented at the boundary. |
| D6 | Inference | Real DeepSeek endpoint; the e2e is `#[ignore]` + env-gated (like `cli_live.rs`), not default CI. Assertions are structural, not exact-content. |
| D7 | Filtered replication (#363) | Exercised live in v1 via the `conversation` PeerDid/Push template (data-plane star edges). |
| D8 | Delivery | One PR, whole arc, under #511, on `fleet-discovery-e2e`. |
| D9 | Join bootstrap + token | Bump `InviteToken` to **v5**, adding `grant: MembershipRecord` + `network: NetworkRecord` (both admin-signed, types already in `network_token.rs`). Join verifies both admin_sigs, requires `grant.member_did == local DID`, `grant.status=="active"`, `grant.network_id == network.network_id == token.network_id` — admission from the **signed payload**, not the (non-signature-bound) `PeerRegistry` rows. Control-plane template then backfills durable rows. |
| D10 | Remote spawn mode | Remote `spawn_subagent` must use **background** `await_mode` (`message_spawn.rs:523` rejects remote+foreground). The e2e asserts background bridge/await semantics. |

## 5. The cuts

### Cut 0 — Interval plumbing (Lean-neutral)
Make env-overridable, defaulting to today's values: `REGISTRY_HEARTBEAT_INTERVAL` / `REGISTRY_STALE_AFTER` (`discovery.rs`), `PAIRING_SWEEP_INTERVAL` (`engine.rs`), **and the new `PeerEndpoint` heartbeat/stale intervals** introduced in Cut 5 (since signed-endpoint freshness — not `PeerRegistry` — is cut 5's real liveness input). Reuse a shared interval-config struct so endpoint freshness and registry freshness aren't divergent magic numbers. Changes no transition, invariant, or provider input → no Lean.

### Cut 3 — Network control-plane CLI (genesis + membership writes)
New `p2p network` subcommands beside `register`/`list`/`rm`:

- **`create --name "<human name>"`** — *genesis, admin-only, singleton.* (1) Computes `network_id` deterministically from `admin_did` + name **before signing** (D4). (2) Writes the single `AgentNetwork` doc: `network_id`, `admin_did` = local DID, `display_name` = name, `default_template`, `created_at`, `admin_sig` over those fields (matches Lean `validNetwork`). (3) **Writes the admin's own active `NetworkMembership`** (admin-signed) so the admin is itself an `admittedMember` — without this the cut-4 admission story has no genesis member. (4) Publishes the admin's signed `PeerEndpoint` (`binding_sig`). **Guard:** refuse if an `AgentNetwork` already exists locally (one per node, mirroring the single-field model); re-run is a no-op/explicit error. Subagent nodes never call this — they receive the doc by control-plane replication. Emits a `danet1-` `NetworkPointer` (signed `NetworkRecord`) for distribution.
- **`grant <member_did>`** — writes a `NetworkMembership` row (`membership_key` = network_id+member_did, `status="active"`, `granted_at`, `admin_sig`); matches Lean `adminSignedMembership`.
- **`revoke <member_did>`** — writes the `status="revoked"` tombstone (row retained, `revoked_at`, `admin_sig`); matches `tombstoneState`.

Conformance: signing payloads validate against `validNetwork`/`adminSignedMembership`; singleton guard tested; admin self-membership present after create; revoke produces a tombstone (retained, not deleted).

### Cut 4 — Join admission via the membership arm
Extend the join gate so admission consults `admittedMember` (admin-signed **active** membership). **Bootstrap + token (D9):** the joiner has not yet replicated `AgentNetwork`/`NetworkMembership`, and the local `PeerRegistry` rows are explicitly *not* signature-bound (`join.rs` comment). So bump `InviteToken` → **v5** with `grant: MembershipRecord` + `network: NetworkRecord` (admin-signed forms already in `network_token.rs`). Join verifies both admin_sigs, requires `grant.member_did == local DID` (the joiner is the grantee), `grant.status=="active"`, and `grant.network_id == network.network_id == token.network_id`; only then writes the durable membership and lets the control-plane template backfill. Conformance drives off `forged_membership_not_admitted` + `membership_growth_requires_admin_sig`.

**Open for cut-4 planning (Lean is arbiter):** whether the membership arm *replaces* the registry-liveness arm or *composes* with it. Target end-state is membership-gated (`admittedMember` is purely membership-based); the registry arm degrades to a transitional bootstrap, retired once the signed-grant-in-invite path lands. TOFU remains only for the **genesis member** (admin's own network, before any grant exists).

### Cut 5 — Reconciler + runtime fence (both layers, one gate)
Materialization input is **`NetworkMembership` + member-signed `PeerEndpoint`** (`binding_sig`, Lean `memberSignedEndpoint`), *not* `PeerRegistry`. Pieces:

- **Signed endpoint heartbeat:** publish/refresh the local `PeerEndpoint` (`did`, `node_id`, `address`, `updated_at`, `binding_sig`) — the signed analogue of today's self-asserted `PeerRegistry` row. Uses the Cut-0 endpoint freshness interval.
- **Narrow `network-control` template (NEW):** the existing `discovery` template (`templates.rs:111`) deliberately bundles agent config (`AgentBehavior`/`ToolSelection`/backends/skills) as the *bootstrap on-ramp*. The Layer-1 **mesh** must NOT re-replicate config fleet-wide, so add a narrow template carrying only `AgentNetwork`/`NetworkMembership`/`PeerEndpoint`/`NetworkJoinRequest`. `discovery` remains the config-bearing on-ramp for edges that actually want config (in the e2e each daemon is configured locally, so the mesh stays config-free).
- **Layer 1 — network-derived desired rows:** the reconciler computes `deriveNetworkDesired` and materializes `source="network"` `PeerPairingDesired` rows with the `network-control` template. `decideMaterializable`/`deriveNetworkDesired` in Rust, already Lean-fenced.
- **Layer 2 — data-plane desired collection (NEW):** a separate operator-owned `DataPlanePairingDesired` collection (name TBD) holds the conversation/delegation star rows (own unique `peer_id`, so no Layer-1 collision). Its reconciler installs the `conversation` (PeerDid/Push) replicators for its rows.
- **Membership master gate (D11):** **both** reconcilers call the shared `decideMaterializable(peer)` before installing/keeping any replicator. A non-member (or revoked, via `revoke_*` tombstone) peer is dropped from *both* layers on the next sweep. The data plane is thus membership-conditioned with no separate prune.
- **Per-address install (verify):** defradb replicators are per-address; Layer 1 (unfiltered control collections) and Layer 2 (peer-did-filtered conversation collections) both target the same peer address on coordinator↔subagent edges. **Open verification:** whether defradb accepts two replicators per address distinguished by `(address, filter)` (diff.rs identity) or requires the reconciler to merge into one replicator (union collections + per-collection filter map). The plan picks the merge-or-dual approach after confirming against the pinned `defradb.rs`.
- **Compatibility:** existing discovery writes `source="registry"` (`discovery.rs:47`); cut 5 adds `source="network"` beside it. Plan states precedence/migration so `registry` (transitional), `network` (membership-fenced target), and `operator` (Layer-2) rows never fight over a `peer_id`.

### Cut 6 — 5-process e2e (the #511 capstone)
New test in `crates/defra-agent-cli/tests/` (e.g. `cli_fleet_delegation_live.rs`), scaling up the existing daemon harness and the 2-daemon `cli_p2p.rs` (which already runs real invite/join incl. replay rejection).

**Harness reuse** (`crates/defra-agent-cli/tests/support/`): `process.rs` (`spawn_server_with_ready_json`, `ServeProcess` kill-on-drop, `run_cli_json`), `ports.rs` (`allocate_port`, `graphql_url`), `waits.rs` (`wait_for_runtime_ready`), `graphql.rs`. Real daemon = `CARGO_BIN_EXE_defra-agent`; readiness via `"status":"serving"` JSON (30s).

**Bring-up & flow:**
1. Spawn 5 daemons (P1 coordinator + P2–P5), Cut-0 fast intervals, each pointed at the DeepSeek endpoint via env/config. Gate the whole test `#[ignore]` + env (D6).
2. **Genesis:** P1 `p2p network create --name "<fleet>"` → `AgentNetwork` doc; capture `network_id`.
3. **Serial join** for each Pi (i=2..5): P1 `grant <did_i>`, P1 `pairings invite` (carries `network_id` + the signed grant per D9), Pi `pairings join <token>` through the real gate (sig → freshness → network-match → **membership admission from the signed payload** → nonce burn → `PeerPairingDesired`). Reciprocal where required.
4. **Control-plane mesh forms** (cut 5): network-derived `source="network"` rows materialize across all admitted members; `AgentNetwork`/`NetworkMembership`/`PeerEndpoint` converge fleet-wide.
5. **Data-plane star** (Layer 2, operator-owned `DataPlanePairingDesired`): P1↔each Pi with the `conversation` (`PeerDid`/`Push`) template; **no subagent↔subagent conversation row**. Installs gated by `decideMaterializable` (D11).
6. **Authorization config:** P1 behavior gets 4 remote `SubagentTarget`s + `subagent_allow_cross_deployment=true`; each subagent has none + `false`.
7. **Delegation:** an engineered prompt drives P1 to fan out `spawn_subagent` to ≥2 subagents in parallel, in **background `await_mode`** (D10: remote+foreground is rejected); each runs a full cross-node round-trip under real inference.
8. **Revoke fence (optional stage):** P1 `revoke`s one subagent; assert both its Layer-1 mesh edge **and** Layer-2 conversation edge retract (D11 master gate).

**Assertions (per stage):**
- *Discovery/membership:* every node has a fresh signed `PeerEndpoint`; `AgentNetwork` + `NetworkMembership` + `PeerEndpoint` replicate fleet-wide (control-plane mesh); admission decisions match `admittedMember`; each nonce burned once, replays rejected.
- *Pairing:* `PeerPairingApplied` converges to desired; Layer-1 control-plane edges exist mesh-wide; the **scoped filtered** conversation replicators are present for the P1↔Pi Layer-2 star edges (live #363 check); **no subagent↔subagent _conversation/delegation_ replicator exists** (Layer-1 membership replication being mesh-wide is expected, not a violation).
- *Delegation:* each child `AgentRequest` materializes on its owning node with that subagent's `agent_did` + correct `behavior_id`; lineage stamped (`caused_by_parent_request_id`/`_tool_call_id`, `caused_by_trigger_kind=subagent`).
- *Round-trip:* ≥2 children produce non-empty responses (structural, not exact); **background bridge/await semantics hold**; terminals replicate back; P1 parent terminates cleanly, no orphaned bridges.
- *No-crosswise:* a subagent's onward `spawn_subagent` is denied at the **authorization** layer (`background_tools.rs:195-219`: no allowed target / `allow_cross_deployment=false`) **and** has no conversation/delegation **data-plane** edge to another subagent.

**Flake control:** Cut-0 fast intervals; explicit per-stage convergence polling with bounded deadlines (reuse `wait_for_*` patterns, ~250ms poll); kill-on-drop teardown; structural-only inference assertions. No tolerated flakes (repo standard).

## 6. Risks

- **Real-inference fan-out is the primary flake vector.** Mitigate with a strongly-constrained prompt + tool schema that reliably elicits parallel `spawn_subagent` to ≥2 targets; assert structure (tool calls issued, children materialized, non-empty responses), never exact text.
- **#363 filtered replication still has the upstream DAG-completeness gate** (per triage note) — the scoped-push path may surface edge cases; we exercise it knowingly and will isolate any failure as upstream vs. ours.
- **Cut-4 admission composition** (registry arm vs. membership arm vs. both) is a genuine fork resolved against the Lean during cut-4 planning; getting TOFU-for-genesis vs. membership-gated-for-the-rest right is the crux.
- **Per-address replicator install** (Layer 1 + Layer 2 on the same coordinator↔subagent address). Must confirm against pinned `defradb.rs` whether two replicators per address (distinct `(address, filter)`) are allowed, or whether the reconciler must merge into one replicator (union collections + per-collection filter map). Drives the Layer-2 install seam.
- **New `DataPlanePairingDesired` collection.** Adds a schema + a second reconciliation seam beside `PeerPairingDesired`. Must share the `decideMaterializable` gate (D11) and the diff/install engine, not fork it.
- **Source-partition coexistence.** Cut 5 adds `source="network"` beside legacy `source="registry"` in `discovery.rs` (`operator` Layer-2 rows now live in the separate collection). The plan defines precedence/migration so registry-derived rows degrade cleanly as membership-fenced rows take over.
- **Data-plane membership gate (Lean treatment).** D11 reuses `decideMaterializable` as the gate, so no new theorem is strictly required; but the *invariant* "Layer-2 edges ⊆ admitted members" is new. Decide at cut-5 planning whether to add a short Lean lemma binding the data-plane reconciler to `decideMaterializable` or fence it by conformance only.
- **Signed-endpoint heartbeat** (`PeerEndpoint.binding_sig`) is new signing surface replacing the self-asserted `PeerRegistry` heartbeat as the materialization input — must stay consistent with `memberSignedEndpoint` and not regress the existing registry liveness assertions the e2e still uses for the `online`+fresh checks.
- **Daemon-process timing** (readiness, replication lag) — bounded deadlines + polling, not sleeps.

## 7. Sequencing & delivery

Build order **0 → 3 → 4 → 5 → 6** (each cut Lean-fenced where it touches legal transitions/invariants; cut 0 trivial, cut 6 green only once 3–5 land). Single PR under #511 on `fleet-discovery-e2e`. Gate with the full package suite (`cargo test -p defra-agent`, plus `-p defra-agent-cli`), not `--lib`. Per long-plan review calibration: skip per-task code-quality reviewers, keep spec-compliance checks + one final branch review.

## 8. Deferred / non-goals

- Multi-hop delegation (subagent re-delegating) — explicitly out (depth=1, breadth/fan-out only).
- Generalizing the Lean model to a set of networks (D5: operational fence instead).
- Default-CI hermetic inference — this is a gated live test (D6); a hermetic mock variant is a possible follow-up.
