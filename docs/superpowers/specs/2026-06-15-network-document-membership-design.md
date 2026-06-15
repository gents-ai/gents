# Network-Document Membership — Design (v1)

**Status:** Finalized for implementation · 2026-06-15
**Author:** Jack Zampolin (with Claude)

## 1. Why this exists

Today defra-agent nodes can pair pairwise over P2P, but there is no *network*: no explicit joining procedure, no membership, no authority over who is in. The user path is "declare a pairing per peer," which doesn't scale and has no admission story.

This feature introduces a **network document** with explicit membership. The node that creates a network is its **admin**. Other nodes **request to join**; the admin **approves**; once admitted, members **find and replicate with each other automatically** as membership permits. This turns a pile of individually-pairable nodes into a **networked agent runtime** — the network path becomes the *primary* flow, and pairwise pairing becomes the bootstrap primitive underneath it.

**This replaces, it does not extend.** It **supersedes** the current self-asserted peer-registry trust model (`p2p network register/list/rm` over `PeerRegistry` in `cli/args.rs` + `agent/p2p_reconcile/discovery.rs`). The self-asserted `register` row goes away; admin-signed `NetworkMembership` + member-signed `PeerEndpoint` replace it. This is a trust-model replacement, not new commands bolted alongside the old ones.

## 2. Scope and threat model (read this first — it bounds everything)

**v1 is application-level authorization + orchestration on a trusted private network.** It is explicitly **not** a confidentiality boundary. Two hard facts about the pinned DefraDB (`bddfcea5`) force this and define the v1/v2 line:

1. **iroh has no inbound admission hook.** The accept loop accepts any dialer (branches only on ALPN); there is no allowlist/authorizer the application can install. Worse, a *merely-connected* peer can pull document DAGs via DocSync/CAR (the sync authorizer treats "connected" as sufficient), bounded only by merge-time ACP — which the runtime does not enable for pairing collections. So at the iroh layer, *if you can connect, you can read*, and the real perimeter is "don't expose the endpoint / don't hand out your NodeId."
2. **Local ACP will not replicate a policy-bearing collection.** Networked ACP enforcement requires **SourceHub ACP** (the shared policy substrate). "DefraDB enforces membership on the wire" and "we run SourceHub" are the same milestone.

| | v1 (this doc — now, Local ACP, no SourceHub) | v2 (SourceHub sprint) |
|---|---|---|
| Membership authority | App-level: **admin-signed** roster entries, **honored by honest nodes** | DefraDB-enforced **SourceHub ACP** policy (`admin`/`member` relations) |
| Network collections | **No ACP policy** (so they replicate under Local ACP) | SourceHub policy attached |
| Non-member reading data | Possible if they know a NodeId (trusted-network assumption) | Cryptographically prevented at merge/replication |
| Revocation | Honest nodes stop replicating; already-sent bytes persist | Replication cut off; access enforced |
| Inbound iroh gating | None (binding gap) | Pending upstream iroh authorizer (tracked separately) |

The v1 data shapes are designed so the v1→v2 transition changes **who enforces membership**, not the document model.

### Explicit non-goals (v1)

- **Multiple networks per node.** v1 is **one network per node**. `PeerPairingDesired` stays unique by `peer_id`; the reconciler materializes at most one network-owned pairing per peer. Multi-network (keying desired rows by `(network_id, peer_id)`, multiple replicators per peer) is a **fast-follow**, not v1.
- DefraDB-enforced confidentiality or inbound connection gating.
- Multi-admin *enforcement* (we structure the policy shape for it; we do not build it).
- A cryptographic DID↔NodeId binding at the transport layer (we attest it app-side; the transport cannot enforce it).
- Revocation data-cutoff (bytes already replicated to a peer persist on its disk).

## 3. The two PKIs and how we bind them

A node carries two independent key materials, never cross-checked by DefraDB:

