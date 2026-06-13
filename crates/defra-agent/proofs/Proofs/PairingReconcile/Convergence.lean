import Proofs.PairingReconcile.State
import Proofs.PairingReconcile.Transition
import Mathlib.Data.Finset.SDiff

/-!
# Pairing Reconcile Convergence

This file states the canonical fixed point, disagreement measure, and
idempotence/no-flap guards used by the conformance harness. It deliberately
does not claim a reachability proof over `Transition⋆`; the transition-level
safety obligations live in `Transition.lean`.
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
      (s.applied.replicators \ desired.replicators).card +
      if desired.hasWiring && !s.actual.connected then 1 else 0

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
      h_applied_collections, h_applied_replicators, h_connected⟩
    by_cases h_has_wiring : desired.hasWiring = true
    · have h_actual_connected : s.actual.connected = true := h_connected h_has_wiring
      simp [disagreementCount, h_desired, h_has_wiring, h_actual_connected,
        Finset.sdiff_eq_empty_iff_subset.mpr h_desired_collections,
        Finset.sdiff_eq_empty_iff_subset.mpr h_desired_replicators,
        Finset.sdiff_eq_empty_iff_subset.mpr h_applied_collections,
        Finset.sdiff_eq_empty_iff_subset.mpr h_applied_replicators
      ]
    · simp [disagreementCount, h_desired, h_has_wiring,
        Finset.sdiff_eq_empty_iff_subset.mpr h_desired_collections,
        Finset.sdiff_eq_empty_iff_subset.mpr h_desired_replicators,
        Finset.sdiff_eq_empty_iff_subset.mpr h_applied_collections,
        Finset.sdiff_eq_empty_iff_subset.mpr h_applied_replicators
      ]

theorem convergedState_has_zero_disagreement (s : ReconcileState) :
    disagreementCount (convergedState s) = 0 := by
  exact converged_disagreementCount_zero (convergedState_converged s)

/-- The canonical fixed-point constructor preserves desired and is converged. -/
theorem convergedState_is_fixpoint
    (s : ReconcileState) :
    ∃ post : ReconcileState,
      post.desired = s.desired ∧
      post.converged ∧
      disagreementCount post = 0 := by
  refine ⟨convergedState s, ?_, convergedState_converged s, convergedState_has_zero_disagreement s⟩
  cases h : s.desired <;> simp [convergedState, h]

/-- No modeled reconcile install/teardown guard remains enabled. -/
def noReconcileOpEnabled (s : ReconcileState) : Prop :=
  match s.desired with
  | none => True
  | some desired =>
      (∀ c, c ∈ desired.collections → c ∈ s.actual.collections) ∧
      (∀ c, c ∈ s.actual.collections → c ∈ s.applied.collections → c ∈ desired.collections) ∧
      (∀ r, r ∈ desired.replicators → r ∈ s.actual.replicators) ∧
      (∀ r, r ∈ s.actual.replicators → r ∈ s.applied.replicators → r ∈ desired.replicators) ∧
      (desired.hasWiring = true → s.actual.connected = true)

/-- Managed actual/applied wiring stayed fixed across a transition. -/
def managedWiringUnchanged (pre post : ReconcileState) : Prop :=
  post.actual.collections = pre.actual.collections ∧
  post.actual.replicators = pre.actual.replicators ∧
  post.applied = pre.applied

/-- A converged state is idempotent for the owned diff: no reconcile op is enabled. -/
theorem reconcile_idempotent_on_converged
    {s : ReconcileState}
    (h : s.converged) :
    noReconcileOpEnabled s := by
  unfold ReconcileState.converged noReconcileOpEnabled at *
  cases h_desired : s.desired with
  | none =>
      simp [h_desired]
  | some desired =>
      simp [h_desired] at h ⊢
      rcases h with ⟨h_desired_collections, h_desired_replicators,
        h_applied_collections, h_applied_replicators, h_connected⟩
      exact ⟨
        (fun c hc => h_desired_collections hc),
        (fun c _ hc_applied => h_applied_collections hc_applied),
        (fun r hr => h_desired_replicators hr),
        (fun r _ hr_applied => h_applied_replicators hr_applied),
        h_connected
      ⟩

/-- A one-step transition from a converged state cannot flap managed wiring. -/
theorem no_flap_on_converged_step
    {pre post : ReconcileState}
    (h : pre.converged)
    (h_trans : Transition pre post) :
    managedWiringUnchanged pre post := by
  cases h_trans with
  | operatorWrite newDesired h_ne h_post =>
      subst h_post
      simp [managedWiringUnchanged]
  | operatorDelete h_post =>
      subst h_post
      simp [managedWiringUnchanged]
  | readFailure h_post =>
      subst h_post
      simp [managedWiringUnchanged]
  | dial desired h_desired h_has_wiring h_disconnected h_post =>
      subst h_post
      simp [managedWiringUnchanged, dialState]
  | peerDisconnected h_connected h_post =>
      subst h_post
      simp [managedWiringUnchanged, disconnectedState]
  | reconcileInstall desired target h_desired h_target h_missing h_connected h_post =>
      exfalso
      unfold ReconcileState.converged at h
      simp [h_desired] at h
      exact h_missing (h.1 h_target)
  | reconcileTeardown desired target h_desired h_actual h_not_desired h_applied h_post =>
      exfalso
      unfold ReconcileState.converged at h
      simp [h_desired] at h
      exact h_not_desired (h.2.2.1 h_applied)
  | reconcileInstallReplicator desired target h_desired h_target h_missing h_connected h_post =>
      exfalso
      unfold ReconcileState.converged at h
      simp [h_desired] at h
      exact h_missing (h.2.1 h_target)
  | reconcileTeardownReplicator desired target h_desired h_actual h_not_desired h_applied h_post =>
      exfalso
      unfold ReconcileState.converged at h
      simp [h_desired] at h
      exact h_not_desired (h.2.2.2.1 h_applied)
  | crash h_post =>
      subst h_post
      simp [managedWiringUnchanged]

/-- Stable converged desired state cannot flap managed wiring. -/
theorem no_flap_on_converged_stable_desired
    {pre post : ReconcileState}
    (h : pre.converged)
    (h_trans : Transition pre post)
    (_h_desired_stable : post.desired = pre.desired) :
    managedWiringUnchanged pre post :=
  no_flap_on_converged_step h h_trans

end PairingReconcile
