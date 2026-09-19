import Proofs.CanonicalOutput.Execution.Transition

namespace CanonicalOutput.Execution

open RequestExecutionLease

theorem checked_success (predicate : World → Bool)
    (result : Except Error World) (post : World)
    (h : checked predicate result = .ok post) : predicate post = true := by
  cases hresult : result with
  | error error => simp [checked, hresult] at h
  | ok world =>
      simp only [checked, hresult] at h
      split at h
      · rename_i hpredicate
        cases h
        exact hpredicate
      · contradiction

theorem renew_success_is_exact_lease_cas
    (world post : World) (generation : Generation) (expectedDeadline : Time)
    (h : renew world generation expectedDeadline = .ok post) :
    ∃ lease, RequestExecutionLease.step? world.lease
        (.renew .mutationWriteGate generation expectedDeadline) = some lease ∧
      post = { world with lease := lease } := by
  unfold renew renewCore at h
  split at h
  · contradiction
  · rename_i lease hlease
    cases h
    exact ⟨lease, hlease, rfl⟩

theorem renew_preserves_canonical_output
    (world post : World) (generation : Generation) (expectedDeadline : Time)
    (h : renew world generation expectedDeadline = .ok post) :
    post.segments = world.segments ∧ post.messages = world.messages ∧
      post.transcript = world.transcript ∧ post.delegatedCalls = world.delegatedCalls := by
  obtain ⟨lease, _, rfl⟩ := renew_success_is_exact_lease_cas world post generation
    expectedDeadline h
  exact ⟨rfl, rfl, rfl, rfl⟩

/-- Renewal admission and its resulting lease depend only on the lease owner
state. Arbitrary canonical records—including conflicting or future-dated
ones—cannot veto the explicit heartbeat CAS. -/
theorem renew_lease_result_independent_of_output
    (left right : World) (generation : Generation) (expectedDeadline : Time)
    (hlease : left.lease = right.lease) :
    (renew left generation expectedDeadline).map (fun post => post.lease) =
      (renew right generation expectedDeadline).map (fun post => post.lease) := by
  simp only [renew, renewCore, hlease]
  cases RequestExecutionLease.step? right.lease
      (.renew .mutationWriteGate generation expectedDeadline) <;> rfl

theorem renewal_due_is_admitted
    (world : World) (generation : Generation) (expectedDeadline : Time)
    (h : renewalEligibility world generation expectedDeadline = .due) :
    ∃ post, renew world generation expectedDeadline = .ok post := by
  cases hlease : world.lease.lease with
  | active owner duration deadline =>
      simp only [renewalEligibility, hlease] at h
      split at h <;> try contradiction
      split at h <;> try contradiction
      split at h <;> try contradiction
      split at h <;> try contradiction
      split at h <;> try contradiction
      cases hrequest : world.lease.request <;>
        simp_all [renew, renewCore, RequestExecutionLease.step?,
          RequestExecutionLease.admitted, RequestExecutionLease.renewDeadline,
          RequestExecutionLease.renewableLifecycle]
  | vacant | recoverable | terminal => simp [renewalEligibility, hlease] at h

theorem appendRaw_preserves_request_control
    (pre post : World) (generation : Generation) (record : Segment)
    (h : appendRaw pre generation record = .ok post) :
    record ∈ post.segments ∧ post.lease.request = pre.lease.request ∧
      post.lease.lease = pre.lease.lease := by
  unfold appendRaw at h
  have hp := checked_success _ _ _ h
  simp only [Bool.and_eq_true, decide_eq_true_eq, beq_iff_eq] at hp
  exact ⟨hp.1.1, hp.1.2, hp.2⟩

theorem exact_raw_replay_core_is_identity
    (world post : World) (generation : Generation) (record : Segment)
    (hcollision : segmentIdentityCollision world record = false)
    (hmem : record ∈ world.segments)
    (hshape : rawReplayShape world generation record = true)
    (h : appendRawCore world generation record = .ok post) :
    post.segments = world.segments ∧ post.messages = world.messages ∧
      post.transcript = world.transcript ∧ post.delegatedCalls = world.delegatedCalls := by
  simp only [appendRawCore, hcollision, Bool.false_eq_true, ↓reduceIte, hmem, hshape] at h
  cases h
  exact ⟨rfl, rfl, rfl, rfl⟩

