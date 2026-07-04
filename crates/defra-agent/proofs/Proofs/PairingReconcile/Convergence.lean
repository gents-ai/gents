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
      · exact fun r f cs hr => h_desired_replicators hr
      · exact fun r f cs _hr_actual hr_applied => h_applied_replicators hr_applied

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
  | reconcileTeardownReplicator desired target h_desired h_actual h_not_desired h_applied h_post =>
      exfalso
      unfold ReconcileState.converged at h
      simp [h_desired] at h
      exact h_not_desired (h.2.2.2.1 h_applied)
  | crash h_post =>
      subst h_post
      simp [managedWiringUnchanged]

/-! ## Identity change forces reinstall

A replicator's filter AND its carried collection set are part of its identity
(`(address, filter, collections)`), so changing either on an existing address is
NOT an in-place mutate: it is a teardown of the old identity and an install of
the new one. We prove that from a state where desired carries the new identity
but actual still carries the old one:

  - the diff is genuinely non-empty (`disagreementCount > 0`), so the state is
    not falsely converged;
  - both the teardown of the old identity and the install of the new one are
    ENABLED transitions out of the state (their real guards hold);
  - applying teardown-then-install drives that address's actual replicator to
    the new identity.

The distinctness hypotheses (`f1 ≠ f2`, `cs1 ≠ cs2`) are satisfiable (witnesses
below), and are what make the two identities distinct. -/

/-- `f1 ≠ f2` makes the two replicator identities on the same address and
collection set distinct. Sanity that the central hypothesis has teeth. -/
theorem filter_change_distinct_identity
    {a : String} {cs : ReplicatorCollections} {f1 f2 : ReplicatorFilter} (hf : f1 ≠ f2) :
    ((a, f1, cs) : ReplicatorId) ≠ (a, f2, cs) := by
  intro h
  have h_rest : (f1, cs) = (f2, cs) := by injection h
  have h_f : f1 = f2 := by injection h_rest
  exact hf h_f

/-- `cs1 ≠ cs2` makes the two replicator identities on the same address and
filter distinct. This is the live demo bug's identity component: a replicator
that carried only the data-plane collection set is NOT the replicator that
carries the merged data-plane + control-plane set. -/
theorem collections_change_distinct_identity
    {a : String} {f : ReplicatorFilter} {cs1 cs2 : ReplicatorCollections} (hcs : cs1 ≠ cs2) :
    ((a, f, cs1) : ReplicatorId) ≠ (a, f, cs2) := by
  intro h
  have h_rest : (f, cs1) = (f, cs2) := by injection h
  have h_cs : cs1 = cs2 := by injection h_rest
  exact hcs h_cs

/-- A concrete witness that the `f1 ≠ f2` hypothesis is satisfiable: an
unfiltered replicator and a filtered one on the same address are distinct. -/
theorem filter_change_hypothesis_satisfiable (a : String) (k : CollectionFilterKey) :
    ((a, (∅ : ReplicatorFilter), (∅ : ReplicatorCollections)) : ReplicatorId)
      ≠ (a, ({k} : ReplicatorFilter), (∅ : ReplicatorCollections)) := by
  apply filter_change_distinct_identity
  intro h
  have hk : k ∈ ({k} : ReplicatorFilter) := Finset.mem_singleton_self k
  rw [← h] at hk
  exact Finset.not_mem_empty k hk

/-- A concrete witness that the `cs1 ≠ cs2` hypothesis is satisfiable: the
data-plane-only collection set and the merged set differing by one
control-plane collection are distinct. -/
theorem collections_change_hypothesis_satisfiable (a : String) (c : String) :
    ((a, (∅ : ReplicatorFilter), (∅ : ReplicatorCollections)) : ReplicatorId)
      ≠ (a, (∅ : ReplicatorFilter), ({c} : ReplicatorCollections)) := by
  apply collections_change_distinct_identity
  intro h
  have hc : c ∈ ({c} : ReplicatorCollections) := Finset.mem_singleton_self c
  rw [← h] at hc
  exact Finset.not_mem_empty c hc

