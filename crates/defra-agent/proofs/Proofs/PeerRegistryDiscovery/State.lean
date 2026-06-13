import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Card
import Mathlib.Data.Finset.Image

/-!
# Peer Registry Discovery — State

Service-discovery derivation that sits *above* the proven `PairingReconcile`
machine. A replicated `PeerRegistry` (one self-registered row per node) is read
by a discovery step which **materializes registry-owned `PeerPairingDesired`
rows**; the existing pairing reconciler then wires them, unchanged.

## Ownership representation (the deliverable that binds R5's Rust schema)

The desired peer set is a union of **operator-owned** and **registry-owned**
rows. We model ownership as **two separate finsets** (`operatorDesired` /
`registryDesired`) rather than a `source : Owner` tag on each row.

**Why two finsets, not a tag.** Every ownership obligation in this model is a
statement about the *operator partition only* — "no derivation step mutates an
operator-owned row" (`ownership_safe`) and "retracting a registry entry removes
exactly its registry-owned rows, no others" (`retraction_sound`). With two
finsets those are `rfl`-shaped facts: the derivation rewrites `registryDesired`
and never names `operatorDesired`, so operator-invariance is definitional and
discharged by `cases`/`simp`, not by a per-row `filter`/`source = operator`
side-condition that every theorem would have to thread. A `source` tag would
force every ownership lemma to reason about a `Finset.filter (·.source = …)`
projection and to carry "the operator never wrote a row tagged registry"
well-formedness hypotheses — strictly more proof surface for the same content.
The disjoint-union *is* the invariant, so we make it structural.

**Binding consequence for R5.** The Rust schema should keep registry-owned
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

/-- One self-registered registry row. `live` folds the heartbeat-freshness and
`status` hint that the runtime computes into a single observed-liveness bit
(the reader derives effective liveness from `updated_at` age; the model takes
that decision as given). -/
structure RegistryEntry where
  peer : PeerId
  did : Did
  live : Bool
  deriving DecidableEq, Repr

/-- The replicated discovery registry: a set of rows. Keyed conceptually on
`peer`; the model does not assume uniqueness, and the derivation is correct
regardless (it is a pure image-filter over the set). -/
abbrev Registry := Finset RegistryEntry

/-- Registry-owned desired peer set = live entries that are not self.

This is the pure derivation `registry → desiredₘ`. It is a function of the
registry alone, which is what makes convergence immediate (see `Derivation`). -/
def deriveRegistryDesired (self : PeerId) (reg : Registry) : Finset PeerId :=
  (reg.filter (fun e => e.live = true ∧ e.peer ≠ self)).image RegistryEntry.peer

/-- Full discovery state. Ownership is a *structural partition*: operator-owned
desired rows and registry-owned desired rows are distinct finsets. The discovery
step only ever rewrites `registryDesired`. -/
structure DiscoveryState where
  self : PeerId
  registry : Registry
  /-- Operator-authored desired peers. The discovery step NEVER touches these. -/
  operatorDesired : Finset PeerId
  /-- Registry-derived desired peers, owned by the discovery step. -/
  registryDesired : Finset PeerId
  deriving DecidableEq

namespace DiscoveryState

/-- The effective desired set fed to `PairingReconcile` is the union of the two
ownership partitions. -/
def effectiveDesired (s : DiscoveryState) : Finset PeerId :=
  s.operatorDesired ∪ s.registryDesired

/-- A discovery state is *settled* when its registry-owned rows already equal
the derivation of its registry. (Operator rows are out of scope: the discovery
step has no opinion on them.) -/
def settled (s : DiscoveryState) : Prop :=
  s.registryDesired = deriveRegistryDesired s.self s.registry

instance (s : DiscoveryState) : Decidable s.settled := by
  unfold settled
  infer_instance

/-- Canonical settled state: run the derivation once. -/
def settle (s : DiscoveryState) : DiscoveryState :=
  { s with registryDesired := deriveRegistryDesired s.self s.registry }

theorem settle_settled (s : DiscoveryState) : (settle s).settled := by
  unfold settled settle
  rfl

/-- Settling preserves the operator partition. -/
theorem settle_preserves_operator (s : DiscoveryState) :
    (settle s).operatorDesired = s.operatorDesired := rfl

end DiscoveryState

end PeerRegistryDiscovery
