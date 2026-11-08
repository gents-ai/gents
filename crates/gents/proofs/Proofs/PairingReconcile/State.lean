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

abbrev ReplicatorFilter := Finset CollectionFilterKey

abbrev ReplicatorCollections := Finset String

abbrev ReplicatorId := String × ReplicatorFilter × ReplicatorCollections

namespace ReplicatorId

def address (r : ReplicatorId) : String := r.1

def filter (r : ReplicatorId) : ReplicatorFilter := r.2.1

def collections (r : ReplicatorId) : ReplicatorCollections := r.2.2

def sameEndpoint (left right : ReplicatorId) : Bool :=
  left.address == right.address

end ReplicatorId

def eraseReplicatorsAtAddress
    (replicators : Finset ReplicatorId) (address : String) : Finset ReplicatorId :=
  replicators.filter fun candidate => candidate.address != address

structure PairingCollectionStatus where
  collectionId : String
  retryCount : Nat
  stuck : Bool
  deriving DecidableEq, Repr

/-- Desired wiring is replicators only.

Decision (supported topology, 2026-11-07): no surviving scope template
subscribes, so pairing reconciles replicator installs and owns no collection
subscriptions. -/
structure PairingDesired where
  replicators : Finset ReplicatorId
  deriving DecidableEq

namespace PairingDesired

def hasWiring (d : PairingDesired) : Bool :=
  decide d.replicators.Nonempty

end PairingDesired

structure PairingActual where
  replicators : Finset ReplicatorId
  connected : Bool
  deriving DecidableEq

structure PairingApplied where
  replicators : Finset ReplicatorId
  deriving DecidableEq

inductive DiffOp where
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
      desired.replicators ⊆ s.actual.replicators ∧
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
          replicators := desired.replicators,
          connected := desired.hasWiring
        } : PairingActual),
        applied := {
          replicators := desired.replicators
        }
      }

theorem convergedState_converged (s : ReconcileState) :
    (convergedState s).converged := by
  unfold converged convergedState
  cases h : s.desired <;> simp [h]

end ReconcileState

end PairingReconcile
