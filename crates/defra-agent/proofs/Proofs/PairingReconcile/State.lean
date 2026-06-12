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

/-- Per-peer-per-collection retry state. Visibility only; not part of safety. -/
structure PairingCollectionStatus where
  collectionId : String
  retryCount : Nat
  stuck : Bool
  deriving DecidableEq, Repr

/-- Operator-set desired pairing for one peer. -/
structure PairingDesired where
  collections : Finset String
  replicators : Finset String
  deriving DecidableEq

/-- Remote-observed actual pairing for one peer. -/
structure PairingActual where
  collections : Finset String
  replicators : Finset String
  deriving DecidableEq

/-- One emitted RPC instruction, matching Rust `DiffOp`. -/
inductive DiffOp where
  | installCollection (c : String)
  | teardownCollection (c : String)
  | installReplicator (r : String)
  | teardownReplicator (r : String)
  deriving DecidableEq, Repr

/-- Full reconcile state for one peer. -/
structure ReconcileState where
  peer : PeerId
  desired : PairingDesired
  actual : PairingActual
  pairing : List PairingCollectionStatus
  deriving DecidableEq

namespace ReconcileState

/-- A peer is converged when desired and actual managed wiring sets match. -/
def converged (s : ReconcileState) : Prop :=
  s.desired.collections = s.actual.collections ∧
    s.desired.replicators = s.actual.replicators

instance (s : ReconcileState) : Decidable s.converged := by
  unfold converged
  infer_instance

/-- Canonical converged state reached after all pending diff ops are applied. -/
def convergedState (s : ReconcileState) : ReconcileState :=
  { s with actual := {
      collections := s.desired.collections
      replicators := s.desired.replicators
    } }

theorem convergedState_converged (s : ReconcileState) :
    (convergedState s).converged := by
  simp [converged, convergedState]

end ReconcileState

end PairingReconcile
