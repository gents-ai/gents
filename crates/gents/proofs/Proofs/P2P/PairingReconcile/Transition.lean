import Proofs.PairingReconcile.State

namespace PairingReconcile

def installReplicatorState (pre : ReconcileState) (r : ReplicatorId) : ReconcileState :=
  { pre with
    actual := ({
      replicators := insert r pre.actual.replicators,
      connected := pre.actual.connected
    } : PairingActual),
    applied := ({ replicators := insert r pre.applied.replicators } : PairingApplied) }

def teardownReplicatorState (pre : ReconcileState) (r : ReplicatorId) : ReconcileState :=
  { pre with
    actual := ({
      replicators := eraseReplicatorsAtAddress pre.actual.replicators r.address,
      connected := pre.actual.connected
    } : PairingActual),
    applied := ({
      replicators := eraseReplicatorsAtAddress pre.applied.replicators r.address
    } : PairingApplied) }

def dialState (pre : ReconcileState) : ReconcileState :=
  { pre with
    actual := ({
      replicators := pre.actual.replicators,
      connected := true
    } : PairingActual) }

def disconnectedState (pre : ReconcileState) : ReconcileState :=
  { pre with
    actual := ({
      replicators := pre.actual.replicators,
      connected := false
    } : PairingActual) }

inductive Transition : ReconcileState → ReconcileState → Prop where
  | operatorWrite {pre post : ReconcileState} (newDesired : PairingDesired) :
      some newDesired ≠ pre.desired →
      post = { pre with desired := some newDesired } →
      Transition pre post
  | operatorDelete {pre post : ReconcileState} :
      post = { pre with desired := some { replicators := ∅ } } →
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
  | dialFailed {pre post : ReconcileState} (desired : PairingDesired) :
      pre.desired = some desired →
      desired.hasWiring = true →
      pre.actual.connected = false →
      post = pre →
      Transition pre post
  | reconcileInstallReplicator {pre post : ReconcileState} (desired : PairingDesired) (r : ReplicatorId) :
      pre.desired = some desired →
      r ∈ desired.replicators →
      r ∉ pre.actual.replicators →
      pre.actual.connected = true →
      post = installReplicatorState pre r →
      Transition pre post
  | reconcileInstallReplicatorFailed {pre post : ReconcileState} (desired : PairingDesired) (r : ReplicatorId) :
      pre.desired = some desired →
      r ∈ desired.replicators →
      r ∉ pre.actual.replicators →
      pre.actual.connected = true →
      post = pre →
      Transition pre post
  | reconcileTeardownReplicator {pre post : ReconcileState} (desired : PairingDesired) (r : ReplicatorId) :
      pre.desired = some desired →
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
  simp [teardownReplicatorState, eraseReplicatorsAtAddress]

theorem reconcileTeardownReplicator_removes_endpoint_drift
    {pre post : ReconcileState} {owned observed : ReplicatorId}
    (h_same : observed.address = owned.address)
    (h_post : post = teardownReplicatorState pre owned) :
    observed ∉ post.actual.replicators := by
  cases h_post
  simp [teardownReplicatorState, eraseReplicatorsAtAddress, h_same]

theorem readFailure_preserves_actual_applied
    {pre post : ReconcileState}
    (h_trans : Transition pre post)
    (h_readFailure : ∃ h, h_trans = Transition.readFailure h) :
    post.actual = pre.actual ∧ post.applied = pre.applied := by
  rcases h_readFailure with ⟨h_post, h_eq⟩
  subst h_eq
  cases h_post
  exact ⟨rfl, rfl⟩

theorem unmanaged_replicator_survives
    {pre post : ReconcileState} (h_trans : Transition pre post)
    {r : ReplicatorId} (hr : r ∈ pre.actual.replicators)
    (hunmanaged : ∀ owned ∈ pre.applied.replicators, r.address ≠ owned.address) :
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
  | dialFailed desired h_desired h_has_wiring h_disconnected h_post =>
      cases h_post
      exact hr
  | peerDisconnected h_connected h_post =>
      cases h_post
      exact hr
  | reconcileInstallReplicator desired target h_desired h_target h_missing h_connected h_post =>
      cases h_post
      exact Finset.mem_insert_of_mem hr
  | reconcileInstallReplicatorFailed desired target h_desired h_target h_missing h_connected h_post =>
      cases h_post
      exact hr
  | reconcileTeardownReplicator desired target h_desired h_not_desired h_applied h_post =>
      cases h_post
      exact Finset.mem_filter.mpr ⟨hr, by
        simpa using hunmanaged target h_applied⟩
  | crash h_post =>
      cases h_post
      exact hr

end PairingReconcile
