import Proofs.PairingReconcile.State
import Proofs.PairingReconcile.Transition
import Mathlib.Data.Finset.SDiff

namespace PairingReconcile

open ReconcileState

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

theorem convergedState_is_fixpoint
    (s : ReconcileState) :
    ∃ post : ReconcileState,
      post.desired = s.desired ∧
      post.converged ∧
      disagreementCount post = 0 := by
  refine ⟨convergedState s, ?_, convergedState_converged s, convergedState_has_zero_disagreement s⟩
  cases h : s.desired <;> simp [convergedState, h]

def noReconcileOpEnabled (s : ReconcileState) : Prop :=
  match s.desired with
  | none => True
  | some desired =>
      (∀ c, c ∈ desired.collections → c ∈ s.actual.collections) ∧
      (∀ c, c ∈ s.actual.collections → c ∈ s.applied.collections → c ∈ desired.collections) ∧
      (∀ r, r ∈ desired.replicators → r ∈ s.actual.replicators) ∧
      (∀ r, r ∈ s.actual.replicators → r ∈ s.applied.replicators → r ∈ desired.replicators) ∧
      (desired.hasWiring = true → s.actual.connected = true)

def managedWiringUnchanged (pre post : ReconcileState) : Prop :=
  post.actual.collections = pre.actual.collections ∧
  post.actual.replicators = pre.actual.replicators ∧
  post.applied = pre.applied

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
      refine ⟨?_, ?_, ?_, ?_, h_connected⟩
      · exact fun c hc => h_desired_collections hc
      · exact fun c _hc_actual hc_applied => h_applied_collections hc_applied
      · exact fun r f cs hr => h_desired_replicators hr
      · exact fun r f cs _hr_actual hr_applied => h_applied_replicators hr_applied

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
  | dialFailed desired h_desired h_has_wiring h_disconnected h_post =>
      subst h_post
      simp [managedWiringUnchanged]
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
  | reconcileInstallReplicatorFailed desired target h_desired h_target h_missing h_connected h_post =>
      subst h_post
      simp [managedWiringUnchanged]
  | reconcileTeardownReplicator desired target h_desired h_not_desired h_applied h_post =>
      exfalso
      unfold ReconcileState.converged at h
      simp [h_desired] at h
      exact h_not_desired (h.2.2.2.1 h_applied)
  | crash h_post =>
      subst h_post
      simp [managedWiringUnchanged]

theorem filter_change_distinct_identity
    {a : String} {cs : ReplicatorCollections} {f1 f2 : ReplicatorFilter} (hf : f1 ≠ f2) :
    ((a, f1, cs) : ReplicatorId) ≠ (a, f2, cs) := by
  intro h
  have h_rest : (f1, cs) = (f2, cs) := by injection h
  have h_f : f1 = f2 := by injection h_rest
  exact hf h_f

theorem collections_change_distinct_identity
    {a : String} {f : ReplicatorFilter} {cs1 cs2 : ReplicatorCollections} (hcs : cs1 ≠ cs2) :
    ((a, f, cs1) : ReplicatorId) ≠ (a, f, cs2) := by
  intro h
  have h_rest : (f, cs1) = (f, cs2) := by injection h
  have h_cs : cs1 = cs2 := by injection h_rest
  exact hcs h_cs

theorem filter_change_hypothesis_satisfiable (a : String) (k : CollectionFilterKey) :
    ((a, (∅ : ReplicatorFilter), (∅ : ReplicatorCollections)) : ReplicatorId)
      ≠ (a, ({k} : ReplicatorFilter), (∅ : ReplicatorCollections)) := by
  apply filter_change_distinct_identity
  intro h
  have hk : k ∈ ({k} : ReplicatorFilter) := Finset.mem_singleton_self k
  rw [← h] at hk
  exact Finset.not_mem_empty k hk

theorem collections_change_hypothesis_satisfiable (a : String) (c : String) :
    ((a, (∅ : ReplicatorFilter), (∅ : ReplicatorCollections)) : ReplicatorId)
      ≠ (a, (∅ : ReplicatorFilter), ({c} : ReplicatorCollections)) := by
  apply collections_change_distinct_identity
  intro h
  have hc : c ∈ ({c} : ReplicatorCollections) := Finset.mem_singleton_self c
  rw [← h] at hc
  exact Finset.not_mem_empty c hc

