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
  unfold appendRaw appendRawCore at h
  split at h <;> try contradiction
  split at h
  · split at h <;> try contradiction
    cases h
    exact ⟨by assumption, rfl, rfl⟩
  · split at h <;> try contradiction
    split at h <;> try contradiction
    next lease hlease =>
      dsimp only at h
      split at h <;> try contradiction
      cases h
      have hid := (RequestExecutionLease.append_output_is_identity_and_never_renews
        pre.lease lease generation hlease).1
      subst lease
      exact ⟨by simp, rfl, rfl⟩

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
    (admissions : List ToolAdmission)
    (h : acceptAndPublish pre generation closing message targets admissions = .ok post) :
    acceptedPublicationPresent post closing message targets = true ∧
      acceptedToolsPresent post message = true ∧
      toolProjectionCoherent post = true ∧
      remoteTargetsMatchConfiguredRoutes post message targets = true ∧
      validateClosingRecord post.segments closing = true ∧
      acceptedMessageValid post generation post.segments message = true ∧
      acceptedSourceBound post generation closing message = true := by
  have hcore := checked_core_success _ _ _ h
  have hcoherent := acceptAndPublishCore_success_toolProjectionCoherent
    pre post generation closing message targets admissions hcore
  have hp := checked_success _ _ _ h
  simp only [Bool.and_eq_true] at hp
  exact ⟨hp.1.1.1.1.1, hp.1.1.1.1.2, hcoherent,
    hp.1.1.1.2, hp.1.1.2, hp.1.2, hp.2⟩

theorem fresh_accept_core_success_requires_exact_extent
    (world post : World) (generation : Generation) (closing : Segment)
    (message : MessageEnvelope) (targets : List RemoteTarget)
    (admissions : List ToolAdmission)
    (hnotReplay : acceptedPublicationPresent world closing message targets = false)
    (h : acceptAndPublishCore world generation closing message targets admissions = .ok post) :
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
    physicalRunning post permit.call = true ∧
      toolDispatchPublicationValid post generation permit.call = true ∧
      toolProjectionCoherent post = true := by
  unfold dispatch at h
  have hp := checked_success _ _ _ h
  simp only [Bool.and_eq_true] at hp
  exact ⟨hp.1.1, hp.1.2, hp.2⟩

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
    (admissions : List ToolAdmission)
    (h : publishHeaderOnly pre generation message admissions = .ok post) :
    headerOnlyPublicationPresent post message = true ∧
      headerOnlyMessageValid post generation message = true ∧
      acceptedToolsPresent post message = true ∧ toolProjectionCoherent post = true := by
  have hcore := checked_core_success _ _ _ h
  have hcoherent := publishHeaderOnlyCore_success_toolProjectionCoherent
    pre post generation message admissions hcore
  have hp := checked_success _ _ _ h
  simp only [Bool.and_eq_true] at hp
  exact ⟨hp.1.1, hp.1.2, hp.2, hcoherent⟩

theorem recovery_is_all_sources_single_winner_and_exact
    (pre post : World) (expected next : Generation)
    (duration deadline : Time) (items : List RecoveryItem)
    (h : recoverExpiredBatch pre expected next duration deadline items = .ok post) :
    recoveryBatchPresent post next items = true ∧
      toolProjectionCoherent post = true ∧
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
  exact ⟨hp.1.1.1, hp.1.1.2, hp.1.2, hp.2⟩

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
      terminalSelectionValid post selection = true ∧
      ownedPendingSettled post generation = true ∧ toolProjectionCoherent post = true := by
  unfold terminalize at h
  have hp := checked_success _ _ _ h
  simp only [Bool.and_eq_true] at hp
  exact ⟨hp.1.1.1, hp.1.1.2, hp.1.2, hp.2⟩

