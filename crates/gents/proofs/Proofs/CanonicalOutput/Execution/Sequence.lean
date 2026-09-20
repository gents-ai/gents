import Proofs.CanonicalOutput.Execution.Transition

namespace CanonicalOutput.Execution

theorem accountOneOwnedTool_preserves_nextSeq
    (world : World) (generation : Generation) (interrupt : Bool) (tool : OwnedTool) :
    (accountOneOwnedTool world generation interrupt tool).2.nextSeq =
      world.transcript.nextSeq := by
  unfold accountOneOwnedTool
  split <;> try rfl
  split <;> try rfl
  split <;> rfl

private theorem accountFold_preserves_nextSeq
    (tools : List OwnedTool) (world : World) (generation : Generation) (interrupt : Bool) :
    (tools.foldl (fun current original =>
      let (tool, transcript) := accountOneOwnedTool current generation interrupt original
      { current with
        toolContexts := replaceOwnedTool current.toolContexts original.document tool
        transcript := transcript }) world).transcript.nextSeq = world.transcript.nextSeq := by
  induction tools generalizing world with
  | nil => rfl
  | cons tool rest ih =>
      simp only [List.foldl_cons]
      rw [ih]
      exact accountOneOwnedTool_preserves_nextSeq world generation interrupt tool

theorem accountOwnedTools_preserves_nextSeq
    (world : World) (generation : Generation) (interrupt : Bool) :
    (accountOwnedTools world generation interrupt).transcript.nextSeq =
      world.transcript.nextSeq :=
  accountFold_preserves_nextSeq world.toolContexts world generation interrupt

private theorem accountFold_preserves_request_identity
    (tools : List OwnedTool) (world : World) (generation : Generation) (interrupt : Bool) :
    let after := tools.foldl (fun current original =>
      let (tool, transcript) := accountOneOwnedTool current generation interrupt original
      { current with
        toolContexts := replaceOwnedTool current.toolContexts original.document tool
        transcript := transcript }) world
    after.requestId = world.requestId ∧ after.sessionId = world.sessionId := by
  induction tools generalizing world with
  | nil => exact ⟨rfl, rfl⟩
  | cons tool rest ih =>
      simp only [List.foldl_cons]
      exact ih _

theorem accountOwnedTools_preserves_request_identity
    (world : World) (generation : Generation) (interrupt : Bool) :
    (accountOwnedTools world generation interrupt).requestId = world.requestId ∧
      (accountOwnedTools world generation interrupt).sessionId = world.sessionId :=
  accountFold_preserves_request_identity world.toolContexts world generation interrupt

theorem accountOwnedTools_preserves_sessionId
    (world : World) (generation : Generation) (interrupt : Bool) :
    (accountOwnedTools world generation interrupt).sessionId = world.sessionId :=
  (accountOwnedTools_preserves_request_identity world generation interrupt).2

private theorem accountFold_preserves_messages
    (tools : List OwnedTool) (world : World) (generation : Generation) (interrupt : Bool) :
    (tools.foldl (fun current original =>
      let (tool, transcript) := accountOneOwnedTool current generation interrupt original
      { current with
        toolContexts := replaceOwnedTool current.toolContexts original.document tool
        transcript := transcript }) world).messages = world.messages := by
  induction tools generalizing world with
  | nil => rfl
  | cons tool rest ih => simpa only [List.foldl_cons] using ih _

theorem accountOwnedTools_preserves_messages
    (world : World) (generation : Generation) (interrupt : Bool) :
    (accountOwnedTools world generation interrupt).messages = world.messages :=
  accountFold_preserves_messages world.toolContexts world generation interrupt

theorem accountOneMetadataOwnedTool_preserves_nextSeq
    (world : World) (generation : Generation) (tool : OwnedTool) :
    (accountOneMetadataOwnedTool world generation tool).2.nextSeq =
      world.transcript.nextSeq := by
  unfold accountOneMetadataOwnedTool
  split <;> try rfl
  split <;> try rfl
  split <;> rfl