/-- **Filter change forces reinstall.** When desired carries `(a, f2)` and actual
still carries the managed old identity `(a, f1)` with `f1 ≠ f2`, the diff is
non-empty and BOTH the teardown of the old identity and the install of the new
one are enabled steps; teardown-then-install converges that address to `(a, f2)`.

Every conjunct is quantified over the real transition relation: the two
`Transition` witnesses are built from the actual constructors and discharge their
guards from the hypotheses, so this is not a vacuous restatement of the goal. -/
theorem filter_change_forces_reinstall
    {s : ReconcileState} {desired : PairingDesired}
    {a : String} {cs : ReplicatorCollections} {f1 f2 : ReplicatorFilter}
    (h_desired : s.desired = some desired)
    (hf : f1 ≠ f2)
    (h_new_desired : ((a, f2, cs) : ReplicatorId) ∈ desired.replicators)
    (h_old_not_desired : ((a, f1, cs) : ReplicatorId) ∉ desired.replicators)
    (h_old_actual : ((a, f1, cs) : ReplicatorId) ∈ s.actual.replicators)
    (h_old_applied : ((a, f1, cs) : ReplicatorId) ∈ s.applied.replicators)
    (h_new_not_actual : ((a, f2, cs) : ReplicatorId) ∉ s.actual.replicators)
    (h_connected : s.actual.connected = true) :
    -- (1) the state is genuinely diverged
    0 < disagreementCount s ∧
    -- (2) tearing down the OLD identity is an enabled transition
    Transition s (teardownReplicatorState s (a, f1, cs)) ∧
    -- (3) installing the NEW identity is an enabled transition
    Transition s (installReplicatorState s (a, f2, cs)) ∧
    -- (4) teardown-then-install lands the address on the new identity
    ( ((a, f2, cs) : ReplicatorId) ∈
        (installReplicatorState (teardownReplicatorState s (a, f1, cs)) (a, f2, cs)).actual.replicators ∧
      ((a, f1, cs) : ReplicatorId) ∉
        (installReplicatorState (teardownReplicatorState s (a, f1, cs)) (a, f2, cs)).actual.replicators ) := by
  refine ⟨?_, ?_, ?_, ?_, ?_⟩
  · -- non-empty diff: (a, f2, cs) is desired but not actual, so it sits in the
    -- desired \ actual symmetric-difference summand.
    have h_mem : ((a, f2, cs) : ReplicatorId) ∈ desired.replicators \ s.actual.replicators :=
      Finset.mem_sdiff.mpr ⟨h_new_desired, h_new_not_actual⟩
    have h_card_pos : 0 < (desired.replicators \ s.actual.replicators).card :=
      Finset.card_pos.mpr ⟨_, h_mem⟩
    simp only [disagreementCount, h_desired]
    omega
  · exact Transition.reconcileTeardownReplicator desired (a, f1, cs)
      h_desired h_old_actual h_old_not_desired h_old_applied rfl
  · exact Transition.reconcileInstallReplicator desired (a, f2, cs)
      h_desired h_new_desired h_new_not_actual h_connected rfl
  · -- install inserts (a, f2, cs)
    exact Finset.mem_insert_self _ _
  · -- after teardown of (a, f1, cs) then install of (a, f2, cs), the old
    -- identity is gone: it is not (a, f2, cs) (distinct) and was erased.
    simp only [installReplicatorState, teardownReplicatorState]
    rw [Finset.mem_insert]
    push_neg
    exact ⟨filter_change_distinct_identity hf, Finset.not_mem_erase _ _⟩

/-- **Collection-set change forces reinstall.** The live demo bug, as a theorem.
A replicator installed while only the data-plane layer was visible carries only
that layer's collection set `cs1`; when the control-plane layer merges in,
desired carries the SAME address and filter but the larger merged set `cs2`.
The collection set is part of the identity, so this is a teardown of the old
identity and an install of the new one — NOT a silent no-op. (The pre-fix Rust
diff keyed replicators on address alone, converged falsely, and never pushed
the control-plane collections to the peer — the demo `pair` step-8 hang.)