- **iroh transport identity** — Ed25519 `SecretKey` → `NodeId` (the pubkey itself), stored at `<home>/p2p-secret-key`. **Stable across moves**: iroh decouples identity from location; when a node changes networks, only its *addresses* change and discovery re-resolves the same NodeId. The NodeId changes only on **key rotation** (reinstall, lost key, ephemeral mode).
- **DefraDB DID** (`did:key`) — stored at `<home>/keys/<agent>.key`. The durable, portable identity; signs documents and pointers; the ACP principal.

These are not derived from each other and the transport does not attest the DID. v1 binds them **at the application layer**, split by authority and volatility:

- **DID = membership** (durable; admin-granted). The roster keys on the DID.
- **NodeId + address = transport binding** (volatile; member-self-asserted, **DID-signed**). "I, DID X, am reachable at NodeId Y / ticket T." This is a **per-node global fact** — the same regardless of network — so it is not network-scoped.

This split makes mobility and rotation cheap:
- **Move (address change):** member refreshes its endpoint (or discovery handles it). No re-auth.
- **NodeId rotation:** member re-asserts NodeId under its DID, signed. No admin re-auth.
- **DID loss:** identity loss → genuine re-admission by the admin (a new DID is a new member).

## 4. Data model

All v1 collections are **policy-free** (Local-ACP-safe) and rely on **document-ownership signatures + app-level trust**. Empty arrays render as `null` (never `[]`); all GraphQL interpolation is escaped. Every signature is over a **canonical CBOR payload** produced by a shared helper in `defra-agent-protocol` (§5) — no ad-hoc string concatenation, so CLI/runtime/tests sign identical bytes.

### `AgentNetwork` — the network document (one row per network)
- `network_id: String @index(unique)` — stable network identifier.
- `admin_did: String` — the creator/admin DID (the authority).
- `display_name: String` (optional), `created_at: String` (RFC3339).
- `default_template: String` — scope template members replicate under (default `conversation`).
- `admin_sig: String` — admin-DID signature over the canonical network record.
Honest nodes treat an `AgentNetwork` row as authoritative only if `admin_sig` verifies against `admin_did`.
**v2:** gains a SourceHub ACP policy with `admin`/`member` relations and `admin manages member`.

### `NetworkMembership` — admin-authored grants (the sole approval authority)
- `network_id: String`, `member_did: String` — **unique together**.
- `status: String` — `active` | `revoked`.
- `granted_at: String`, `revoked_at: String` (optional).
- `admin_sig: String` — admin-DID signature over `(network_id, member_did, status, granted_at|revoked_at)`.
Only entries with a valid `admin_sig` from the network's `admin_did` are honored. **Membership presence is the authority**: a node is admitted iff an active, admin-signed `NetworkMembership` exists for its DID. Revocation is a signed `status=revoked` update (tombstone), not a delete, so revocation is attributable and replicates.
`create` writes the admin's **own** `NetworkMembership(member_did = admin_did, status=active)` (self-signed) so the admin appears in its own roster and auto-pairs as a member.
**v2:** becomes the `member` ACP relation; `admin_sig` is replaced by the enforced grant.

### `PeerEndpoint` — member-self-asserted transport binding (global per node)
- `did: String @index(unique)` — the member's DID. **Unique by `did`; not network-scoped** — a node's reachability is one global fact, identical across networks (same iroh key).
- `node_id: String`, `address: String` (shareable ticket).
- `updated_at: String` — heartbeat freshness (liveness derived from age).
- `binding_sig: String` — **member-DID** signature over `(did, node_id, address, updated_at)`.
Self-authored; honored only if `binding_sig` verifies against `did`. The reconciler looks endpoints up **by did** when walking a network's membership. Subsumes the old self-asserted `PeerRegistry`; the difference is the DID signature (closing the H4 "unsigned registry rows" weakness for the reachability claim).

### `NetworkJoinRequest` — candidate-authored enrollment requests
- `network_id: String`, `candidate_did: String` — unique together.
- `candidate_node_id: String`, `candidate_address: String`.
- `requested_at: String`, `status: String` — `pending` (candidate-set; **informational only**).
- `req_sig: String` — **candidate-DID** signature over the request.
Authored by the candidate. **Authority lives in `NetworkMembership`, not here** — `status` is a UX hint. Approval = the admin writes `NetworkMembership` (candidate observes its membership replicate down). Denial = an admin-signed `NetworkJoinDenial` marker (or request TTL/expiry); the admin never edits the candidate-owned request row (avoids cross-author writes).