private theorem accountMetadataFold_preserves_nextSeq
    (tools : List OwnedTool) (world : World) (generation : Generation) :
    (tools.foldl (fun current original =>
      let (tool, transcript) := accountOneMetadataOwnedTool current generation original
      { current with
        toolContexts := replaceOwnedTool current.toolContexts original.document tool
        transcript := transcript }) world).transcript.nextSeq = world.transcript.nextSeq := by
  induction tools generalizing world with
  | nil => rfl
  | cons tool rest ih =>
      simp only [List.foldl_cons]
      rw [ih]
      exact accountOneMetadataOwnedTool_preserves_nextSeq world generation tool

theorem accountMetadataOwnedTools_preserves_nextSeq
    (world : World) (generation : Generation) :
    (accountMetadataOwnedTools world generation).transcript.nextSeq =
      world.transcript.nextSeq :=
  accountMetadataFold_preserves_nextSeq world.toolContexts world generation

private theorem accountMetadataFold_preserves_request_identity
    (tools : List OwnedTool) (world : World) (generation : Generation) :
    let after := tools.foldl (fun current original =>
      let (tool, transcript) := accountOneMetadataOwnedTool current generation original
      { current with
        toolContexts := replaceOwnedTool current.toolContexts original.document tool
        transcript := transcript }) world
    after.requestId = world.requestId ∧ after.sessionId = world.sessionId := by
  induction tools generalizing world with
  | nil => exact ⟨rfl, rfl⟩
  | cons tool rest ih =>
      simp only [List.foldl_cons]
      exact ih _

theorem accountMetadataOwnedTools_preserves_request_identity
    (world : World) (generation : Generation) :
    (accountMetadataOwnedTools world generation).requestId = world.requestId ∧
      (accountMetadataOwnedTools world generation).sessionId = world.sessionId :=
  accountMetadataFold_preserves_request_identity world.toolContexts world generation

theorem prepareRecoveryItems_nextSeq_monotone
    (world : World) (expected fresh : Generation) (items : List RecoveryItem)
    (before after : RecoveryPrepared)
    (h : prepareRecoveryItems world expected fresh items before = .ok after) :
    before.transcript.nextSeq ≤ after.transcript.nextSeq := by
  induction items generalizing before with
  | nil => simp [prepareRecoveryItems] at h; cases h; exact Nat.le_refl _
  | cons item rest ih =>
      simp only [prepareRecoveryItems] at h
      repeat' first
        | contradiction
        | (have hgrow := ih _ h
           exact Nat.le_trans (by simp [Transcript.TranscriptState.publishPartialAssistant]) hgrow)
        | split at h

theorem prepareRecoveryBatch_nextSeq_monotone
    (world : World) (expected fresh : Generation) (items : List RecoveryItem)
    (after : RecoveryPrepared)
    (h : prepareRecoveryBatch world expected fresh items = .ok after) :
    world.transcript.nextSeq ≤ after.transcript.nextSeq := by
  unfold prepareRecoveryBatch at h
  split at h
  · contradiction
  · exact prepareRecoveryItems_nextSeq_monotone world expected fresh items _ after h

theorem recoverExpiredBatchCore_preserves_request_identity
    (world after : World) (expected fresh : Generation) (duration deadline : Time)
    (items : List RecoveryItem)
    (h : recoverExpiredBatchCore world expected fresh duration deadline items = .ok after) :
    after.requestId = world.requestId ∧ after.sessionId = world.sessionId := by
  unfold recoverExpiredBatchCore at h
  split at h
  · cases h; exact ⟨rfl, rfl⟩
  · split at h <;> try contradiction
    rename_i prepared hprepared
    dsimp only at h
    split at h <;> try contradiction
    cases h
    simpa only [preparedRecoveryWorld] using
      accountOwnedTools_preserves_request_identity
        { world with
          segments := prepared.segments
          messages := prepared.messages
          transcript := prepared.transcript }
        expected true

theorem terminalizeCore_preserves_request_identity
    (world after : World) (generation : Generation)
    (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection)
    (h : terminalizeCore world generation outcome selection = .ok after) :
    after.requestId = world.requestId ∧ after.sessionId = world.sessionId := by
  simp only [terminalizeCore] at h
  repeat' first
    | contradiction
    | (solve | cases h; exact ⟨rfl, rfl⟩)
    | (solve | cases h; apply accountOwnedTools_preserves_request_identity)
    | split at h

