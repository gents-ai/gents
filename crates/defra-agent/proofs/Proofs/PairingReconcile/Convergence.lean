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
      refine ⟨?_, ?_, ?_, ?_, h_connected⟩
      · exact fun c hc => h_desired_collections hc
      · exact fun c _hc_actual hc_applied => h_applied_collections hc_applied
      · exact fun r f hr => h_desired_replicators hr
      · exact fun r f _hr_actual hr_applied => h_applied_replicators hr_applied

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

/-! ## Filter change forces reinstall

A replicator's filter is part of its identity (`(address, filter)`), so changing
the filter on an existing address is NOT an in-place mutate: it is a teardown of
the old `(a, f1)` identity and an install of the new `(a, f2)` identity. We prove
that from a state where desired carries `(a, f2)` but actual still carries the
old `(a, f1)`:

  - the diff is genuinely non-empty (`disagreementCount > 0`), so the state is
    not falsely converged;
  - both `reconcileTeardownReplicator (a, f1)` and `reconcileInstallReplicator
    (a, f2)` are ENABLED transitions out of the state (their real guards hold);
  - applying teardown-then-install drives that address's actual replicator from
    `(a, f1)` to `(a, f2)`.

The hypothesis `f1 ≠ f2` is satisfiable (e.g. `∅` vs a singleton
per-collection filter, or two distinct filter maps), and is what makes
`(a, f1) ≠ (a, f2)` — the two identities distinct. -/

/-- `f1 ≠ f2` makes the two replicator identities on the same address distinct.
Sanity that the central hypothesis has teeth. -/
theorem filter_change_distinct_identity
    {a : String} {f1 f2 : ReplicatorFilter} (hf : f1 ≠ f2) :
    ((a, f1) : ReplicatorId) ≠ (a, f2) := by
  intro h
  exact hf (by injection h)

/-- A concrete witness that the `f1 ≠ f2` hypothesis is satisfiable: an
unfiltered replicator and a filtered one on the same address are distinct. -/
theorem filter_change_hypothesis_satisfiable (a : String) (k : CollectionFilterKey) :
    ((a, (∅ : ReplicatorFilter)) : ReplicatorId) ≠ (a, ({k} : ReplicatorFilter)) := by
  apply filter_change_distinct_identity
  intro h
  have hk : k ∈ ({k} : ReplicatorFilter) := Finset.mem_singleton_self k
  rw [← h] at hk
  exact Finset.not_mem_empty k hk

/-- **Filter change forces reinstall.** When desired carries `(a, f2)` and actual
still carries the managed old identity `(a, f1)` with `f1 ≠ f2`, the diff is
non-empty and BOTH the teardown of the old identity and the install of the new
one are enabled steps; teardown-then-install converges that address to `(a, f2)`.

Every conjunct is quantified over the real transition relation: the two
`Transition` witnesses are built from the actual constructors and discharge their
guards from the hypotheses, so this is not a vacuous restatement of the goal. -/
theorem filter_change_forces_reinstall
    {s : ReconcileState} {desired : PairingDesired}
    {a : String} {f1 f2 : ReplicatorFilter}
    (h_desired : s.desired = some desired)
    (hf : f1 ≠ f2)
    (h_new_desired : ((a, f2) : ReplicatorId) ∈ desired.replicators)
    (h_old_not_desired : ((a, f1) : ReplicatorId) ∉ desired.replicators)
    (h_old_actual : ((a, f1) : ReplicatorId) ∈ s.actual.replicators)
    (h_old_applied : ((a, f1) : ReplicatorId) ∈ s.applied.replicators)
    (h_new_not_actual : ((a, f2) : ReplicatorId) ∉ s.actual.replicators)
    (h_connected : s.actual.connected = true) :
    -- (1) the state is genuinely diverged
    0 < disagreementCount s ∧
    -- (2) tearing down the OLD identity is an enabled transition
    Transition s (teardownReplicatorState s (a, f1)) ∧
    -- (3) installing the NEW identity is an enabled transition
    Transition s (installReplicatorState s (a, f2)) ∧
    -- (4) teardown-then-install lands the address on the new identity
    ( ((a, f2) : ReplicatorId) ∈
        (installReplicatorState (teardownReplicatorState s (a, f1)) (a, f2)).actual.replicators ∧
      ((a, f1) : ReplicatorId) ∉
        (installReplicatorState (teardownReplicatorState s (a, f1)) (a, f2)).actual.replicators ) := by
  refine ⟨?_, ?_, ?_, ?_, ?_⟩
  · -- non-empty diff: (a, f2) is desired but not actual, so it sits in the
    -- desired \ actual symmetric-difference summand.
    have h_mem : ((a, f2) : ReplicatorId) ∈ desired.replicators \ s.actual.replicators :=
      Finset.mem_sdiff.mpr ⟨h_new_desired, h_new_not_actual⟩
    have h_card_pos : 0 < (desired.replicators \ s.actual.replicators).card :=
      Finset.card_pos.mpr ⟨_, h_mem⟩
    simp only [disagreementCount, h_desired]
    omega
  · exact Transition.reconcileTeardownReplicator desired (a, f1)
      h_desired h_old_actual h_old_not_desired h_old_applied rfl
  · exact Transition.reconcileInstallReplicator desired (a, f2)
      h_desired h_new_desired h_new_not_actual h_connected rfl
  · -- install inserts (a, f2)
    exact Finset.mem_insert_self _ _
  · -- after teardown of (a, f1) then install of (a, f2), (a, f1) is gone:
    -- it is not (a, f2) (distinct identities) and was erased.
    simp only [installReplicatorState, teardownReplicatorState]
    rw [Finset.mem_insert]
    push_neg
    exact ⟨filter_change_distinct_identity hf, Finset.not_mem_erase _ _⟩

/-- Stable converged desired state cannot flap managed wiring. -/
theorem no_flap_on_converged_stable_desired
    {pre post : ReconcileState}
    (h : pre.converged)
    (h_trans : Transition pre post)
    (_h_desired_stable : post.desired = pre.desired) :
    managedWiringUnchanged pre post :=
  no_flap_on_converged_step h h_trans

end PairingReconcile
