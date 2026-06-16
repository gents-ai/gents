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
- `deriveNetworkDesired` / `decideMaterializable` + `decideMaterializable_agrees` — the **cut-5 reconciler**; the model header states it fences `agent/p2p_reconcile/discovery.rs`.
- `membership_growth_requires_admin_sig`, `forged_membership_not_admitted`, `unsigned_*_not_materialized` — admission/materialization safety.
- `revoke_*`, `tombstone_characterization`, `deriveNetworkDesired_tombstone_eq_revoke` — revocation as a retained `status=revoked` tombstone (≡ erase for the derived set).

**Model/impl boundary (genesis).** `NetworkState.network` is a single fixed field, and `NetTransition` has no `createNetwork` variant (variants: derive/grant/revoke/joinRequest/endpoint/netOperatorWrite). So the model *assumes* exactly one network and proves everything relative to it; **genesis is a bootstrap act outside the proven transition system**. We fence the singleton operationally (§5) rather than generalizing the model to a set of networks (decided: operational guard + conformance test).

## 4. Resolved decisions

| # | Decision | Choice |
|---|----------|--------|
| D1 | Network model for #511 | Build the real `AgentNetwork` control-plane (cuts 3/4/5) Lean-first, then the e2e. |
| D2 | No-crosswise enforcement | Explicit scoped **star** pairings (coordinator↔each subagent); subagents never pair with each other. Enforced + asserted at **both** replication-topology and authorization layers. (No auto-pair mesh.) |
| D3 | Interval cadence | Make heartbeat/stale/sweep intervals env-overridable (Lean-neutral) so convergence is seconds, not minutes. |
| D4 | `network_id` | The `AgentNetwork` doc's deterministic `_docID` (derived from content incl. `admin_did` + operator name) — human-named, collision-free, admin-bound. |
| D5 | Singleton fence | Operational create-guard + conformance test; Lean single-network assumption documented at the boundary. |
| D6 | Inference | Real DeepSeek endpoint; the e2e is `#[ignore]` + env-gated (like `cli_live.rs`), not default CI. Assertions are structural, not exact-content. |
| D7 | Filtered replication (#363) | Exercised live in v1 via the `conversation` PeerDid/Push template. |
| D8 | Delivery | One PR, whole arc, under #511, on `fleet-discovery-e2e`. |

## 5. The cuts

### Cut 0 — Interval plumbing (Lean-neutral)
Make `REGISTRY_HEARTBEAT_INTERVAL` / `REGISTRY_STALE_AFTER` (`discovery.rs`) and `PAIRING_SWEEP_INTERVAL` (`engine.rs`) env-overridable (e.g. `DEFRA_AGENT_REGISTRY_HEARTBEAT_MS`, `..._STALE_MS`, `..._PAIRING_SWEEP_MS`), defaulting to today's values. Changes no transition, invariant, or provider input → no Lean.

### Cut 3 — Network control-plane CLI (genesis + membership writes)
New `p2p network` subcommands beside `register`/`list`/`rm`:

- **`create --name "<human name>"`** — *genesis, admin-only, singleton.* Writes the single `AgentNetwork` doc: `admin_did` = local identity DID, `display_name` = name, `admin_sig` over the doc (matches Lean `validNetwork`). `network_id` = the resulting `_docID` (D4). **Guard:** refuse if an `AgentNetwork` already exists locally (one per node, mirroring the single-field model); re-run is a no-op/explicit error. Subagent nodes never call this — they receive the doc by replication via the `discovery` template.
- **`grant <member_did>`** — writes a `NetworkMembership` row (`membership_key` = composite of network_id+member_did, `status="active"`, `granted_at`, `admin_sig`); matches Lean `adminSignedMembership`.
- **`revoke <member_did>`** — writes the `status="revoked"` tombstone (row retained, `revoked_at`, `admin_sig`); matches `tombstoneState`.

Conformance: signing payloads validate against `validNetwork`/`adminSignedMembership`; singleton guard tested; revoke produces a tombstone (retained, not deleted).

### Cut 4 — Join admission via the membership arm
Extend the join gate so the admission step consults `NetworkMembership` (`admittedMember`): the invite issuer must be an admin-signed **active** member; `admin_sig` verified; the joiner is recorded as a member. Conformance drives off `forged_membership_not_admitted` + `membership_growth_requires_admin_sig`.

**Open for cut-4 planning (Lean is arbiter):** whether the membership arm *replaces* the registry-liveness arm or *composes* with it (registry liveness as bootstrap until the network doc has replicated). TOFU is still required for the **genesis member** (admin's own network, empty membership set). The Lean `admittedMember` is purely membership-based, so the target end-state is membership-gated; the registry arm's role is the bootstrap/transition question.

### Cut 5 — Reconciler + runtime fence
The pairing/discovery reconciler honors `decideMaterializable`: materialize replicators only for admitted members; a revoke/tombstone **retracts** replication (drop the replicator). This is `deriveNetworkDesired` in Rust, already fenced by the Lean. Runtime fence: a revoked member loses replication on the next sweep.

### Cut 6 — 5-process e2e (the #511 capstone)
New test in `crates/defra-agent-cli/tests/` (e.g. `cli_fleet_delegation_live.rs`), scaling up the existing daemon harness and the 2-daemon `cli_p2p.rs` (which already runs real invite/join incl. replay rejection).

**Harness reuse** (`crates/defra-agent-cli/tests/support/`): `process.rs` (`spawn_server_with_ready_json`, `ServeProcess` kill-on-drop, `run_cli_json`), `ports.rs` (`allocate_port`, `graphql_url`), `waits.rs` (`wait_for_runtime_ready`), `graphql.rs`. Real daemon = `CARGO_BIN_EXE_defra-agent`; readiness via `"status":"serving"` JSON (30s).

**Bring-up & flow:**
1. Spawn 5 daemons (P1 coordinator + P2–P5), Cut-0 fast intervals, each pointed at the DeepSeek endpoint via env/config. Gate the whole test `#[ignore]` + env (D6).
2. **Genesis:** P1 `p2p network create --name "<fleet>"` → `AgentNetwork` doc; capture `network_id`.
3. **Serial join** for each Pi (i=2..5): P1 `grant <did_i>`, P1 `pairings invite` (carries `network_id`), Pi `pairings join <token>` through the real gate (sig → freshness → network-match → **membership admission** → nonce burn → `PeerPairingDesired`). Reciprocal where required.
4. **Scoped star pairings:** P1↔each Pi with the `conversation` (`PeerDid`/`Push`) template; subagents never pair with each other.
5. **Authorization config:** P1 behavior gets 4 remote `SubagentTarget`s + `subagent_allow_cross_deployment=true`; each subagent has none + `false`.
6. **Delegation:** an engineered prompt drives P1 to fan out `spawn_subagent` to ≥2 subagents in parallel; each runs a full cross-node round-trip under real inference.

**Assertions (per stage):**
- *Discovery/membership:* every node `online`+fresh in peers' `PeerRegistry`; `AgentNetwork` + `NetworkMembership` replicate fleet-wide; admission decisions match `admittedMember`; each nonce burned once, replays rejected.
- *Pairing:* `PeerPairingApplied` converges to desired; the **scoped filtered** replicators are present for P1↔Pi (live #363 check); no subagent↔subagent replicator exists.
- *Delegation:* each child `AgentRequest` materializes on its owning node with that subagent's `agent_did` + correct `behavior_id`; lineage stamped (`caused_by_parent_request_id`/`_tool_call_id`, `caused_by_trigger_kind=subagent`).
- *Round-trip:* ≥2 children produce non-empty responses (structural, not exact); terminals replicate back; P1 parent terminates cleanly, no orphaned bridges.
- *No-crosswise:* a subagent's onward `spawn_subagent` is denied at the authorization layer (`background_tools.rs:195-219`: no allowed target / `allow_cross_deployment=false`) **and** no topology path exists for it.

**Flake control:** Cut-0 fast intervals; explicit per-stage convergence polling with bounded deadlines (reuse `wait_for_*` patterns, ~250ms poll); kill-on-drop teardown; structural-only inference assertions. No tolerated flakes (repo standard).

## 6. Risks

- **Real-inference fan-out is the primary flake vector.** Mitigate with a strongly-constrained prompt + tool schema that reliably elicits parallel `spawn_subagent` to ≥2 targets; assert structure (tool calls issued, children materialized, non-empty responses), never exact text.
- **#363 filtered replication still has the upstream DAG-completeness gate** (per triage note) — the scoped-push path may surface edge cases; we exercise it knowingly and will isolate any failure as upstream vs. ours.
- **Cut-4 admission composition** (registry arm vs. membership arm vs. both) is a genuine fork resolved against the Lean during cut-4 planning; getting TOFU-for-genesis vs. membership-gated-for-the-rest right is the crux.
- **Daemon-process timing** (readiness, replication lag) — bounded deadlines + polling, not sleeps.

## 7. Sequencing & delivery

Build order **0 → 3 → 4 → 5 → 6** (each cut Lean-fenced where it touches legal transitions/invariants; cut 0 trivial, cut 6 green only once 3–5 land). Single PR under #511 on `fleet-discovery-e2e`. Gate with the full package suite (`cargo test -p defra-agent`, plus `-p defra-agent-cli`), not `--lib`. Per long-plan review calibration: skip per-task code-quality reviewers, keep spec-compliance checks + one final branch review.

## 8. Deferred / non-goals

- Multi-hop delegation (subagent re-delegating) — explicitly out (depth=1, breadth/fan-out only).
- Generalizing the Lean model to a set of networks (D5: operational fence instead).
- Default-CI hermetic inference — this is a gated live test (D6); a hermetic mock variant is a possible follow-up.