theorem revokeCorruptCore_preserves_request_identity
    (world after : World) (expected fresh : Generation)
    (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection)
    (h : revokeCorruptCore world expected fresh outcome selection = .ok after) :
    after.requestId = world.requestId ∧ after.sessionId = world.sessionId := by
  simp only [revokeCorruptCore] at h
  repeat' first
    | contradiction
    | (solve | cases h; exact ⟨rfl, rfl⟩)
    | (solve | cases h; apply accountMetadataOwnedTools_preserves_request_identity)
    | split at h

set_option maxHeartbeats 1000000 in
set_option maxRecDepth 100000 in
theorem acceptAndPublish_nextSeq_monotone
    (world after : World) (generation : Generation) (closing : Segment)
    (message : MessageEnvelope) (targets : List RemoteTarget) (admissions : List ToolAdmission)
    (h : acceptAndPublish world generation closing message targets admissions = .ok after) :
    world.transcript.nextSeq ≤ after.transcript.nextSeq := by
  have hcore := checked_core_success _ _ _ h
  simp (config := { maxSteps := 1000000 }) [acceptAndPublishCore] at hcore
  by_cases hc : segmentIdentityCollision world closing = true ∨
      messageIdentityCollision world message = true
  · simp only [if_pos hc] at hcore
    contradiction
  · simp only [if_neg hc] at hcore
    by_cases hn : (targets.map (fun target => target.call)).Nodup
    · simp only [if_pos hn] at hcore
      by_cases hr : remoteTargetsMatchConfiguredRoutes world message targets = false
      · simp only [if_pos hr] at hcore
        contradiction
      · simp only [if_neg hr] at hcore
        by_cases hp : acceptedPublicationPresent world closing message targets = true ∧
            acceptedToolsPresent world message = true
        · simp only [if_pos hp] at hcore
          split at hcore <;> try contradiction
          cases hcore
          exact Nat.le_refl _
        · simp only [if_neg hp] at hcore
          split at hcore <;> try contradiction
          split at hcore <;> try contradiction
          split at hcore <;> try contradiction
          split at hcore <;> try contradiction
          split at hcore <;> try contradiction
          split at hcore <;> try contradiction
          split at hcore <;> try contradiction
          split at hcore <;> try contradiction
          split at hcore <;> try contradiction
          split at hcore <;> try contradiction
          all_goals (cases hcore; simp [Transcript.TranscriptState.publishAcceptedAssistant])
    · simp only [if_neg hn] at hcore
      contradiction

theorem publishAuthored_nextSeq_monotone
    (world after : World) (generation : Generation) (closing : Segment)
    (message : MessageEnvelope)
    (h : publishAuthored world generation closing message = .ok after) :
    world.transcript.nextSeq ≤ after.transcript.nextSeq := by
  have hcore := checked_core_success _ _ _ h
  simp only [publishAuthoredCore] at hcore
  repeat' first
    | contradiction
    | (solve | cases hcore; cases hr : message.header.role <;>
        simp [appendAuthoredRow, hr, Transcript.TranscriptState.appendUserMessage])
    | split at hcore

theorem publishHeaderOnly_nextSeq_monotone
    (world after : World) (generation : Generation) (message : MessageEnvelope)
    (admissions : List ToolAdmission)
    (h : publishHeaderOnly world generation message admissions = .ok after) :
    world.transcript.nextSeq ≤ after.transcript.nextSeq := by
  have hcore := checked_core_success _ _ _ h
  simp only [publishHeaderOnlyCore] at hcore
  repeat' first
    | contradiction
    | (solve | cases hcore; cases hr : message.header.role <;>
        simp [appendHeaderOnlyRow, hr, Transcript.TranscriptState.appendUserMessage,
          Transcript.TranscriptState.publishAcceptedAssistant])
    | split at hcore

theorem recoverExpiredBatch_nextSeq_monotone
    (world after : World) (expected fresh : Generation) (duration deadline : Time)
    (items : List RecoveryItem)
    (h : recoverExpiredBatch world expected fresh duration deadline items = .ok after) :
    world.transcript.nextSeq ≤ after.transcript.nextSeq := by
  have hcore := checked_core_success _ _ _ h
  unfold recoverExpiredBatchCore at hcore
  split at hcore
  · cases hcore; exact Nat.le_refl _
  · split at hcore
    · contradiction
    · rename_i prepared hprepare
      dsimp only at hcore
      split at hcore
      · contradiction
      · cases hcore
        simp only [preparedRecoveryWorld, accountOwnedTools_preserves_nextSeq]
        exact prepareRecoveryBatch_nextSeq_monotone world expected fresh items prepared hprepare