### Desired-row ownership
`PeerPairingDesired` gains a `source` discriminator: `operator` (hand-authored) | `network` (materialized from membership, carrying `network_id`). This replaces the internal "registry-owned" marker. v1 keeps the collection **unique by `peer_id`** (single network); the `source`/`network_id` fields record provenance for the ownership-safe diff.

## 5. Protocol primitives (`defra-agent-protocol`)

- **Canonical signing payloads** for `AgentNetwork`, `NetworkMembership`, `PeerEndpoint`, `NetworkJoinRequest`, and the network pointer — CBOR of the record with the `*_sig` field zeroed, mirroring `pairing_token::signing_payload`. One helper per record; CLI, runtime, and conformance all call them.
- **Network pointer token** — a **new token type with its own prefix (`danet1-`)**, distinct from the pairwise `dapair1-`. Carries `(network_id, admin_did, admin_ticket)`, admin-signed. It is a *pointer to the admin*, not a pairing authorization and not a membership grant. Reuses the v4 nonce/freshness machinery as appropriate.

## 6. The `discovery` scope template

The control-plane collections (`AgentNetwork`, `NetworkMembership`, `PeerEndpoint`, `NetworkJoinRequest`) plus agent-config must replicate so a joiner can learn and run the network. No current template carries them (the `discovery` *profile* survives only under `p2p admin`). Add a built-in **`discovery` template** = those control-plane collections + agent-config, `Replicate`/unscoped, used by the bootstrap pairing and network reconcile. Small, early, focused-tests PR. This is the on-ramp the template-only normalization had cut off.

## 7. Runtime / reconciler change

Rework discovery derivation (`agent/p2p_reconcile/discovery.rs`) from:

> `PeerRegistry` live row → `PeerPairingDesired`

to:

> valid `AgentNetwork` + active admin-signed `NetworkMembership` + fresh member-signed `PeerEndpoint` → `PeerPairingDesired` (`source="network"`)

- Verify signatures before materializing (reject unsigned/forged membership and endpoints).
- Ownership-safe materialization (operator vs network partition) is the existing two-finset model, with the marker renamed `registry`→`network`.
- **Auto-pair default:** for the one network this node has *joined* (it holds an active `NetworkMembership` for itself), auto-pair is on by default. The global `DEFRA_AGENT_DISCOVERY_AUTO_PAIR` escape hatch remains for non-network use.

## 8. CLI surface (network-first)

Replaces the current registry-facing `p2p network register/list/rm` with:

```
p2p network create   --network-id <id> [--display-name <n>] [--template <t>]   # creates AgentNetwork + admin's own membership; caller = admin
p2p network invite                                                              # admin: emit danet1- bootstrap pointer
p2p network join     <pointer> [--wait]                                         # candidate: request to join, then auto-pair on approval
p2p network requests [--output table|json]                                      # admin: list pending join requests
p2p network approve  <did>                                                      # admin: write active NetworkMembership
p2p network deny     <did>                                                      # admin: write NetworkJoinDenial
p2p network revoke   <did>                                                      # admin: write NetworkMembership(status=revoked)
p2p network list     [--output table|json]                                      # members + liveness + pairing health
p2p network rm                                                                  # leave: drop local network-owned pairings + stop participating
```

