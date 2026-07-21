import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Card

/-!
# Pairing Reconcile State

Per-peer pairing reconcile state for the defra-agent supervisor. Each tick
reads desired and actual state from a peer, computes a diff, and emits remote
admin calls until actual state matches desired state.
-/

namespace PairingReconcile

abbrev PeerId := String

/-- A per-collection scope filter predicate carried by a filtered replicator.

Mirrors the Rust `FilterPredicate { field, value }` (the #1033 single-field
equality predicate resolved from a `Scope::PeerDid`) plus the collection it
applies to. The model treats it abstractly: only equality matters, since a
filter is part of the replicator's *identity*, not behavior we reason about
field-by-field here. -/
structure CollectionFilterKey where
  collection : String
  field : String
  value : String
  deriving DecidableEq, Repr

/-- A managed replicator's per-collection filter identity.

Mirrors Rust `PairingFilters = BTreeMap<collection, FilterPredicate>`. Empty
means unfiltered. A non-empty set may filter only some collections carried by a
replicator; collections absent from the set are unfiltered control-plane
collections. -/
abbrev ReplicatorFilter := Finset CollectionFilterKey

/-- The collection set a managed replicator carries (collection-name space). -/
abbrev ReplicatorCollections := Finset String

/-- A managed replicator's identity: its address, its filter map identity, and
the collection set it carries.

This is the deliverable of Part A generalized for per-collection filters and
then for the carried collection set: `(address, filters, collections)` makes
"same address, different filter map" AND "same address, same filter, different
collection set" distinct replicators, so either change is a teardown of the old
identity and an install of the new one (no in-place mutate). The collections
component fences the live demo bug where a replicator installed from the
data-plane layer alone silently kept its narrow collection set after the
control-plane layer merged in — an address-keyed diff converged falsely and the
network collections were never pushed. `∅` filters means an unfiltered
replicator (`Replicate` delivery). -/
abbrev ReplicatorId := String × ReplicatorFilter × ReplicatorCollections

namespace ReplicatorId

/-- The replicator's transport address (the connection target). -/
def address (r : ReplicatorId) : String := r.1

/-- The replicator's per-collection filter identity (`∅` = unfiltered). -/
def filter (r : ReplicatorId) : ReplicatorFilter := r.2.1

/-- The collection set the replicator carries. -/
def collections (r : ReplicatorId) : ReplicatorCollections := r.2.2

end ReplicatorId

/-- Per-peer-per-collection retry state. Visibility only; not part of safety. -/
structure PairingCollectionStatus where
  collectionId : String
  retryCount : Nat
  stuck : Bool
  deriving DecidableEq, Repr

/-- Operator-set desired pairing for one peer. Replicators are keyed on
`(address, filter)` so a filter change is a distinct desired replicator. -/
structure PairingDesired where
  collections : Finset String
  replicators : Finset ReplicatorId
  deriving DecidableEq

namespace PairingDesired

/-- A peer only needs a live connection when there is managed wiring to maintain. -/
def hasWiring (d : PairingDesired) : Bool :=
  decide (d.collections.Nonempty ∨ d.replicators.Nonempty)

end PairingDesired

/-- Remote-observed actual pairing for one peer.

RUST BOUNDARY: the model keys actual replicators on the full `ReplicatorId =
(address, ReplicatorFilter, ReplicatorCollections)` so the convergence proofs
can reason about the identity uniformly. The Rust `PairingActual`
(`p2p_reconcile/diff.rs`) observes the transport *address* and the *collection
set* each replicator carries (`list_replicators` returns both) — the installed
filter map is not recoverable from the peer, so the filter component is
recovered from the reconciler-owned `PairingApplied.replicator_filter` instead.
The composition is equivalent for the diff's safety obligations because a
managed replicator's filter map is always known on the applied side; this
abstraction gap is the intended boundary, not a hole. -/
structure PairingActual where
  collections : Finset String
  replicators : Finset ReplicatorId
  connected : Bool
  deriving DecidableEq

/-- Wiring introduced by this reconciler and therefore safe to remove. -/
structure PairingApplied where
  collections : Finset String
  replicators : Finset ReplicatorId
  deriving DecidableEq

/-- One emitted RPC instruction, matching Rust `DiffOp`. -/
inductive DiffOp where
  | installCollection (c : String)
  | teardownCollection (c : String)
  | installReplicator (r : ReplicatorId)
  | teardownReplicator (r : ReplicatorId)
  deriving DecidableEq

/-- Full reconcile state for one peer. -/
structure ReconcileState where
  peer : PeerId
  /-- `none` means the desired row read failed; `some ∅` means positive absence. -/
  desired : Option PairingDesired
  actual : PairingActual
  applied : PairingApplied
  pairing : List PairingCollectionStatus
  deriving DecidableEq

namespace ReconcileState

/-- A peer is converged when desired and actual managed wiring sets match. -/
def converged (s : ReconcileState) : Prop :=
  match s.desired with
  | none => True
  | some desired =>
      desired.collections ⊆ s.actual.collections ∧
      desired.replicators ⊆ s.actual.replicators ∧
      s.applied.collections ⊆ desired.collections ∧
      s.applied.replicators ⊆ desired.replicators ∧
      (desired.hasWiring = true → s.actual.connected = true)

instance (s : ReconcileState) : Decidable s.converged := by
  classical
  unfold converged
  cases s.desired <;> infer_instance

/-- Canonical converged state reached after all pending diff ops are applied. -/
def convergedState (s : ReconcileState) : ReconcileState :=
  match s.desired with
  | none => s
  | some desired =>
      { s with
        actual := ({
          collections := desired.collections,
          replicators := desired.replicators,
          connected := desired.hasWiring
        } : PairingActual),
        applied := {
          collections := desired.collections
          replicators := desired.replicators
        }
      }

theorem convergedState_converged (s : ReconcileState) :
    (convergedState s).converged := by
  unfold converged convergedState
  cases h : s.desired <;> simp [h]

end ReconcileState

end PairingReconcile
