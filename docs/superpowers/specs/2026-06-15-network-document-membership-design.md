# Network-Document Membership — Design (v1)

**Status:** Draft for review · 2026-06-15
**Author:** Jack Zampolin (with Claude)
**Supersedes the pairing-as-primary model from** `2026-06-13-peer-registry-service-discovery-design.md` (that registry becomes a sub-layer here).

## 1. Why this exists

Today defra-agent nodes can pair pairwise over P2P, but there is no *network*: no explicit joining procedure, no membership, no single authority over who is in. The user path is "declare a pairing per peer," which doesn't scale and has no admission story.

This feature introduces a **network document** with explicit membership. The node that creates a network is its **admin**. Other nodes **request to join**; the admin **approves**; once admitted, members **find and replicate with each other automatically** as membership permits. This is what turns a pile of individually-pairable nodes into a **networked agent runtime** — the network path becomes the *primary* flow, and pairwise pairing becomes the bootstrap primitive underneath it.

## 2. Scope and threat model (read this first — it bounds everything)

**v1 is application-level authorization + orchestration on a trusted private network.** It is explicitly **not** a confidentiality boundary. Two hard facts about the pinned DefraDB (`bddfcea5`) force this and define the v1/v2 line:

1. **iroh has no inbound admission hook.** The accept loop accepts any dialer (branches only on ALPN); there is no allowlist/authorizer the application can install. Worse, a *merely-connected* peer can pull document DAGs via DocSync/CAR (the sync authorizer treats "connected" as sufficient), bounded only by merge-time ACP — which the runtime does not enable for pairing collections. So at the iroh layer, *if you can connect, you can read*, and the real perimeter is "don't expose the endpoint / don't hand out your NodeId."
2. **Local ACP will not replicate a policy-bearing collection.** Networked ACP enforcement requires **SourceHub ACP** (the shared policy substrate). "DefraDB enforces membership on the wire" and "we run SourceHub" are the same milestone.

Therefore:

| | v1 (this doc — now, Local ACP, no SourceHub) | v2 (SourceHub sprint) |
|---|---|---|
| Membership authority | App-level: **admin-signed** roster entries, **honored by honest nodes** | DefraDB-enforced **SourceHub ACP** policy (`admin`/`member` relations) |
| Network collections | **No ACP policy** (so they replicate under Local ACP) | SourceHub policy attached |
| Non-member reading data | Possible if they know a NodeId (trusted-network assumption) | Cryptographically prevented at merge/replication |
| Revocation | Honest nodes stop replicating; already-sent bytes persist | Replication cut off; access enforced |
| Inbound iroh gating | None (binding gap) | Pending upstream iroh authorizer (tracked separately) |

The v1 data shapes are designed so the v1→v2 transition changes **who enforces membership**, not the document model.

### Explicit non-goals (v1)

- DefraDB-enforced confidentiality or inbound connection gating.
- Multi-admin *enforcement* (we structure the policy shape for it; we do not build it).
- A cryptographic DID↔NodeId binding at the transport layer (we attest it app-side; the transport cannot enforce it).
- Revocation data-cutoff (bytes already replicated to a peer persist on its disk).

## 3. The two PKIs and how we bind them

A node carries two independent key materials, never cross-checked by DefraDB:

- **iroh transport identity** — Ed25519 `SecretKey` → `NodeId` (the pubkey itself), stored at `<home>/p2p-secret-key`. **Stable across moves**: iroh decouples identity from location; when a node changes networks, only its *addresses* change and discovery (pkarr/DNS) re-resolves the same NodeId. The NodeId changes only on **key rotation** (reinstall, lost key, ephemeral mode).
- **DefraDB DID** (`did:key`) — stored at `<home>/keys/<agent>.key`. The durable, portable identity; signs documents and invites; the ACP principal.

These are not derived from each other and the transport does not attest the DID. v1 binds them **at the application layer**, split by authority and volatility:

- **DID = membership** (durable; admin-granted). The roster keys on the DID.
- **NodeId + address = transport binding** (volatile; member-self-asserted, **DID-signed**). "I, DID X, am currently reachable at NodeId Y / ticket T."

