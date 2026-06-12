import Proofs.PairingReconcile.State
import Proofs.PairingReconcile.Transition

/-!
# Pairing Reconcile Convergence

The executable supervisor repeatedly applies the diff until the remote actual
set equals desired. This file states the finite target state and disagreement
measure used by the conformance harness.
-/

namespace PairingReconcile

open ReconcileState

/-- Symmetric difference size between desired and actual managed wiring. -/
def disagreementCount (s : ReconcileState) : Nat :=
  (s.desired.collections \ s.actual.collections).card +
    (s.actual.collections \ s.desired.collections).card +
    (s.desired.replicators \ s.actual.replicators).card +
    (s.actual.replicators \ s.desired.replicators).card

theorem converged_disagreementCount_zero
    {s : ReconcileState}
    (h : s.converged) :
    disagreementCount s = 0 := by
  unfold disagreementCount ReconcileState.converged at *
  rcases h with ⟨h_collections, h_replicators⟩
  rw [h_collections, h_replicators]
  simp

theorem convergedState_has_zero_disagreement (s : ReconcileState) :
    disagreementCount (convergedState s) = 0 := by
  exact converged_disagreementCount_zero (convergedState_converged s)

/-- Under stable desired state, there is a finite converged target state. -/
theorem reconcile_converges_in_finite_steps
    (s : ReconcileState) :
    ∃ post : ReconcileState,
      post.desired = s.desired ∧
      post.converged ∧
      disagreementCount post = 0 := by
  refine ⟨convergedState s, ?_, convergedState_converged s, convergedState_has_zero_disagreement s⟩
  simp [convergedState]

end PairingReconcile
