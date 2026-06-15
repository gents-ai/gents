# Peer registry + signed invites — service discovery and membership

Date: 2026-06-13
Branch: `cli-normalization` (extends the pairing work in
`2026-06-12-cli-normalization-p2p-pairing-design.md`)
Status: draft for review

## Goal

Turn pairwise pairing into a **network**. Today `p2p pairings invite/join`
pairs exactly two nodes; joining one node tells you nothing about the others.
This adds a replicated **service-discovery registry** so that joining one
member lets a node discover and pair with the whole network, and a
**signed-invite authorization** so that membership is gated by a member's
cryptographic say-so rather than mere reachability.

This composes with, and does not replace, the proven pairing reconciler. The
registry is a new *source of desired pairings*; the existing
`PeerPairingDesired` → reconcile → `PeerPairingApplied` machinery does the
actual wiring, unchanged.

## Three layers, kept distinct

The whole point of this design is to not conflate three things:

1. **Discovery** — *who is out there?* The `PeerRegistry` collection. Makes
   peers findable. This spec.
2. **Replication** — *move the documents.* `PeerPairingDesired` +
   reconciler. Already built.
3. **Authorization** — *who may join / who may delegate?*
   - *Join:* a **signed invite** from an existing member (this spec). The
     signature is the credential.
   - *Delegate:* `subagent_allow_cross_deployment` on the tool selection
     (already built and enforced at dispatch, `background_tools.rs`).

Discovery makes a peer *visible*; it does not make it *authorized*. ACP still
gates document reads at merge; the delegation gate still gates subagents. The
signed invite gates *entry into the registry's trust set*. Wire-enforced
admission and two-sided revocation remain deferred to upstream
defradb.rs#1012 (admin channel) / #180 (NAC); filtered replication remains
deferred to the upstream #1013 work. None of those block this design.

## `PeerRegistry` — the discovery collection

A new collection, **one row per node, self-registered** (a node writes only
its own row, keyed by `peer_id`):

```graphql
type PeerRegistry {
    peer_id: String @index(unique: true)
    agent_did: String @index
    addresses: [String!]!          # shareable tickets / multiaddrs
    profiles: [String!]            # collection profiles this node offers (nillable → null when empty)
    display_name: String
    status: String                 # "online" | "offline"
    invited_by: String             # agent_did of the member whose signed invite admitted this node
    registered_at: DateTime @index(direction: DESC)
    updated_at: DateTime @index(direction: DESC)   # heartbeat
}
```

- **Self-registration.** Each node writes/updates *its own* row at startup and
  refreshes `updated_at` on a heartbeat interval. Keying on `peer_id` and
  writing only your own row keeps ownership trivial without needing ACP
  enforcement we don't yet have. (When upstream NAC lands, an ACP policy can
  enforce "a node may only write the row whose `peer_id`/`agent_did` is its
  own" — noted, not required now.)
- **Liveness.** A reader treats a row whose `updated_at` is older than
  `REGISTRY_STALE_AFTER` (proposed 3× heartbeat) as `offline`. The node sets
  its own `status` and the reader derives effective liveness from the
  heartbeat age — the field is a hint, the timestamp is the truth.
- **`invited_by`** records the admitting member's DID — an audit trail of how
  each node entered the trust set, and the hook the discovery policy uses
  (below).

The registry travels in a small bootstrap profile (a new `discovery` profile
containing `PeerRegistry`, plus the `agent` profile's config collections so a
joiner can actually run what it discovers). When a node pairs with a seed via
a signed invite, that first pairing replicates `PeerRegistry`, and the node
now sees every member the seed knows.

## Signed invites — authorization by member signature

The invite token (`InviteToken`, today in the CLI; **moves to
`defra-agent-protocol`** as shared vocabulary) gains a signature.

```
InviteToken v2 {
    v: 2,
    issuer_did: String,     # the member minting the invite
    peer_id, ticket, profiles,   # (as today)
    network_id: String,     # optional: which network this invite admits to
    issued_at: String,
    sig: bytes,             # issuer's signature over the canonical payload
}
```

- **Mint.** `p2p pairings invite` signs the canonical-encoded payload with the
  local principal identity (`identity.rs` `sign`). The issuer is, by
  construction, a member.
- **Verify on join.** `p2p pairings join` verifies `sig` against
  `issuer_did`. Because `did:key` embeds the public key, verification needs
  only the token — no lookup. Authorization rule:
  - **Bootstrap / first join:** trust-on-first-use. The operator handed you
    this token out-of-band; a valid signature over a well-formed token is
    accepted, and `issuer_did` is recorded as `invited_by`. (You have no
    registry yet to check against.)
  - **Subsequent invites:** verify the signature **and** that `issuer_did` is
    a live member in the replicated `PeerRegistry`. A signature from a
    non-member (or evicted member) is rejected.
- **Why this is enough for now.** It moves authorization from "can reach the
  address" to "holds a credential signed by a member." It needs no wire admin
  channel — pure DID crypto over a document we already replicate. Revocation
  is still one-sided (a node can stop honoring a DID and drop its registry
  row / pairings) until upstream #1012 gives two-sided eviction.

Old `v: 1` tokens are rejected with a clear "re-issue with a newer
defra-agent" error (the v2 type adds a required signature; an unsigned invite
is no longer valid).

## Discovery reconciler — registry → desired pairings

A discovery step reads `PeerRegistry` and **materializes `PeerPairingDesired`
rows** for discovered peers, then the existing pairing reconciler wires them.
The discovery step never touches live P2P state directly — it only writes
desired documents, exactly like the operator does.

