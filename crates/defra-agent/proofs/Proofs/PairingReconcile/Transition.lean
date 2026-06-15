import Proofs.PairingReconcile.State

/-!
# Pairing Reconcile Transitions

One transition per supervisor-observable step. Reconcile actions move actual
collections toward desired collections. Operator writes change desired state.
Crashes clear only in-memory retry visibility.
-/

namespace PairingReconcile

def installCollectionState (pre : ReconcileState) (c : String) : ReconcileState :=
  { pre with
    actual := ({
      collections := insert c pre.actual.collections,
      replicators := pre.actual.replicators,
      connected := pre.actual.connected
    } : PairingActual),
    applied := ({ collections := insert c pre.applied.collections, replicators := pre.applied.replicators } : PairingApplied) }

def teardownCollectionState (pre : ReconcileState) (c : String) : ReconcileState :=
  { pre with
    actual := ({
      collections := pre.actual.collections.erase c,
      replicators := pre.actual.replicators,
      connected := pre.actual.connected
    } : PairingActual),
    applied := ({ collections := pre.applied.collections.erase c, replicators := pre.applied.replicators } : PairingApplied) }

def installReplicatorState (pre : ReconcileState) (r : ReplicatorId) : ReconcileState :=
  { pre with
    actual := ({
      collections := pre.actual.collections,
      replicators := insert r pre.actual.replicators,
      connected := pre.actual.connected
    } : PairingActual),
    applied := ({ collections := pre.applied.collections, replicators := insert r pre.applied.replicators } : PairingApplied) }

def teardownReplicatorState (pre : ReconcileState) (r : ReplicatorId) : ReconcileState :=
  { pre with
    actual := ({
      collections := pre.actual.collections,
      replicators := pre.actual.replicators.erase r,
      connected := pre.actual.connected
    } : PairingActual),
    applied := ({ collections := pre.applied.collections, replicators := pre.applied.replicators.erase r } : PairingApplied) }

def dialState (pre : ReconcileState) : ReconcileState :=
  { pre with
    actual := ({
      collections := pre.actual.collections,
      replicators := pre.actual.replicators,
      connected := true
    } : PairingActual) }

def disconnectedState (pre : ReconcileState) : ReconcileState :=
  { pre with
    actual := ({
      collections := pre.actual.collections,
      replicators := pre.actual.replicators,
      connected := false
    } : PairingActual) }

inductive Transition : ReconcileState → ReconcileState → Prop where
  | operatorWrite {pre post : ReconcileState} (newDesired : PairingDesired) :
      some newDesired ≠ pre.desired →
      post = { pre with desired := some newDesired } →
      Transition pre post
  | operatorDelete {pre post : ReconcileState} :
      post = { pre with desired := some {
        collections := ∅
        replicators := ∅
      } } →
      Transition pre post
  | readFailure {pre post : ReconcileState} :
      post = { pre with desired := none } →
      Transition pre post
  | dial {pre post : ReconcileState} (desired : PairingDesired) :
      pre.desired = some desired →
      desired.hasWiring = true →
      pre.actual.connected = false →
      post = dialState pre →
      Transition pre post
  | peerDisconnected {pre post : ReconcileState} :
      pre.actual.connected = true →
      post = disconnectedState pre →
      Transition pre post
  | reconcileInstall {pre post : ReconcileState} (desired : PairingDesired) (c : String) :
      pre.desired = some desired →
      c ∈ desired.collections →
      c ∉ pre.actual.collections →
      pre.actual.connected = true →
      post = installCollectionState pre c →
      Transition pre post
  | reconcileTeardown {pre post : ReconcileState} (desired : PairingDesired) (c : String) :
      pre.desired = some desired →
      c ∈ pre.actual.collections →
      c ∉ desired.collections →
      c ∈ pre.applied.collections →
      post = teardownCollectionState pre c →
      Transition pre post
  | reconcileInstallReplicator {pre post : ReconcileState} (desired : PairingDesired) (r : ReplicatorId) :
      pre.desired = some desired →
      r ∈ desired.replicators →
      r ∉ pre.actual.replicators →
      pre.actual.connected = true →
      post = installReplicatorState pre r →
      Transition pre post
  | reconcileTeardownReplicator {pre post : ReconcileState} (desired : PairingDesired) (r : ReplicatorId) :
      pre.desired = some desired →
      r ∈ pre.actual.replicators →
      r ∉ desired.replicators →
      r ∈ pre.applied.replicators →
      post = teardownReplicatorState pre r →
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
    (h_post : post = installCollectionState pre c) :
    c ∈ post.actual.collections := by
  cases h_post
  exact Finset.mem_insert_self c pre.actual.collections