theorem filter_change_forces_reinstall
    {s : ReconcileState} {desired : PairingDesired}
    {a : String} {cs : ReplicatorCollections} {f1 f2 : ReplicatorFilter}
    (h_desired : s.desired = some desired)
    (hf : f1 ≠ f2)
    (h_new_desired : ((a, f2, cs) : ReplicatorId) ∈ desired.replicators)
    (h_old_not_desired : ((a, f1, cs) : ReplicatorId) ∉ desired.replicators)
    (h_old_applied : ((a, f1, cs) : ReplicatorId) ∈ s.applied.replicators)
    (h_new_not_actual : ((a, f2, cs) : ReplicatorId) ∉ s.actual.replicators)
    (h_connected : s.actual.connected = true) :
    0 < disagreementCount s ∧
    Transition s (teardownReplicatorState s (a, f1, cs)) ∧
    Transition s (installReplicatorState s (a, f2, cs)) ∧
    ( ((a, f2, cs) : ReplicatorId) ∈
        (installReplicatorState (teardownReplicatorState s (a, f1, cs)) (a, f2, cs)).actual.replicators ∧
      ((a, f1, cs) : ReplicatorId) ∉
        (installReplicatorState (teardownReplicatorState s (a, f1, cs)) (a, f2, cs)).actual.replicators ) := by
  refine ⟨?_, ?_, ?_, ?_, ?_⟩
  ·
    have h_mem : ((a, f2, cs) : ReplicatorId) ∈ desired.replicators \ s.actual.replicators :=
      Finset.mem_sdiff.mpr ⟨h_new_desired, h_new_not_actual⟩
    have h_card_pos : 0 < (desired.replicators \ s.actual.replicators).card :=
      Finset.card_pos.mpr ⟨_, h_mem⟩
    simp only [disagreementCount, h_desired]
    omega
  · exact Transition.reconcileTeardownReplicator desired (a, f1, cs)
      h_desired h_old_not_desired h_old_applied rfl
  · exact Transition.reconcileInstallReplicator desired (a, f2, cs)
      h_desired h_new_desired h_new_not_actual h_connected rfl
  ·
    exact Finset.mem_insert_self _ _
  ·
    simp only [installReplicatorState, teardownReplicatorState]
    rw [Finset.mem_insert]
    push_neg
    exact ⟨filter_change_distinct_identity hf, Finset.not_mem_erase _ _⟩

theorem collections_change_forces_reinstall
    {s : ReconcileState} {desired : PairingDesired}
    {a : String} {f : ReplicatorFilter} {cs1 cs2 : ReplicatorCollections}
    (h_desired : s.desired = some desired)
    (hcs : cs1 ≠ cs2)
    (h_new_desired : ((a, f, cs2) : ReplicatorId) ∈ desired.replicators)
    (h_old_not_desired : ((a, f, cs1) : ReplicatorId) ∉ desired.replicators)
    (h_old_applied : ((a, f, cs1) : ReplicatorId) ∈ s.applied.replicators)
    (h_new_not_actual : ((a, f, cs2) : ReplicatorId) ∉ s.actual.replicators)
    (h_connected : s.actual.connected = true) :
    0 < disagreementCount s ∧
    Transition s (teardownReplicatorState s (a, f, cs1)) ∧
    Transition s (installReplicatorState s (a, f, cs2)) ∧
    ( ((a, f, cs2) : ReplicatorId) ∈
        (installReplicatorState (teardownReplicatorState s (a, f, cs1)) (a, f, cs2)).actual.replicators ∧
      ((a, f, cs1) : ReplicatorId) ∉
        (installReplicatorState (teardownReplicatorState s (a, f, cs1)) (a, f, cs2)).actual.replicators ) := by
  refine ⟨?_, ?_, ?_, ?_, ?_⟩
  · have h_mem : ((a, f, cs2) : ReplicatorId) ∈ desired.replicators \ s.actual.replicators :=
      Finset.mem_sdiff.mpr ⟨h_new_desired, h_new_not_actual⟩
    have h_card_pos : 0 < (desired.replicators \ s.actual.replicators).card :=
      Finset.card_pos.mpr ⟨_, h_mem⟩
    simp only [disagreementCount, h_desired]
    omega
  · exact Transition.reconcileTeardownReplicator desired (a, f, cs1)
      h_desired h_old_not_desired h_old_applied rfl
  · exact Transition.reconcileInstallReplicator desired (a, f, cs2)
      h_desired h_new_desired h_new_not_actual h_connected rfl
  · exact Finset.mem_insert_self _ _
  · simp only [installReplicatorState, teardownReplicatorState]
    rw [Finset.mem_insert]
    push_neg
    exact ⟨collections_change_distinct_identity hcs, Finset.not_mem_erase _ _⟩

