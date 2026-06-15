import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Card
import Mathlib.Data.Finset.Image

/-!
# Peer Registry Discovery — State

Service-discovery derivation that sits *above* the proven `PairingReconcile`
machine. Instead of self-asserted registry rows, the desired peer set is derived
from a **signed network-membership** world: a `Network` with an admin DID, a set
of admin-signed `Membership` rows, a global set of member-signed `Endpoint`
announcements, and pending `JoinRequest`s. A discovery step **materializes
network-owned `PeerPairingDesired` rows**; the existing pairing reconciler then
wires them, unchanged.

## Ownership representation (the deliverable that binds R5's Rust schema)

The desired peer set is a union of **operator-owned** and **network-owned** rows.
We model ownership as **two separate finsets** (`operatorDesired` /
`registryDesired`) rather than a `source : Owner` tag on each row. (The name
`registryDesired` is kept to limit churn; it now means "network-derived".)

**Why two finsets, not a tag.** Every ownership obligation in this model is a
statement about the *operator partition only* — "no derivation step mutates an
operator-owned row" (`ownership_safe`) and "retracting a membership removes
exactly its network-owned rows, no others" (`retraction_sound`). With two
finsets those are `rfl`-shaped facts: the derivation rewrites `registryDesired`
and never names `operatorDesired`, so operator-invariance is definitional and
discharged by `cases`/`simp`, not by a per-row `filter`/`source = operator`
side-condition that every theorem would have to thread. A `source` tag would
force every ownership lemma to reason about a `Finset.filter (·.source = …)`
projection and to carry "the operator never wrote a row tagged network"
well-formedness hypotheses — strictly more proof surface for the same content.
The disjoint-union *is* the invariant, so we make it structural.

**Binding consequence for R5.** The Rust schema should keep network-owned
desired rows in a record distinct from operator-authored rows (a parallel
applied-style table / `source` discriminator that is *queried as a partition*),
mirroring the `PeerPairingApplied` ownership pattern — NOT an in-place mutable
flag the discovery step flips on shared rows. The effective desired set handed
to the pairing reconciler is the union; the two origins are never blended in a
way that lets a derivation step touch operator intent.
-/

namespace PeerRegistryDiscovery

abbrev Did := String
abbrev PeerId := String
/-- A single-use invite nonce. Concretely the unique `nonce` field carried by a
v2 signed `InviteToken`; the model only needs to compare equality and track which
ones have been consumed, so a `String` is enough. -/
abbrev Nonce := String

abbrev NetworkId := String

/-- A network, identified by `networkId`, governed by a single admin DID. The
`adminSigValid` bit records whether the admin's self-attestation of the network
record verifies; a network whose admin signature does not verify can authorize
nothing. -/
structure Network where
  networkId : NetworkId
  adminDid  : Did
  adminSigValid : Bool
  deriving DecidableEq, Repr

/-- A membership row: a claim that `memberDid` is a member of `networkId`. It is
**only** authoritative when `signedBy` is this network's admin DID *and*
`adminSigValid` is true — together those two facts mean "signed by this network's
admin". `active` is the admin's revocation bit. A `Membership` is what admits a
DID into a network; nothing else does. -/
structure Membership where
  networkId : NetworkId
  memberDid : Did
  active    : Bool
  signedBy  : Did
  adminSigValid : Bool
  deriving DecidableEq, Repr

/-- An endpoint announcement: node `did` is reachable at `peer`. Endpoints are
**GLOBAL per-node**, not network-scoped — a node announces one address regardless
of how many networks it belongs to. `fresh` is the heartbeat-freshness bit (the
reader derives effective liveness from `updated_at` age; the model takes that
decision as given). `memberSigValid` records that the announcement is signed by
the node itself; an endpoint with an invalid member signature is ignored. -/
structure Endpoint where
  did   : Did
  peer  : PeerId
  fresh : Bool
  memberSigValid : Bool
  deriving DecidableEq, Repr

