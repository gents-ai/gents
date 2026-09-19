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

theorem deriveProgress_scopes_every_fact
    (world : World) (generation : Generation)
    (facts : List (OutputFact Generation))
    (h : deriveProgress world generation = .ok facts) :
    ∀ fact, fact ∈ facts →
      fact.generation = generation ∧ fact.eligibility = .currentRequest ∧
        factBackedByCanonicalRecord world generation fact = true := by
  unfold deriveProgress at h
  split at h
  · contradiction
  · split at h
    · contradiction
    · cases hcollect : collectProgress world generation (requestCoordinates world) with
      | error error => simp [hcollect] at h
      | ok collected =>
          simp only [hcollect] at h
          split at h
          · rename_i hbacked
            cases h
            intro fact hmem
            simp only [List.mem_map] at hmem
            rcases hmem with ⟨original, horiginal, rfl⟩
            refine ⟨rfl, rfl, ?_⟩
            exact List.all_eq_true.mp hbacked _
              (List.mem_map.mpr ⟨original, horiginal, rfl⟩)
          · contradiction

theorem authoritativeLease_uses_only_derived_progress
    (world : World) (lease : RequestExecutionLease.World Generation)
    (generation : Generation) (duration deadline : Time)
    (hactive : world.lease.lease = .active generation duration deadline)
    (h : authoritativeLease world = .ok lease) :
    deriveProgress world generation = .ok lease.output := by
  unfold authoritativeLease at h
  rw [hactive] at h
  cases hprogress : deriveProgress world generation with
  | error error =>
      simp only [hprogress, Bind.bind, Except.bind] at h
      contradiction
  | ok output => simp [hprogress] at h; cases h; rfl

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
  simp only [appendRawCore, hcollision, Bool.false_eq_true, ↓reduceIte, hmem, hshape,
    synchronize] at h
  cases hauthoritative : authoritativeLease world with
  | error error => simp [hauthoritative] at h
  | ok lease =>
      simp [hauthoritative] at h
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
  simp only [recoverExpiredBatchCore, hreplay, ↓reduceIte, synchronize] at h
  cases hauthoritative : authoritativeLease world with
  | error error => simp [hauthoritative] at h
  | ok lease =>
      simp [hauthoritative] at h
      cases h
      have hcontrol : lease.lease = world.lease.lease := by
        unfold authoritativeLease at hauthoritative
        cases hworld : world.lease.lease with
        | vacant => simp [hworld] at hauthoritative; cases hauthoritative; rfl
        | active generation duration deadline =>
            rw [hworld] at hauthoritative
            cases hprogress : deriveProgress world generation with
            | error error =>
                simp only [hprogress, Bind.bind, Except.bind] at hauthoritative
                contradiction
            | ok output =>
                simp only [hprogress, Bind.bind, Except.bind, Except.ok.injEq] at hauthoritative
                cases hauthoritative
                exact hworld
        | recoverable generation duration deadline =>
            rw [hworld] at hauthoritative
            cases hprogress : deriveProgress world generation with
            | error error =>
                simp only [hprogress, Bind.bind, Except.bind] at hauthoritative
                contradiction
            | ok output =>
                simp only [hprogress, Bind.bind, Except.bind, Except.ok.injEq] at hauthoritative
                cases hauthoritative
                exact hworld
        | terminal generation outcome =>
            simp [hworld] at hauthoritative
            cases hauthoritative
            rfl
      exact ⟨hcontrol, rfl, rfl, rfl⟩

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
    (world authoritative : World)
    (lease : RequestExecutionLease.World Generation)
    (generation : Generation) (outcome : RequestExecutionLease.Outcome)
    (selection : TerminalSelection)
    (hvalid : terminalSelectionValid world selection = true)
    (hnotReplay : terminalReplayPresent world generation outcome selection = false)
    (hempty : world.terminalSelection.isSome = false)
    (hauthoritative : synchronize world = .ok authoritative)
    (hlease : RequestExecutionLease.step? authoritative.lease
      (.finalize .mutationWriteGate generation outcome) = some lease) :
    terminalizeCore world generation outcome selection = .ok
      { authoritative with
        lease := lease
        transcript := terminalizeOwnedPending authoritative
        terminalSelection := some selection } := by
  simp [terminalizeCore, hvalid, hnotReplay, hempty, hauthoritative, hlease]

theorem no_message_requires_no_eligible_owned_assistant
    (world : World)
    (h : terminalSelectionValid world .noMessage = true) :
    eligibleOwnedAssistantExists world = false := by
  simpa [terminalSelectionValid] using h

def newestCreated : List (OutputFact Generation) → Time
  | [] => 0
  | fact :: rest => max fact.createdAt (newestCreated rest)

theorem createdAt_le_newest (facts : List (OutputFact Generation))
    (fact : OutputFact Generation) (h : fact ∈ facts) :
    fact.createdAt ≤ newestCreated facts := by
  induction facts with
  | nil => simp at h
  | cons first rest ih =>
      simp only [List.mem_cons] at h
      rcases h with rfl | h
      · exact Nat.le_max_left _ _
      · exact Nat.le_trans (ih h) (Nat.le_max_right _ _)