This split is what makes mobility and rotation cheap:
- **Move (address change):** member refreshes its address record (or discovery handles it). No re-auth.
- **NodeId rotation:** member re-asserts NodeId under its DID, signed. No admin re-auth — proving DID control is sufficient to rebind one's own transport coordinates.
- **DID loss:** identity loss → genuine re-admission by the admin (a new DID is a new member).

## 4. Data model

All v1 collections are **policy-free** (Local-ACP-safe) and rely on **document-ownership signatures + app-level trust**. Empty arrays render as `null` (never `[]`); all GraphQL interpolation is escaped.

### `AgentNetwork` — the network document (one row per network)
- `network_id: String @index(unique)` — stable network identifier.
- `admin_did: String` — the creator/admin DID (the authority).
- `display_name: String` (optional), `created_at: String` (RFC3339).
- `default_template: String` — scope template members replicate under (default `conversation`).
- `admin_sig: String` — admin-DID signature over the canonical network record.
Honest nodes treat an `AgentNetwork` row as authoritative only if `admin_sig` verifies against `admin_did`.
**v2:** this collection gains a SourceHub ACP policy with `admin`/`member` relations and `admin manages member`.

### `NetworkMembership` — admin-authored grants (one row per member per network)
- `network_id: String`, `member_did: String` — unique together.
- `status: String` — `active` | `revoked`.
- `granted_at: String`, `revoked_at: String` (optional).
- `admin_sig: String` — admin-DID signature over `(network_id, member_did, status, granted_at|revoked_at)`.
Only entries with a valid `admin_sig` from the network's `admin_did` are honored. Revocation is a signed `status=revoked` update (tombstone), not a delete, so the revocation itself is attributable and replicates.
**v2:** this becomes the `member` ACP relation; `admin_sig` is replaced by the enforced relationship grant.

### `PeerEndpoint` — member-self-asserted transport binding (reshapes today's `PeerRegistry`)
- `did: String @index(unique)` — the member's DID.
- `node_id: String`, `address: String` (shareable ticket).
- `network_id: String` — which network this reachability is for.
- `updated_at: String` — heartbeat freshness (liveness derived from age, as today).
- `binding_sig: String` — **member-DID** signature over `(did, node_id, address, network_id, updated_at)`.
Self-authored; honest nodes honor a binding only if `binding_sig` verifies against `did`. This is the volatile "where to reach DID X right now" record. (Subsumes the old self-asserted `PeerRegistry`; the difference is the DID signature, which closes the H4 "registry rows are unsigned" weakness for the reachability claim.)

### `NetworkJoinRequest` — candidate-authored enrollment requests
- `network_id: String`, `candidate_did: String` — unique together.
- `candidate_node_id: String`, `candidate_address: String`.
- `requested_at: String`, `status: String` — `pending` | `approved` | `denied`.
- `req_sig: String` — **candidate-DID** signature over the request.
Authored by the candidate; the admin reads pending requests and acts on them.

## 5. Flows

### 5.1 Create a network
`p2p network create --network-id prod [--display-name …] [--template conversation]`
→ writes a single `AgentNetwork` row signed by this node's DID (the creator = admin). No network exists until created; there is no implicit registry.

### 5.2 Bootstrap pointer ("network invite")
`p2p network invite` (admin) → emits a **bootstrap pointer**: `(network_id, admin_did, admin_ticket)`, encoded like the existing `dapair1-` token but carrying no per-pair authorization — it is a *pointer to the admin*, signed by the admin DID. Shared out-of-band. (This is the network-level invite that did not exist before; it does not by itself grant membership.)

### 5.3 Enrollment: request → approve → accept
1. **Candidate** `p2p network join <pointer>`:
   - Establishes a **bootstrap pairwise pairing** with the admin from the pointer, using a new `discovery` scope template (§6) so the control-plane collections replicate.
   - Writes a signed `NetworkJoinRequest` (its DID + current NodeId/ticket). The request replicates to the admin over the bootstrap pairing.
   - Waits (or returns; `--wait` blocks until approved).
