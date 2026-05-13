import Proofs.PairingReconcile.State

/-!
# Pairing Reconcile Transitions

One transition per supervisor-observable step. Reconcile actions move actual
collections toward desired collections. Operator writes change desired state.
Crashes clear only in-memory retry visibility.
-/

namespace PairingReconcile

inductive Transition : ReconcileState → ReconcileState → Prop where
  | operatorWrite {pre post : ReconcileState} (newDesired : PairingDesired) :
      newDesired ≠ pre.desired →
      post = { pre with desired := newDesired } →
      Transition pre post
  | reconcileInstall {pre post : ReconcileState} (c : String) :
      c ∈ pre.desired.collections →
      c ∉ pre.actual.collections →
      post = { pre with actual := { collections := insert c pre.actual.collections } } →
      Transition pre post
  | reconcileTeardown {pre post : ReconcileState} (c : String) :
      c ∈ pre.actual.collections →
      c ∉ pre.desired.collections →
      post = { pre with actual := { collections := pre.actual.collections.erase c } } →
      Transition pre post
  | crash {pre post : ReconcileState} :
      post = { pre with pairing := [] } →
      Transition pre post

theorem crash_preserves_desired_actual
    {pre post : ReconcileState}
    (h_trans : Transition pre post)
    (h_crash : ∃ h, h_trans = Transition.crash h) :
    post.desired = pre.desired ∧ post.actual = pre.actual := by
  rcases h_crash with ⟨h_post, h_eq⟩
  subst h_eq
  cases h_post
  exact ⟨rfl, rfl⟩

theorem reconcileInstall_adds_target
    {pre post : ReconcileState} {c : String}
    (h_post : post = { pre with actual := { collections := insert c pre.actual.collections } }) :
    c ∈ post.actual.collections := by
  cases h_post
  exact Finset.mem_insert_self c pre.actual.collections

theorem reconcileTeardown_removes_target
    {pre post : ReconcileState} {c : String}
    (h_post : post = { pre with actual := { collections := pre.actual.collections.erase c } }) :
    c ∉ post.actual.collections := by
  cases h_post
  exact Finset.not_mem_erase c pre.actual.collections

end PairingReconcile