`p2p network rm` is a **local leave** (removes this node's network-owned desired rows, stops heartbeating its endpoint, drops local network state). Admin self-teardown (an adminless network) is out of scope for v1 — `rm` by the admin leaves locally; it does not delete others' replicated membership. `p2p pairings {set,invite,join,…}` stays as the lower-level primitive and the bootstrap transport for enrollment.

## 9. Lean model (model first — `Proofs/PeerRegistryDiscovery/` → network-membership discovery)

Extend/rename the existing discovery model. Model signatures **abstractly as booleans/predicates**, not crypto. Core predicates:
- `validNetwork` — an `AgentNetwork` whose `admin_sig` is valid for its `admin_did`.
- `adminSignedMembership` — a `NetworkMembership` whose `admin_sig` is valid for the network's admin.
- `memberSignedEndpoint` — a `PeerEndpoint` whose `binding_sig` is valid for its `did`.
- `admittedMember` — DID with an active `adminSignedMembership` in a `validNetwork`.
- `materializableEndpoint` — `admittedMember` ∧ fresh `memberSignedEndpoint`.

Prove (each non-vacuous, with witnesses):
1. forged/unsigned membership is **never** materialized;
2. active admin-signed membership + fresh member-signed endpoint **is** materialized;
3. revoked membership retracts **exactly** that member (mirror `retraction_sound`);
4. discovery **never mutates operator-owned** desired rows (reuse the two-finset `ownership_safe`);
5. a forged/unsigned join request **cannot** produce a grant.

Conformance fences each against the Rust reconciler, as the existing model already does.

## 10. Implementation sequencing (PR cuts)

1. **Proof/model only** — the §9 Lean model + conformance fence. (Foundation; first slice.)
2. **Schema + protocol** — SDL for the four collections, canonical CBOR signing helpers in `defra-agent-protocol`, and the `discovery` template (+ focused tests).
3. **CLI `create` / `invite`** — pointer scaffolding (`danet1-` token, `AgentNetwork` + admin membership write).
4. **CLI `join` / `requests` / `approve` / `deny`** — the document enrollment flow.
5. **Runtime** — `PeerEndpoint` heartbeat + the network discovery reconciler (membership+endpoint → `source="network"` desired rows).
6. **`revoke` / `list` / `rm` + multi-node e2e cleanup.**

Each cut is its own PR. The whole stack lands **on top of #490** (which merges first as the verified foundation), not folded into it.

## 11. v2 boundary (SourceHub)

Attach a SourceHub policy to `AgentNetwork`/control-plane collections (`admin`/`member` relations, `admin manages member`); membership becomes DefraDB-enforced at the replication/merge boundary; revocation cuts off replication; multi-admin becomes a relationship grant. The v1 document model and CLI flow are unchanged — only the *enforcement* layer is swapped in. First v2 task: confirm SourceHub ACP is usable on the then-current pin and whether a non-owner `admin` actor can write grants. **Multi-network per node** is a separate fast-follow (orthogonal to SourceHub): key network-owned desired rows by `(network_id, peer_id)` and allow multiple replicators per peer.

## 12. Testing

- **Lean:** §9 obligations, zero `sorry`, non-vacuous.
- **Conformance:** mirror each Lean property against the reconciler (signed-membership/endpoint materialization, revocation retraction, ownership safety, signed join request).
- **CLI integration:** create → invite → join(request) → approve → auto-pair → revoke, asserting the documents and resulting replicator state at each step; signature-forgery rejection; deny path; move/rotation re-binding.
- **Multi-node e2e:** 3-node network (admin S, members A and B): A joins via S, B joins via S, A and B auto-pair transitively once both are members; revoking A retracts A's pairings on B.

## 13. Resolved contract decisions

- **One network per node in v1** (multi-network = fast-follow). `PeerPairingDesired` stays `peer_id`-unique.
- **`PeerEndpoint` unique by `did`**, not network-scoped (reachability is a global per-node fact).
- **`create` writes the admin's own active `NetworkMembership`.**
- **Network pointer is a distinct token type (`danet1-`)**, never reuses `dapair1-`.
- **`NetworkMembership` is the sole approval authority; `NetworkJoinRequest.status` is informational.** Deny = admin-signed `NetworkJoinDenial`, not an edit of the candidate's row.
- **`source="network"`** replaces the internal "registry-owned" marker.

Remaining for the plan, not blocking: exact `NetworkJoinDenial`/TTL shape; admin self-teardown semantics beyond local leave.