2. **Admin** `p2p network requests` → sees pending requests; `p2p network approve <did>` (or `deny`):
   - Writes a signed `NetworkMembership(status=active)` for the candidate DID.
   - The candidate's `PeerEndpoint` (from the request) is now an honored reachability record.
3. **Candidate** observes its membership grant replicate down → it is now an authorized member. Its discovery reconciler (§7) materializes pairings to all other live members. Membership replaces the per-pairing signature from here on.

Gate at each step:

| Step | Identity | Proven | Gate |
|---|---|---|---|
| pointer handoff | — | nothing | out-of-band trust (human channel) |
| bootstrap pairing | admin DID sig on pointer + iroh ticket | admin authored pointer | signed-pointer verify + freshness + network_id |
| join request | candidate DID sig | candidate authored request | `req_sig` verify (admin side) |
| approve | admin DID sig | admin admitted candidate | admin-only authorship (app-honored) |
| membership-driven pairing | admin-signed `NetworkMembership` + member-signed `PeerEndpoint` | DID is an admitted member, reachable here | discovery reconciler honors only valid signatures |

### 5.4 Discovery / auto-pair (the "find each other" payoff)
Once a node holds the network's `NetworkMembership` + `PeerEndpoint` records, its discovery reconciler materializes a `PeerPairingDesired` row for **each live, admin-approved member** (verifying admin signature on membership and DID signature on the binding), dialing that member's current NodeId. The existing pairing reconciler wires it. **Within a network you have joined, auto-pair is the default** (not the global default-off env gate that exists today; see §7).

### 5.5 Move / rotation
- **Move:** member updates its `PeerEndpoint.address` (re-signed) or relies on iroh discovery to re-resolve the NodeId. No admin involvement.
- **NodeId rotation:** member writes a new `PeerEndpoint` (new `node_id`, re-signed by its DID). Other members' reconcilers pick up the new binding and re-dial. No re-admission.

### 5.6 Revocation
`p2p network revoke <did>` (admin) → signed `NetworkMembership(status=revoked)`. Replicates; honest reconcilers retract that member's materialized pairings and stop dialing/replicating with it. **Limit (v1):** bytes already replicated to the revoked node persist on its disk, and a revoked node that still knows a member's NodeId can still connect and pull until v2/SourceHub enforcement.

## 6. The `discovery` scope template (closes a known gap)

The control-plane collections (`AgentNetwork`, `NetworkMembership`, `PeerEndpoint`, `NetworkJoinRequest`) plus the agent-config collections must replicate so a joiner can learn and run the network. Today the front door is template-only and **no template carries these collections** (the `discovery` *profile* survives only under `p2p admin`). This feature adds a built-in **`discovery` template** = those control-plane collections + agent-config, `Replicate`/unscoped, used by the bootstrap pairing and network reconcile. This is the clean on-ramp the normalization had cut off.

## 7. Discovery reconciler changes

Extend the existing discovery reconciler (`agent/p2p_reconcile/discovery.rs`):
- Source desired pairings from `NetworkMembership` (admin-signed, status=active) joined with `PeerEndpoint` (member-signed, fresh), instead of the old self-asserted registry rows.
- Verify signatures before materializing (reject unsigned/forged entries — closes H4 for membership).
- Ownership-safe materialization (registry-owned vs operator-owned partition) is unchanged from the existing model.
- **Auto-pair default:** for a network this node has *joined* (it holds an active `NetworkMembership` for itself), auto-pair is on by default. The global `DEFRA_AGENT_DISCOVERY_AUTO_PAIR` escape hatch remains for non-network use.

## 8. CLI surface (network-first)

```
p2p network create   --network-id <id> [--display-name <n>] [--template <t>]   # creates AgentNetwork; caller = admin
p2p network invite                                                              # admin: emit bootstrap pointer
p2p network join     <pointer> [--wait]                                         # candidate: request to join, then auto-pair
p2p network requests [--output table|json]                                      # admin: list pending join requests
p2p network approve  <did>                                                      # admin: admit
p2p network deny     <did>                                                      # admin: reject
p2p network revoke   <did>                                                      # admin: remove member
p2p network list     [--output table|json]                                      # members + liveness + pairing health
p2p network rm                                                                  # leave / delete local network state
```

