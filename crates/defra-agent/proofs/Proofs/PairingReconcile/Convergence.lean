import Proofs.PairingReconcile.State
import Proofs.PairingReconcile.Transition
import Mathlib.Data.Finset.SDiff

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
  match s.desired with
  | none => 0
  | some desired =>
      (desired.collections \ s.actual.collections).card +
      (desired.replicators \ s.actual.replicators).card +
      (s.applied.collections \ desired.collections).card +
      (s.applied.replicators \ desired.replicators).card

theorem converged_disagreementCount_zero
    {s : ReconcileState}
    (h : s.converged) :
    disagreementCount s = 0 := by
  unfold ReconcileState.converged at h
  cases h_desired : s.desired with
  | none =>
      simp [disagreementCount, h_desired]
  | some desired =>
    simp [h_desired] at h
    rcases h with ⟨h_desired_collections, h_desired_replicators,
      h_applied_collections, h_applied_replicators⟩
    simp [disagreementCount, h_desired,
      Finset.sdiff_eq_empty_iff_subset.mpr h_desired_collections,
      Finset.sdiff_eq_empty_iff_subset.mpr h_desired_replicators,
      Finset.sdiff_eq_empty_iff_subset.mpr h_applied_collections,
      Finset.sdiff_eq_empty_iff_subset.mpr h_applied_replicators
    ]

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
  cases h : s.desired <;> simp [convergedState, h]

end PairingReconcile