- **Policy (opt-in).** Off by default. When `discovery_auto_pair` is enabled
  (a runtime setting), the reconciler creates a desired row for each live
  registry member that (a) is not self and (b) passes the trust check
  (`invited_by` chains to a member, or a configured DID allowlist). With it
  off, `p2p network list` shows discovered peers and the operator/agent pairs
  explicitly.
- **Ownership, reused.** Registry-derived desired rows are marked
  **registry-owned** (a `source` discriminator on `PeerPairingDesired`, or a
  parallel applied-style record — decide at impl), mirroring the
  `PeerPairingApplied` ownership pattern. Consequences:
  - A registry entry going stale/removed retracts **only** its derived desired
    rows (and the pairing reconciler then tears down only what *it* applied) —
    never the operator's manual pairings.
  - The discovery step never overwrites or deletes an operator-authored row
    for the same peer; operator intent wins.
- **Properties to prove (Lean).** The derivation `registry → desiredₘ` is:
  idempotent (deriving twice = once), convergent (a stable registry yields a
  stable derived set), ownership-safe (derived ops never mutate operator-owned
  rows), and retraction-sound (removing/staling an entry removes exactly its
  derived rows). This is a pure function over registry state feeding the
  already-proven `PairingReconcile` model — it sits *above* it, same way the
  reconciler sits above the admin API.

## The headline use case, end to end

> An agent is spawned, joins its home network, and delegates to a peer agent
> via subagents.

1. A network member mints a signed invite; the new node `join`s it (TOFU,
   signature valid) → pairs with the seed → replicates `PeerRegistry` + config.
2. The new node self-registers into `PeerRegistry` (heartbeating).
3. Discovery (auto or explicit) materializes desired rows for the members it
   needs; the pairing reconciler wires them bidirectionally (reciprocal
   pairing as designed — the parent reads the child's
   requests/tool-calls, the child reads the parent's responses/messages).
4. A cross-deployment subagent call succeeds **iff**
   `subagent_allow_cross_deployment` is set on both behaviors' tool selections
   — replication moved the documents; the gate authorized the delegation.

## CLI surface

Under the declarative noun, mirroring `pairings`:

```
p2p network register     # self-register / refresh this node's PeerRegistry row
p2p network list         # discovered members + liveness (table/json)
p2p network rm           # remove this node's own row (deregister)
p2p pairings invite      # now mints a SIGNED v2 token
p2p pairings join <tok>  # now VERIFIES the signature (TOFU first, registry-checked after)
```

`register` is mostly automatic (the runtime self-registers at startup); the
explicit command is for manual refresh, naming, and offered-profile edits.
Auto-pair is a runtime/config setting surfaced in `network list` output
(shows whether a discovered peer is paired / would be auto-paired).

## Lean obligations

- **New model** `Proofs/PeerRegistryDiscovery/` (or a module beside
  `PairingReconcile`): registry state → derived desired set, with the four
  properties above. Feeds, and is composed with, the existing
  `PairingReconcile` (whose desired set becomes operator-owned ∪
  registry-owned).
- **Signature guard** is modeled as a precondition on the join transition:
  admission requires a valid member signature; the abstract model carries a
  `signedByMember` predicate so the "non-member invite rejected" property is
  fenced, not just prose.
- Conformance-mirrored under `tests/conformance/` per the structure fence;
  then runtime, then CLI.

## Testing

- Lean: discovery derivation properties + signature-guard, zero `sorry`.
- Conformance: registry→desired scenarios (new member appears → derived row;
  member stales → derived row retracts; operator row untouched throughout).
- Integration (the proof it works):
  1. **3-node transitive discovery** — node S seeds; A joins S (signed); B
     joins S (signed); A and B discover each other via the replicated registry
     and auto-pair without a direct invite; a document written on A is visible
     on B.
  2. **Signed-invite authorization** — a join with an unsigned/forged/
     non-member signature is rejected; a valid member signature is accepted and
     recorded as `invited_by`.
  3. **Cross-node delegation** — with pairing live and
     `subagent_allow_cross_deployment` on, a parent on one node delegates to a
     child behavior on another and reads its result; with the gate off, the
     dispatch is refused (proving discovery/replication ≠ authorization).
  4. **Ownership** — a stale registry entry retracts only its derived pairing;
     a manual pairing for the same peer survives.
- Gate with `cargo test -p defra-agent` (full) + CLI suite + `lake build`.

## Scope / deferred

- **In:** `PeerRegistry` + self-registration + heartbeat; signed v2 invites +
  verification; discovery → desired derivation (opt-in auto-pair); token type
  to protocol crate; delegation-gate ergonomics + e2e; Lean model + conformance
  + integration tests; CLI `p2p network *`.
- **Deferred upstream (not blocking):** filtered replication (defradb.rs
  #1013 — the in-progress upstream PR); wire-enforced admission + two-sided
  revocation (defradb.rs #1012 / #180). Signed invites are the authorization
  story until those land.

## Sequencing (clean stacked commits on this branch)

1. `PeerRegistry` schema + migration + self-registration/heartbeat daemon.
2. Token type → `defra-agent-protocol`; v2 signed format + sign on invite +
   verify on join (TOFU).
3. Lean: discovery-derivation model + signature guard; conformance mirror.
4. Discovery reconciler (registry → desired, ownership-safe) in the runtime;
   registry-membership check on join.
5. CLI `p2p network register/list/rm`; auto-pair setting + `list` health.
6. Delegation-gate ergonomics + the 4 integration tests; docs (demo Part 3?
   "join a network").

## Open questions

- `source` discriminator on `PeerPairingDesired` vs a separate
  `PeerPairingDerived` record for registry ownership — which keeps the Lean
  model cleanest? (Lean to lead.)
- `network_id` scoping: single implicit network for the prototype, or
  multi-network from the start? (Proposed: carry the field, enforce single for
  now.)
- Heartbeat interval / stale threshold defaults.