theorem terminalize_preserves_nextSeq
    (world after : World) (generation : Generation)
    (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection)
    (h : terminalize world generation outcome selection = .ok after) :
    after.transcript.nextSeq = world.transcript.nextSeq := by
  have hcore := checked_core_success _ _ _ h
  simp only [terminalizeCore] at hcore
  repeat' first
    | contradiction
    | (solve | cases hcore; simp [accountOwnedTools_preserves_nextSeq])
    | split at hcore

theorem renew_preserves_nextSeq (world after : World) (generation : Generation)
    (deadline : Time) (h : renew world generation deadline = .ok after) :
    after.transcript.nextSeq = world.transcript.nextSeq := by
  unfold renew renewCore at h
  split at h
  · contradiction
  · cases h; rfl

theorem appendRaw_preserves_nextSeq (world after : World) (generation : Generation)
    (record : Segment) (h : appendRaw world generation record = .ok after) :
    after.transcript.nextSeq = world.transcript.nextSeq := by
  have hcore := h
  unfold appendRaw at hcore
  simp only [appendRawCore] at hcore
  try dsimp only at hcore
  repeat' first | contradiction | (solve | cases hcore; rfl) | split at hcore

theorem retractBeforeRetry_preserves_nextSeq (world after : World) (generation : Generation)
    (record : Segment) (h : retractBeforeRetry world generation record = .ok after) :
    after.transcript.nextSeq = world.transcript.nextSeq := by
  have hcore := checked_core_success _ _ _ h
  simp only [retractBeforeRetryCore] at hcore
  try dsimp only at hcore
  repeat' first | contradiction | (solve | cases hcore; rfl) | split at hcore

@[simp] theorem dispatchMode_preserves_nextSeq (transcript : Transcript.TranscriptState)
    (call : ToolExecution.ToolCallId) (mode : Subagent.AwaitMode) :
    (transcript.dispatchToolCallWithMode call mode).nextSeq = transcript.nextSeq := by
  cases mode <;> rfl

theorem dispatch_preserves_nextSeq (world after : World) (generation : Generation)
    (permit : DispatchPermit) (h : dispatch world generation permit = .ok after) :
    after.transcript.nextSeq = world.transcript.nextSeq := by
  have hcore := checked_core_success _ _ _ h
  simp only [dispatchCore] at hcore
  try dsimp only at hcore
  repeat' first
    | contradiction
    | (solve | cases hcore; simp [dispatchMode_preserves_nextSeq])
    | split at hcore

theorem changeToolControl_preserves_nextSeq (world after : World) (generation : Generation)
    (document : DocId) (action : ToolExecution.ToolCallContext.Action)
    (h : changeToolControl world generation document action = .ok after) :
    after.transcript.nextSeq = world.transcript.nextSeq := by
  have hcore := checked_core_success _ _ _ h
  simp only [changeToolControlCore] at hcore
  try dsimp only at hcore
  repeat' first
    | contradiction
    | (solve | cases hcore; cases action <;> rfl)
    | split at hcore

theorem admitSpawnedBackground_preserves_nextSeq (world after : World)
    (generation : Generation) (admission : SpawnedToolAdmission)
    (h : admitSpawnedBackground world generation admission = .ok after) :
    after.transcript.nextSeq = world.transcript.nextSeq := by
  have hcore := checked_core_success _ _ _ h
  simp only [admitSpawnedBackgroundCore] at hcore
  try dsimp only at hcore
  repeat' first | contradiction | (solve | cases hcore; rfl) | split at hcore

theorem revokeCorrupt_preserves_nextSeq
    (world after : World) (expected fresh : Generation)
    (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection)
    (h : revokeCorrupt world expected fresh outcome selection = .ok after) :
    after.transcript.nextSeq = world.transcript.nextSeq := by
  have hcore := checked_core_success _ _ _ h
  simp only [revokeCorruptCore] at hcore
  repeat' first
    | contradiction
    | (solve | cases hcore; simp [accountMetadataOwnedTools_preserves_nextSeq])
    | split at hcore

end CanonicalOutput.Execution