theorem admitted_terminal_core_commits_lifecycle_and_pending_cancellation
    (world : World)
    (lease : RequestExecutionLease.World Generation)
    (generation : Generation) (outcome : RequestExecutionLease.Outcome)
    (selection : TerminalSelection)
    (hvalid : terminalSelectionValid world selection = true)
    (hnotReplay : terminalReplayPresent world generation outcome selection = false)
    (hempty : world.terminalSelection.isSome = false)
    (hready : ¬ (outcome = .completed ∧
      normalCompletionToolsReady world generation = false))
    (hlease : RequestExecutionLease.step?
      (accountOwnedTools world generation (outcome != .completed)).lease
      (.finalize .mutationWriteGate generation outcome) = some lease) :
    terminalizeCore world generation outcome selection = .ok
      { accountOwnedTools world generation (outcome != .completed) with
        lease := lease
        terminalSelection := some selection } := by
  simp [terminalizeCore, hvalid, hnotReplay, hempty, hready, hlease]

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
    (hlease : RequestExecutionLease.step?
      (preparedRecoveryWorld world prepared expected).lease
      (.recoverExpired .mutationWriteGate expected next duration deadline) = some lease) :
    recoverExpiredBatchCore world expected next duration deadline items =
      .ok { preparedRecoveryWorld world prepared expected with
        lease := lease } := by
  simp [recoverExpiredBatchCore, hnotReplay, hprepare, hlease]

theorem accounting_cancels_exact_owned_pending
    (world : World) (generation : Generation) (tool : OwnedTool)
    (howned : ownedByGeneration world generation tool = true)
    (hpending : tool.context.state = .pending) :
    accountOneOwnedTool world generation true tool =
      ({ tool with context := { tool.context with state := .cancelled } },
        world.transcript.terminalizeToolCall tool.document .cancelled) := by
  simp [accountOneOwnedTool, howned, hpending,
    ToolExecution.ToolCallContext.step?]

theorem accounting_hands_off_running_without_claiming_stop
    (world : World) (generation : Generation) (tool : OwnedTool)
    (howned : ownedByGeneration world generation tool = true)
    (hrunning : tool.context.state = .running) :
    accountOneOwnedTool world generation true tool =
      (handoffRunningTool world tool,
        world.transcript.releaseParentInFlight tool.document) ∧
      (handoffRunningTool world tool).context.state = .running ∧
      tool.document ∉
        (world.transcript.releaseParentInFlight tool.document).inFlight := by
  simp [accountOneOwnedTool, howned, hrunning, handoffRunningTool,
    Transcript.TranscriptState.releaseParentInFlight]

theorem accounting_ignores_foreign_request_even_same_generation
    (world : World) (generation : Generation) (tool : OwnedTool)
    (hforeign : tool.requestDoc ≠ world.requestId) :
    accountOneOwnedTool world generation true tool = (tool, world.transcript) := by
  have hnotOwned : acceptedHeaderBindsToolGeneration world tool generation = false := by
    unfold acceptedHeaderBindsToolGeneration
    cases tool.provenance <;> simp [hforeign]
  simp [accountOneOwnedTool, ownedByGeneration, hnotOwned]

theorem explicit_background_control_updates_physical_and_parent_hook
    (world : World) (generation : Generation) (tool : OwnedTool)
    (lease : RequestExecutionLease.World Generation)
    (hlookup : ownedToolByDocument? world tool.document = some tool)
    (howned : acceptedHeaderBindsToolGeneration world tool generation = true)
    (hrunning : tool.context.state = .running)
    (hforeground : tool.context.awaitMode = .foreground)
    (hcancel : tool.cancelCascadeIntentAt = none)
    (hstuck : tool.stuckSince = none)
    (hlease : RequestExecutionLease.step? world.lease
      (.authorizeProducerDecision .mutationWriteGate generation .dispatch) = some lease) :
    changeToolControlCore world generation tool.document .background = .ok
      { world with
        lease := lease
        toolContexts := replaceOwnedTool world.toolContexts tool.document
          { tool with context := { tool.context with awaitMode := .background } }
        transcript := world.transcript.releaseParentInFlight tool.document } := by
  simp [changeToolControlCore, toolControlAction, hlookup, howned, hcancel, hstuck,
    ToolExecution.ToolCallContext.step?, hrunning, hforeground, hlease]

theorem tool_control_success_preserves_projection_coherence
    (world post : World) (generation : Generation) (document : DocId)
    (action : ToolExecution.ToolCallContext.Action)
    (h : changeToolControl world generation document action = .ok post) :
    toolProjectionCoherent post = true := by
  unfold changeToolControl at h
  exact checked_success _ _ _ h