theorem no_flap_on_converged_stable_desired
    {pre post : ReconcileState}
    (h : pre.converged)
    (h_trans : Transition pre post)
    (_h_desired_stable : post.desired = pre.desired) :
    managedWiringUnchanged pre post :=
  no_flap_on_converged_step h h_trans

theorem dial_failure_is_nonconverging_fixpoint
    {s : ReconcileState} {desired : PairingDesired}
    (h_desired : s.desired = some desired)
    (h_wiring : desired.hasWiring = true)
    (h_disconnected : s.actual.connected = false) :
    Transition s s ∧ ¬ s.converged ∧ 0 < disagreementCount s := by
  refine ⟨Transition.dialFailed desired h_desired h_wiring h_disconnected rfl, ?_, ?_⟩
  · intro hc
    unfold ReconcileState.converged at hc
    simp only [h_desired] at hc
    obtain ⟨_, _, _, _, h_conn⟩ := hc
    have hco := h_conn h_wiring
    rw [h_disconnected] at hco
    simp at hco
  · simp only [disagreementCount, h_desired, h_wiring, h_disconnected]
    simp

theorem partial_applied_replicator_stuck
    {s : ReconcileState} {desired : PairingDesired} {r : ReplicatorId}
    (h_desired : s.desired = some desired)
    (h_target : r ∈ desired.replicators)
    (h_missing : r ∉ s.actual.replicators)
    (h_connected : s.actual.connected = true) :
    ¬ s.converged ∧
      Transition s (installReplicatorState s r) ∧
      Transition s s ∧
      0 < disagreementCount s := by
  refine ⟨?_, ?_, ?_, ?_⟩
  · intro hc
    unfold ReconcileState.converged at hc
    simp only [h_desired] at hc
    obtain ⟨_, h_repl, _, _, _⟩ := hc
    exact h_missing (h_repl h_target)
  · exact Transition.reconcileInstallReplicator desired r h_desired h_target h_missing h_connected rfl
  · exact Transition.reconcileInstallReplicatorFailed desired r h_desired h_target h_missing h_connected rfl
  · have h_mem : r ∈ desired.replicators \ s.actual.replicators :=
      Finset.mem_sdiff.mpr ⟨h_target, h_missing⟩
    have h_card : 0 < (desired.replicators \ s.actual.replicators).card :=
      Finset.card_pos.mpr ⟨_, h_mem⟩
    simp only [disagreementCount, h_desired]
    omega

theorem stale_applied_replicator_can_be_torn_down
    {s : ReconcileState} {desired : PairingDesired} {r : ReplicatorId}
    (h_desired : s.desired = some desired)
    (h_applied : r ∈ s.applied.replicators)
    (h_not_desired : r ∉ desired.replicators) :
    Transition s (teardownReplicatorState s r) ∧
      r ∉ (teardownReplicatorState s r).actual.replicators ∧
      r ∉ (teardownReplicatorState s r).applied.replicators := by
  refine ⟨Transition.reconcileTeardownReplicator desired r h_desired h_not_desired h_applied rfl,
    ?_, ?_⟩
  · exact Finset.not_mem_erase r s.actual.replicators
  · exact Finset.not_mem_erase r s.applied.replicators

theorem convergence_requires_successful_install
    {s post : ReconcileState} {desired : PairingDesired} {r : ReplicatorId}
    (h_desired : s.desired = some desired)
    (h_target : r ∈ desired.replicators)
    (h_missing : r ∉ s.actual.replicators)
    (h_failed_step : post = s) :
    disagreementCount post = disagreementCount s ∧ 0 < disagreementCount post := by
  rw [h_failed_step]
  refine ⟨rfl, ?_⟩
  have h_mem : r ∈ desired.replicators \ s.actual.replicators :=
    Finset.mem_sdiff.mpr ⟨h_target, h_missing⟩
  have h_card : 0 < (desired.replicators \ s.actual.replicators).card :=
    Finset.card_pos.mpr ⟨_, h_mem⟩
  simp only [disagreementCount, h_desired]
  omega

end PairingReconcile