/-- A pending request to join `networkId`. `reqSigValid` records whether the
candidate's request signature verifies. A `JoinRequest` alone NEVER creates a
membership — it is an input to an admin decision, not an admission. Only the
admin writing a signed `Membership` admits the candidate. -/
structure JoinRequest where
  networkId    : NetworkId
  candidateDid : Did
  reqSigValid  : Bool
  deriving DecidableEq, Repr

/-- Full discovery state. Ownership is a *structural partition*: operator-owned
desired rows and network-owned desired rows are distinct finsets. The discovery
step only ever rewrites `registryDesired`. -/
structure DiscoveryState where
  self : PeerId
  network : Network
  memberships : Finset Membership
  endpoints : Finset Endpoint
  requests : Finset JoinRequest
  /-- Operator-authored desired peers. The discovery step NEVER touches these. -/
  operatorDesired : Finset PeerId
  /-- Network-derived desired peers, owned by the discovery step. (Name kept from
  the prior registry model to limit churn.) -/
  registryDesired : Finset PeerId
  /-- Nonces of invite tokens already redeemed by a join. A join is admitted only
  if its token's nonce is NOT in this set; the join inserts it, so the same token
  cannot be redeemed twice (single-use enforcement — see `replay_rejected`). -/
  consumedNonces : Finset Nonce
  deriving DecidableEq

/-- Well-formedness: at most one membership per `(networkId, memberDid)`. The
admin maintains a single authoritative membership row per (network, member); the
model carries this as an explicit invariant rather than a keyed map. -/
def DiscoveryState.wellFormed (s : DiscoveryState) : Prop :=
  ∀ m₁ ∈ s.memberships, ∀ m₂ ∈ s.memberships,
    m₁.networkId = m₂.networkId → m₁.memberDid = m₂.memberDid → m₁ = m₂

instance (s : DiscoveryState) : Decidable s.wellFormed := by
  unfold DiscoveryState.wellFormed
  infer_instance

/-- Network-owned desired peer set = endpoints that are fresh, self-signed, not
self, and whose announcing DID has an **active, admin-signed membership** in this
network. The membership carries the network scope; the endpoint stays global.

This is the pure derivation `(network, memberships, endpoints) → desiredₘ`. It is
a function of the membership world alone, which is what makes convergence
immediate (see `Derivation`). -/
def deriveMaterializable (s : DiscoveryState) : Finset PeerId :=
  (s.endpoints.filter (fun ep =>
      ep.fresh = true ∧ ep.memberSigValid = true ∧ ep.peer ≠ s.self ∧
      s.network.adminSigValid = true ∧
      (s.memberships.filter (fun m =>
        m.memberDid = ep.did ∧ m.active = true ∧
        m.adminSigValid = true ∧ m.signedBy = s.network.adminDid ∧
        m.networkId = s.network.networkId)).Nonempty)).image Endpoint.peer

namespace DiscoveryState

/-- The effective desired set fed to `PairingReconcile` is the union of the two
ownership partitions. -/
def effectiveDesired (s : DiscoveryState) : Finset PeerId :=
  s.operatorDesired ∪ s.registryDesired

/-- A discovery state is *settled* when its network-owned rows already equal the
derivation of its membership world. (Operator rows are out of scope: the
discovery step has no opinion on them.) -/
def settled (s : DiscoveryState) : Prop :=
  s.registryDesired = deriveMaterializable s

instance (s : DiscoveryState) : Decidable s.settled := by
  unfold settled
  infer_instance

/-- Canonical settled state: run the derivation once. -/
def settle (s : DiscoveryState) : DiscoveryState :=
  { s with registryDesired := deriveMaterializable s }

theorem settle_settled (s : DiscoveryState) : (settle s).settled := by
  unfold settled settle
  rfl

/-- Settling preserves the operator partition. -/
theorem settle_preserves_operator (s : DiscoveryState) :
    (settle s).operatorDesired = s.operatorDesired := rfl

end DiscoveryState

end PeerRegistryDiscovery