theorem retraction_precedes_retry
    (pre post : World) (generation : Generation) (record : Segment)
    (h : retractBeforeRetry pre generation record = .ok post) :
    record ∈ post.segments ∧
      (closures post.segments record.coordinate).dedup = [record] := by
  unfold retractBeforeRetry at h
  have hp := checked_success _ _ _ h
  simpa only [Bool.and_eq_true, decide_eq_true_eq, beq_iff_eq] using hp

theorem accepted_publication_is_composed_atomically
    (pre post : World) (generation : Generation) (closing : Segment)
    (message : MessageEnvelope) (targets : List RemoteTarget)
    (h : acceptAndPublish pre generation closing message targets = .ok post) :
    acceptedPublicationPresent post closing message targets = true ∧
      remoteTargetsMatchConfiguredRoutes post message targets = true ∧
      validateClosingRecord post.segments closing = true ∧
      acceptedMessageValid post generation post.segments message = true ∧
      acceptedSourceBound post generation closing message = true := by
  unfold acceptAndPublish at h
  have hp := checked_success _ _ _ h
  simp only [Bool.and_eq_true] at hp
  exact ⟨hp.1.1.1.1, hp.1.1.1.2, hp.1.1.2, hp.1.2, hp.2⟩

theorem fresh_accept_core_success_requires_exact_extent
    (world post : World) (generation : Generation) (closing : Segment)
    (message : MessageEnvelope) (targets : List RemoteTarget)
    (hnotReplay : acceptedPublicationPresent world closing message targets = false)
    (h : acceptAndPublishCore world generation closing message targets = .ok post) :
    freshCompleteExtentExact (world.segments ++ [closing]) closing = true := by
  by_contra hnotExact
  have hexact : freshCompleteExtentExact (world.segments ++ [closing]) closing = false :=
    Bool.eq_false_of_not_eq_true hnotExact
  simp (config := { maxSteps := 1000000 })
    [acceptAndPublishCore, hnotReplay, hexact] at h
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> contradiction

theorem dispatch_requires_committed_intent_and_marks_running
    (pre post : World) (generation : Generation)
    (permit : DispatchPermit)
    (h : dispatch pre generation permit = .ok post) :
    post.transcript.RunningPublishedCall permit.call ∧
      dispatchPublicationValid post generation permit.call = true := by
  unfold dispatch at h
  have hp := checked_success _ _ _ h
  simpa only [Bool.and_eq_true, decide_eq_true_eq] using hp

theorem authored_publication_is_atomic_and_headed
    (pre post : World) (generation : Generation) (closing : Segment)
    (message : MessageEnvelope)
    (h : publishAuthored pre generation closing message = .ok post) :
    authoredPublicationPresent post closing message = true ∧
      validateClosingRecord post.segments closing = true ∧
      authoredMessageValid post generation post.segments closing message = true := by
  unfold publishAuthored at h
  have hp := checked_success _ _ _ h
  simp only [Bool.and_eq_true] at hp
  exact ⟨hp.1.1, hp.1.2, hp.2⟩

theorem fresh_authored_core_success_requires_exact_extent
    (world post : World) (generation : Generation) (closing : Segment)
    (message : MessageEnvelope)
    (hnotReplay : authoredPublicationPresent world closing message = false)
    (h : publishAuthoredCore world generation closing message = .ok post) :
    freshCompleteExtentExact (world.segments ++ [closing]) closing = true := by
  by_contra hnotExact
  have hexact : freshCompleteExtentExact (world.segments ++ [closing]) closing = false :=
    Bool.eq_false_of_not_eq_true hnotExact
  simp (config := { maxSteps := 1000000 })
    [publishAuthoredCore, hnotReplay, hexact] at h
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> contradiction

theorem fresh_complete_extent_accounts_for_all_data
    (segments : List Segment) (closing : Segment)
    (count : Nat) (bytes : List Nat)
    (hclose : closing.close = some (.closed .complete count bytes))
    (hexact : freshCompleteExtentExact segments closing = true) :
    count = (sourceData segments closing.coordinate).length ∧
      timestampsNondecreasing (sourceData segments closing.coordinate) = true ∧
      (sourceData segments closing.coordinate).all
        (fun record => record.createdAt ≤ closing.createdAt) = true := by
  simp [freshCompleteExtentExact, hclose] at hexact
  exact ⟨hexact.1.1.1, hexact.1.2, List.all_eq_true.mpr (fun record hrecord =>
    decide_eq_true (hexact.2 record hrecord))⟩

