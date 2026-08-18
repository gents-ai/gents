import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Card

namespace PairingReconcile

abbrev PeerId := String

structure CollectionFilterKey where
  collection : String
  field : String
  operator : String := "_eq"
  value : String
  deriving DecidableEq, Repr

/-- Filter atoms are interpreted conjunctively for each collection. -/
abbrev ReplicatorFilter := Finset CollectionFilterKey

abbrev ReplicatorCollections := Finset String

abbrev ReplicatorId := String × ReplicatorFilter × ReplicatorCollections

namespace ReplicatorId

def address (r : ReplicatorId) : String := r.1

def filter (r : ReplicatorId) : ReplicatorFilter := r.2.1

def collections (r : ReplicatorId) : ReplicatorCollections := r.2.2

end ReplicatorId

structure PairingCollectionStatus where
  collectionId : String
  retryCount : Nat
  stuck : Bool
  deriving DecidableEq, Repr

structure PairingDesired where
  collections : Finset String
  replicators : Finset ReplicatorId
  deriving DecidableEq

namespace PairingDesired

def hasWiring (d : PairingDesired) : Bool :=
  decide (d.collections.Nonempty ∨ d.replicators.Nonempty)

end PairingDesired

structure PairingActual where
  collections : Finset String
  replicators : Finset ReplicatorId
  connected : Bool
  deriving DecidableEq

structure PairingApplied where
  collections : Finset String
  replicators : Finset ReplicatorId
  deriving DecidableEq

inductive DiffOp where
  | installCollection (c : String)
  | teardownCollection (c : String)
  | installReplicator (r : ReplicatorId)
  | teardownReplicator (r : ReplicatorId)
  deriving DecidableEq

structure ReconcileState where
  peer : PeerId
  desired : Option PairingDesired
  actual : PairingActual
  applied : PairingApplied
  pairing : List PairingCollectionStatus
  deriving DecidableEq

namespace ReconcileState

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