def silentRecoveryTime (lease : RequestExecutionLease.World Generation) : Time :=
  max (effectiveExpiry lease) (newestCreated lease.output)

theorem finite_silence_enables_atomic_recovery
    (lease : RequestExecutionLease.World Generation)
    (expected next : Generation) (oldDuration oldDeadline duration deadline : Time)
    (hlease : lease.lease = .active expected oldDuration oldDeadline)
    (hscoped : ∀ fact, fact ∈ lease.output →
      fact.generation = expected ∧ fact.eligibility = .currentRequest)
    (hfresh : RequestExecutionLease.fresh lease next) (hduration : duration > 0)
    (hdeadline : silentRecoveryTime lease < deadline) :
    ∃ post, RequestExecutionLease.step?
      { lease with now := silentRecoveryTime lease }
      (.recoverExpired .mutationWriteGate expected next duration deadline) = some post := by
  let quiet := { lease with now := silentRecoveryTime lease }
  have hintegrity : integrityHealthy quiet := by
    unfold integrityHealthy quiet
    apply List.all_eq_true.mpr
    intro fact hfact
    have hscopeFact := hscoped fact hfact
    simp [hscopeFact.2, OutputEligibility.isConflict]
  have hclock : clockCoherent quiet expected := by
    unfold clockCoherent quiet
    apply List.all_eq_true.mpr
    intro fact hfact
    have hbound := createdAt_le_newest lease.output fact hfact
    have hmax : newestCreated lease.output ≤ silentRecoveryTime lease :=
      Nat.le_max_right _ _
    simp only [eligibleFor]
    split
    · exact decide_eq_true (Nat.le_trans hbound hmax)
    · trivial
  have hexpired : effectiveExpiry quiet ≤ quiet.now := by
    rw [show effectiveExpiry quiet = effectiveExpiry lease by simp [quiet, effectiveExpiry, hlease]]
    exact Nat.le_max_left _ _
  have hquietLease : quiet.lease = .active expected oldDuration oldDeadline := by
    simpa [quiet] using hlease
  exact expired_recovery_enabled quiet expected next oldDuration oldDeadline duration deadline
    hquietLease hintegrity hclock hexpired hduration
    (by simpa [quiet, RequestExecutionLease.fresh] using hfresh)
    (by simpa [quiet] using hdeadline)

theorem derived_finite_silence_enables_lease_generation_swap
    (world : World) (lease : RequestExecutionLease.World Generation)
    (expected next : Generation) (oldDuration oldDeadline duration deadline : Time)
    (hworld : world.lease.lease = .active expected oldDuration oldDeadline)
    (hauthoritative : authoritativeLease world = .ok lease)
    (hfresh : RequestExecutionLease.fresh lease next) (hduration : duration > 0)
    (hdeadline : silentRecoveryTime lease < deadline) :
    ∃ post, RequestExecutionLease.step?
      { lease with now := silentRecoveryTime lease }
      (.recoverExpired .mutationWriteGate expected next duration deadline) = some post := by
  have hprogress := authoritativeLease_uses_only_derived_progress world lease expected
    oldDuration oldDeadline hworld hauthoritative
  have hscope := deriveProgress_scopes_every_fact world expected lease.output hprogress
  have hlease : lease.lease = .active expected oldDuration oldDeadline := by
    simp [authoritativeLease, hworld, hprogress] at hauthoritative
    simp only [Bind.bind, Except.bind, Except.ok.injEq] at hauthoritative
    have hcontrol := congrArg RequestExecutionLease.World.lease hauthoritative
    simpa using hcontrol.symm
  exact finite_silence_enables_atomic_recovery lease expected next oldDuration oldDeadline
    duration deadline hlease (fun fact hfact =>
      ⟨(hscope fact hfact).1, (hscope fact hfact).2.1⟩)
    hfresh hduration hdeadline

/-- Once the executable all-source batch has prepared and the authoritative
lease gate admits the swap, the composed core reaches the exact post-build
synchronization. This is a reachability equation, not a scheduler-fairness or
cross-process transaction premise. -/
theorem prepared_batch_enables_composed_recovery
    (world authoritative : World) (prepared : RecoveryPrepared)
    (lease : RequestExecutionLease.World Generation)
    (expected next : Generation) (duration deadline : Time) (items : List RecoveryItem)
    (hnotReplay : recoveryReplayValid world expected next items = false)
    (hprepare : prepareRecoveryBatch world expected next items = .ok prepared)
    (hauthoritative : synchronize world = .ok authoritative)
    (hlease : RequestExecutionLease.step? authoritative.lease
      (.recoverExpired .mutationWriteGate expected next duration deadline) = some lease) :
    recoverExpiredBatchCore world expected next duration deadline items =
      synchronizePost
        { authoritative with
          lease := lease
          segments := prepared.segments
          messages := prepared.messages
          transcript := prepared.transcript } := by
  simp [recoverExpiredBatchCore, hnotReplay, hprepare, hauthoritative, hlease]

end CanonicalOutput.Execution