The pairwise `p2p pairings {set,invite,join,…}` surface stays as the lower-level primitive (and the bootstrap transport for enrollment); the network surface is the primary, documented path.

## 9. Lean-first obligations

This changes legal transitions and invariants, so the model leads (extends `Proofs/PeerRegistryDiscovery/`):
- **Admission:** a `NetworkMembership` is honored iff admin-signed by the network's `admin_did`. Model an `admittedMember` predicate; prove **only admin-signed members are materialized** (non-vacuous; a forged/unsigned grant is rejected).
- **Materialization:** discovery materializes a pairing iff (admin-signed active membership) ∧ (member-signed fresh binding). Ownership-safe (operator partition untouched) — reuse the existing two-finset rep and `ownership_safe`/`retraction_sound`.
- **Revocation soundness:** a signed `status=revoked` retracts exactly that member's materialized pairings, no others (mirror `retraction_sound`).
- **Enrollment:** a join transition requires a candidate-DID-signed request; prove an unsigned/forged request cannot produce a membership grant.
Conformance tests fence each against the Rust reconciler, as the existing PeerRegistryDiscovery model already does.

## 10. Relationship to PR #490 / reuse

- **Reuses:** the proven `PairingReconcile` engine; the discovery reconciler (extended); the v4 signed token crypto (`pairing_token.rs`) — the bootstrap pointer is a variant of it; `did:key` verify in `identity.rs`; the `ConsumedInviteNonce` single-use machinery (applies to join requests/pointers as needed).
- **Reshapes:** `PeerRegistry` → `PeerEndpoint` (now DID-signed reachability); membership authority moves from self-asserted rows to the admin-signed `NetworkMembership`.
- **Adds:** `AgentNetwork`, `NetworkMembership`, `NetworkJoinRequest` collections; the `discovery` template; the `p2p network create/invite/join/requests/approve/deny/revoke` commands.

## 11. v2 boundary (SourceHub)

When SourceHub ACP is wired: attach a SourceHub policy to `AgentNetwork`/control-plane collections (`admin`/`member` relations, `admin manages member`); membership becomes DefraDB-enforced at the replication/merge boundary; revocation cuts off replication; multi-admin becomes a relationship grant. The v1 document model and CLI flow are unchanged — only the *enforcement* layer is swapped in. First v2 task: confirm SourceHub ACP is usable on the then-current DefraDB pin and whether a non-owner `admin` actor can write grants.

## 12. Testing

- **Lean:** the obligations in §9, zero `sorry`, non-vacuous.
- **Conformance:** mirror each Lean property against the reconciler (signature-gated materialization, revocation retraction, ownership safety).
- **CLI integration:** create → invite → join(request) → approve → auto-pair → revoke, asserting the membership/binding documents and the resulting replicator state at each step; signature-forgery rejection; move/rotation re-binding; deny path.
- **Multi-node e2e:** 3-node network (admin S, members A and B): A joins via S, B joins via S, A and B auto-pair transitively once both are members; revoking A retracts A's pairings on B.

## 13. Open questions / sequencing

- **Sequencing (decision needed):** recommend landing PR #490 as-is (it is the verified foundation this builds on) and shipping this as its own PR on top, rather than reshaping #490 — the network feature is Lean-first and multi-component, and folding it in would invalidate #490's verification and mix concerns. The CLI can still *present* `p2p network` as the headline flow without cramming the implementation into #490.
- **Pointer reuse vs new token type:** whether the bootstrap pointer reuses the `dapair1-` v4 token (with a "pointer, not grant" flag) or gets its own type.
- **Single network per node vs multiple:** v1 assumes a node can belong to multiple networks (`network_id` on every record); confirm the CLI defaults (a `--network-id` selector vs a configured default).