theorem reconcileTeardown_removes_target
    {pre post : ReconcileState} {c : String}
    (h_post : post = teardownCollectionState pre c) :
    c ∉ post.actual.collections := by
  cases h_post
  exact Finset.not_mem_erase c pre.actual.collections

theorem reconcileInstallReplicator_adds_target
    {pre post : ReconcileState} {r : ReplicatorId}
    (h_post : post = installReplicatorState pre r) :
    r ∈ post.actual.replicators := by
  cases h_post
  exact Finset.mem_insert_self r pre.actual.replicators

theorem reconcileTeardownReplicator_removes_target
    {pre post : ReconcileState} {r : ReplicatorId}
    (h_post : post = teardownReplicatorState pre r) :
    r ∉ post.actual.replicators := by
  cases h_post
  exact Finset.not_mem_erase r pre.actual.replicators

theorem readFailure_preserves_actual_applied
    {pre post : ReconcileState}
    (h_trans : Transition pre post)
    (h_readFailure : ∃ h, h_trans = Transition.readFailure h) :
    post.actual = pre.actual ∧ post.applied = pre.applied := by
  rcases h_readFailure with ⟨h_post, h_eq⟩
  subst h_eq
  cases h_post
  exact ⟨rfl, rfl⟩

theorem unmanaged_collection_survives
    {pre post : ReconcileState} (h_trans : Transition pre post)
    {c : String} (hc : c ∈ pre.actual.collections)
    (hunmanaged : c ∉ pre.applied.collections) :
    c ∈ post.actual.collections := by
  cases h_trans with
  | operatorWrite newDesired h_ne h_post =>
      cases h_post
      exact hc
  | operatorDelete h_post =>
      cases h_post
      exact hc
  | readFailure h_post =>
      cases h_post
      exact hc
  | dial desired h_desired h_has_wiring h_disconnected h_post =>
      cases h_post
      exact hc
  | peerDisconnected h_connected h_post =>
      cases h_post
      exact hc
  | reconcileInstall desired target h_desired h_target h_missing h_connected h_post =>
      cases h_post
      exact Finset.mem_insert_of_mem hc
  | reconcileTeardown desired target h_desired h_actual h_not_desired h_applied h_post =>
      cases h_post
      by_cases h_eq : c = target
      · subst h_eq
        exact False.elim (hunmanaged h_applied)
      · exact Finset.mem_erase.mpr ⟨h_eq, hc⟩
  | reconcileInstallReplicator desired target h_desired h_target h_missing h_connected h_post =>
      cases h_post
      exact hc
  | reconcileTeardownReplicator desired target h_desired h_actual h_not_desired h_applied h_post =>
      cases h_post
      exact hc
  | crash h_post =>
      cases h_post
      exact hc

theorem unmanaged_replicator_survives
    {pre post : ReconcileState} (h_trans : Transition pre post)
    {r : ReplicatorId} (hr : r ∈ pre.actual.replicators)
    (hunmanaged : r ∉ pre.applied.replicators) :
    r ∈ post.actual.replicators := by
  cases h_trans with
  | operatorWrite newDesired h_ne h_post =>
      cases h_post
      exact hr
  | operatorDelete h_post =>
      cases h_post
      exact hr
  | readFailure h_post =>
      cases h_post
      exact hr
  | dial desired h_desired h_has_wiring h_disconnected h_post =>
      cases h_post
      exact hr
  | peerDisconnected h_connected h_post =>
      cases h_post
      exact hr
  | reconcileInstall desired target h_desired h_target h_missing h_connected h_post =>
      cases h_post
      exact hr
  | reconcileTeardown desired target h_desired h_actual h_not_desired h_applied h_post =>
      cases h_post
      exact hr
  | reconcileInstallReplicator desired target h_desired h_target h_missing h_connected h_post =>
      cases h_post
      exact Finset.mem_insert_of_mem hr
  | reconcileTeardownReplicator desired target h_desired h_actual h_not_desired h_applied h_post =>
      cases h_post
      by_cases h_eq : r = target
      · subst h_eq
        exact False.elim (hunmanaged h_applied)
      · exact Finset.mem_erase.mpr ⟨h_eq, hr⟩
  | crash h_post =>
      cases h_post
      exact hr

end PairingReconcile