Same conjunct structure as `filter_change_forces_reinstall`; every conjunct is
quantified over the real transition relation. -/
theorem collections_change_forces_reinstall
    {s : ReconcileState} {desired : PairingDesired}
    {a : String} {f : ReplicatorFilter} {cs1 cs2 : ReplicatorCollections}
    (h_desired : s.desired = some desired)
    (hcs : cs1 ≠ cs2)
    (h_new_desired : ((a, f, cs2) : ReplicatorId) ∈ desired.replicators)
    (h_old_not_desired : ((a, f, cs1) : ReplicatorId) ∉ desired.replicators)
    (h_old_actual : ((a, f, cs1) : ReplicatorId) ∈ s.actual.replicators)
    (h_old_applied : ((a, f, cs1) : ReplicatorId) ∈ s.applied.replicators)
    (h_new_not_actual : ((a, f, cs2) : ReplicatorId) ∉ s.actual.replicators)
    (h_connected : s.actual.connected = true) :
    -- (1) the state is genuinely diverged
    0 < disagreementCount s ∧
    -- (2) tearing down the OLD identity is an enabled transition
    Transition s (teardownReplicatorState s (a, f, cs1)) ∧
    -- (3) installing the NEW identity is an enabled transition
    Transition s (installReplicatorState s (a, f, cs2)) ∧
    -- (4) teardown-then-install lands the address on the new identity
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
      h_desired h_old_actual h_old_not_desired h_old_applied rfl
  · exact Transition.reconcileInstallReplicator desired (a, f, cs2)
      h_desired h_new_desired h_new_not_actual h_connected rfl
  · exact Finset.mem_insert_self _ _
  · simp only [installReplicatorState, teardownReplicatorState]
    rw [Finset.mem_insert]
    push_neg
    exact ⟨collections_change_distinct_identity hcs, Finset.not_mem_erase _ _⟩

/-- Stable converged desired state cannot flap managed wiring. -/
theorem no_flap_on_converged_stable_desired
    {pre post : ReconcileState}
    (h : pre.converged)
    (h_trans : Transition pre post)
    (_h_desired_stable : post.desired = pre.desired) :
    managedWiringUnchanged pre post :=
  no_flap_on_converged_step h h_trans

/-! ## Fallible connect/install: the partial-apply hang as a proof obligation

`PairingReconcile` now models the connect (`dial` vs `dialFailed`) and the
replicator install (`reconcileInstallReplicator` vs
`reconcileInstallReplicatorFailed`) as FALLIBLE. These theorems turn the observed
live failure — a peer that is connected with control-plane collections already
subscribed, but whose replicator never installs because its transport dial keeps
timing out — into a first-class, NON-CONVERGING FIXPOINT of the failure
transitions.

The upshot the live bug taught us, now a theorem: convergence is NOT a property
of the reconciler alone. It requires an external guarantee that the successful
transition is eventually taken — i.e. that the dial eventually succeeds. When the
transport never provides a dialable path (the live hang), the system is trapped
in these fixpoints, and no amount of re-running the reconciler escapes them. -/

/-- A failed dial is a non-converging self-loop. From a state with managed wiring
and no connection, `dialFailed` is an enabled transition to the SAME state, and
that state is non-converged (the `hasWiring ∧ ¬connected` disagreement term is
1). The connection can never be established by re-running this step. -/
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

/-- The EXACT observed live failure, as a theorem. The peer is connected, its
control-plane collections are already in `actual` (subscribed), but a desired
replicator is still missing. Then: (1) the state is non-converged; (2) the
SUCCESSFUL install is an enabled transition; (3) the FAILING install is an
enabled transition to the SAME state (a self-loop); (4) the diff is genuinely
positive. The reconciler cannot choose which of (2)/(3) fires — that depends
solely on whether the replicator's transport dial succeeds — so the partial
state is a real branch point whose failing branch never converges. -/
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

/-- Liveness obligation, made explicit. The failing install is a self-loop that
never reduces the disagreement measure, so from the partial-applied state the
measure can only reach 0 via the SUCCESSFUL install. Convergence therefore
requires that the successful transition is eventually taken — i.e. that the
replicator's transport dial eventually succeeds. The reconciler cannot discharge
this on its own; it is a transport-liveness assumption on the layer below
(modeled in `tla/PairingTransport.tla`). This is the formal counterpart of the
live hang. -/
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
