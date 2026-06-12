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

namespace PairingDesired

/-- A peer only needs a live connection when there is managed wiring to maintain. -/
def hasWiring (d : PairingDesired) : Bool :=
  decide (d.collections.Nonempty ∨ d.replicators.Nonempty)

end PairingDesired

/-- Remote-observed actual pairing for one peer. -/
structure PairingActual where
  collections : Finset String
  replicators : Finset String
  connected : Bool
  deriving DecidableEq

/-- Wiring introduced by this reconciler and therefore safe to remove. -/
structure PairingApplied where
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