theorem spawned_background_admission_is_owned_without_fabricated_intent
    (world post : World) (generation : Generation)
    (admission : SpawnedToolAdmission)
    (h : admitSpawnedBackground world generation admission = .ok post) :
    toolProjectionCoherent post = true ∧ spawnedToolPresent post admission = true := by
  unfold admitSpawnedBackground at h
  have hp := checked_success _ _ _ h
  simpa only [Bool.and_eq_true] using hp

theorem spawned_admission_lost_ack_replay_is_identity
    (world : World) (generation : Generation) (admission : SpawnedToolAdmission)
    (hreplay : spawnedAdmissionReplayValid world admission = true) :
    admitSpawnedBackgroundCore world generation admission = .ok world := by
  simp [admitSpawnedBackgroundCore, hreplay]

theorem second_spawned_child_for_parent_is_rejected
    (world : World) (generation : Generation) (admission : SpawnedToolAdmission)
    (hnotReplay : spawnedAdmissionReplayValid world admission = false)
    (hexisting : world.toolContexts.any (fun tool =>
      tool.provenance == .spawnedBackground admission.parentToolDoc) = true) :
    admitSpawnedBackgroundCore world generation admission = .error .transcriptRejected := by
  simp [admitSpawnedBackgroundCore, hnotReplay, hexisting]

theorem fresh_completed_core_requires_foreground_accounting
    (world post : World) (generation : Generation) (selection : TerminalSelection)
    (hnotReplay : terminalReplayPresent world generation .completed selection = false)
    (h : terminalizeCore world generation .completed selection = .ok post) :
    normalCompletionToolsReady world generation = true := by
  by_contra hnot
  have hfalse : normalCompletionToolsReady world generation = false :=
    Bool.eq_false_of_not_eq_true hnot
  simp only [terminalizeCore] at h
  split at h <;> try contradiction
  split at h
  · rename_i hreplay
    exact Bool.noConfusion (hnotReplay.symm.trans hreplay)
  · split at h <;> try contradiction
    simp [hfalse] at h

theorem remote_bytes_do_not_bypass_execution_admission
    (world : World) (generation : Generation) (permit : DispatchPermit)
    (hpublication : toolDispatchPublicationValid world generation permit.call = true)
    (hnotRunning : physicalRunning world permit.call = false)
    (hdenied : remoteExecutionAdmitted world permit.call = false) :
    dispatchCore world generation permit = .error .transcriptRejected := by
  simp [dispatchCore, hpublication, hnotRunning, hdenied]

theorem revoke_corrupt_exact_replay_is_identity
    (world : World) (expected fresh : Generation)
    (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection)
    (hreplay : terminalReplayPresent world fresh outcome selection = true) :
    revokeCorruptCore world expected fresh outcome selection = .ok world := by
  simp [revokeCorruptCore, hreplay]

theorem revoke_corrupt_preserves_conflicting_output_facts
    (world post : World) (expected fresh : Generation)
    (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection)
    (h : revokeCorrupt world expected fresh outcome selection = .ok post) :
    post.segments = world.segments ∧ post.messages = world.messages := by
  unfold revokeCorrupt at h
  have hp := checked_success _ _ _ h
  simp only [Bool.and_eq_true, beq_iff_eq] at hp
  exact ⟨hp.1.2, hp.2⟩

theorem metadata_owned_pending_is_cancelled_by_revocation_accounting
    (world : World) (generation : Generation) (tool : OwnedTool)
    (howned : metadataOwnedByGeneration world generation tool = true)
    (hpending : tool.context.state = .pending) :
    (accountOneMetadataOwnedTool world generation tool).1.context.state = .cancelled := by
  simp [accountOneMetadataOwnedTool, howned, hpending,
    ToolExecution.ToolCallContext.step?]

theorem metadata_owned_running_is_handed_off_without_fake_stop
    (world : World) (generation : Generation) (tool : OwnedTool)
    (howned : metadataOwnedByGeneration world generation tool = true)
    (hrunning : tool.context.state = .running) :
    (accountOneMetadataOwnedTool world generation tool).1.context.state = .running ∧
      (accountOneMetadataOwnedTool world generation tool).1.stuckSince = some world.lease.now := by
  simp [accountOneMetadataOwnedTool, howned, hrunning, handoffRunningTool]

end CanonicalOutput.Execution