theorem header_only_publication_is_atomic
    (pre post : World) (generation : Generation) (message : MessageEnvelope)
    (h : publishHeaderOnly pre generation message = .ok post) :
    headerOnlyPublicationPresent post message = true ∧
      headerOnlyMessageValid post generation message = true := by
  unfold publishHeaderOnly at h
  simpa only [Bool.and_eq_true] using (checked_success _ _ _ h)

theorem recovery_is_all_sources_single_winner_and_exact
    (pre post : World) (expected next : Generation)
    (duration deadline : Time) (items : List RecoveryItem)
    (h : recoverExpiredBatch pre expected next duration deadline items = .ok post) :
    recoveryBatchPresent post next items = true ∧
      (recoveryReplayValid pre expected next items ||
        recoveryCoversAllSources pre expected items) = true ∧
      items.all (fun item =>
        (closures post.segments item.closing.coordinate).dedup == [item.closing] &&
          item.closing.writer == .request expected &&
          (recoveryReplayValid pre expected next items ||
            recoveryExtentExact pre expected item.closing)) = true := by
  unfold recoverExpiredBatch at h
  have hp := checked_success _ _ _ h
  simp only [Bool.and_eq_true] at hp
  exact ⟨hp.1.1, hp.1.2, hp.2⟩

theorem exact_recovery_replay_core_does_not_renew_or_republish
    (world post : World) (expected fresh : Generation)
    (duration deadline : Time) (items : List RecoveryItem)
    (hreplay : recoveryReplayValid world expected fresh items = true)
    (h : recoverExpiredBatchCore world expected fresh duration deadline items = .ok post) :
    post.lease.lease = world.lease.lease ∧ post.segments = world.segments ∧
      post.messages = world.messages ∧ post.transcript = world.transcript := by
  simp only [recoverExpiredBatchCore, hreplay, ↓reduceIte] at h
  cases h
  exact ⟨rfl, rfl, rfl, rfl⟩

theorem terminal_selection_commits_with_lifecycle
    (pre post : World) (generation : Generation)
    (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection)
    (h : terminalize pre generation outcome selection = .ok post) :
    terminalReplayPresent post generation outcome selection = true ∧
      terminalSelectionValid post selection = true ∧ ownedPendingSettled post = true := by
  unfold terminalize at h
  have hp := checked_success _ _ _ h
  simp only [Bool.and_eq_true] at hp
  exact ⟨hp.1.1, hp.1.2, hp.2⟩

theorem admitted_terminal_core_commits_lifecycle_and_pending_cancellation
    (world : World)
    (lease : RequestExecutionLease.World Generation)
    (generation : Generation) (outcome : RequestExecutionLease.Outcome)
    (selection : TerminalSelection)
    (hvalid : terminalSelectionValid world selection = true)
    (hnotReplay : terminalReplayPresent world generation outcome selection = false)
    (hempty : world.terminalSelection.isSome = false)
    (hlease : RequestExecutionLease.step? world.lease
      (.finalize .mutationWriteGate generation outcome) = some lease) :
    terminalizeCore world generation outcome selection = .ok
      { world with
        lease := lease
        transcript := terminalizeOwnedPending world
        terminalSelection := some selection } := by
  simp [terminalizeCore, hvalid, hnotReplay, hempty, hlease]

theorem no_message_requires_no_eligible_owned_assistant
    (world : World)
    (h : terminalSelectionValid world .noMessage = true) :
    eligibleOwnedAssistantExists world = false := by
  simpa [terminalSelectionValid] using h

/-- Once the executable all-source batch has prepared and the lease gate admits
the swap, the composed core reaches the exact atomic post-state. This is a
reachability equation, not a scheduler-fairness or cross-process transaction
premise. -/
theorem prepared_batch_enables_composed_recovery
    (world : World) (prepared : RecoveryPrepared)
    (lease : RequestExecutionLease.World Generation)
    (expected next : Generation) (duration deadline : Time) (items : List RecoveryItem)
    (hnotReplay : recoveryReplayValid world expected next items = false)
    (hprepare : prepareRecoveryBatch world expected next items = .ok prepared)
    (hlease : RequestExecutionLease.step? world.lease
      (.recoverExpired .mutationWriteGate expected next duration deadline) = some lease) :
    recoverExpiredBatchCore world expected next duration deadline items =
      .ok { world with
        lease := lease
        segments := prepared.segments
        messages := prepared.messages
        transcript := prepared.transcript } := by
  simp [recoverExpiredBatchCore, hnotReplay, hprepare, hlease]

end CanonicalOutput.Execution
