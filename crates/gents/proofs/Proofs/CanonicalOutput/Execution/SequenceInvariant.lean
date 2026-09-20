import Proofs.CanonicalOutput.Execution.Gate
import Proofs.CanonicalOutput.Execution.Properties

/-!
# Canonical message sequence bound

The shared transcript cursor is the allocator for canonical message headers.
This invariant deliberately ranges only over messages for the world's current
session: durable observations may also contain messages copied from another
session, whose allocator is owned by that session.
-/
namespace CanonicalOutput.Execution

def SequenceBound (world : World) : Prop :=
  ∀ message ∈ world.messages,
    message.header.session = world.sessionId →
      message.sequence < world.transcript.nextSeq

theorem empty_messages_sequenceBound (world : World)
    (h : world.messages = []) : SequenceBound world := by
  intro message hmem
  simp [h] at hmem

/-- Transport the bound across an operation which preserves the canonical
message collection and current-session identity while advancing the allocator. -/
theorem SequenceBound.of_messages_eq {before after : World}
    (hbound : SequenceBound before)
    (hmessages : after.messages = before.messages)
    (hsession : after.sessionId = before.sessionId)
    (hnext : before.transcript.nextSeq ≤ after.transcript.nextSeq) :
    SequenceBound after := by
  intro message hmem hcurrent
  apply Nat.lt_of_lt_of_le (hbound message (hmessages ▸ hmem) ?_) hnext
  simpa [hsession] using hcurrent

/-- The allocator step used by every singleton canonical publication. -/
theorem SequenceBound.of_append {before after : World} {message : MessageEnvelope}
    (hbound : SequenceBound before)
    (hmessages : after.messages = before.messages ++ [message])
    (hsession : after.sessionId = before.sessionId)
    (hnext : before.transcript.nextSeq < after.transcript.nextSeq)
    (hsequence : message.sequence = before.transcript.nextSeq) :
    SequenceBound after := by
  intro candidate hmem hcurrent
  rw [hmessages] at hmem
  rcases List.mem_append.mp hmem with hold | hnew
  · exact Nat.lt_of_lt_of_le
      (hbound candidate hold (by simpa [hsession] using hcurrent))
      (Nat.le_of_lt hnext)
  · simp only [List.mem_singleton] at hnew
    subst candidate
    simpa [hsequence] using hnext

private theorem mapError_success {error₁ error₂ value : Type}
    (f : error₁ → error₂) (result : Except error₁ value) (post : value)
    (h : result.mapError f = .ok post) : result = .ok post := by
  cases result with
  | error error => simp [Except.mapError] at h
  | ok value => simpa [Except.mapError] using h

set_option maxHeartbeats 1000000 in
theorem renew_preserves_sequenceBound (before after : World) (generation : Generation)
    (deadline : Time) (hbound : SequenceBound before)
    (h : renew before generation deadline = .ok after) : SequenceBound after := by
  unfold renew renewCore at h
  split at h <;> try contradiction
  cases h
  exact hbound.of_messages_eq rfl rfl (Nat.le_refl _)

set_option maxHeartbeats 1000000 in
theorem appendRaw_preserves_sequenceBound (before after : World) (generation : Generation)
    (record : Segment) (hbound : SequenceBound before)
    (h : appendRaw before generation record = .ok after) : SequenceBound after := by
  have hcore := h
  unfold appendRaw appendRawCore at hcore
  dsimp only at hcore
  repeat' first
    | contradiction
    | (solve | cases hcore; exact hbound)
    | split at hcore

set_option maxHeartbeats 2000000 in
set_option maxRecDepth 100000 in
theorem acceptAndPublish_preserves_sequenceBound
    (before after : World) (generation : Generation) (closing : Segment)
    (message : MessageEnvelope) (targets : List RemoteTarget) (admissions : List ToolAdmission)
    (hbound : SequenceBound before)
    (h : acceptAndPublish before generation closing message targets admissions = .ok after) :
    SequenceBound after := by
  have hcore := checked_core_success _ _ _ h
  simp (config := { maxSteps := 1000000 }) [acceptAndPublishCore] at hcore
  by_cases hc : segmentIdentityCollision before closing = true ∨
      messageIdentityCollision before message = true
  · simp only [if_pos hc] at hcore
    contradiction
  · simp only [if_neg hc] at hcore
    by_cases hn : (targets.map (fun target => target.call)).Nodup
    · simp only [if_pos hn] at hcore
      by_cases hr : remoteTargetsMatchConfiguredRoutes before message targets = false
      · simp only [if_pos hr] at hcore
        contradiction
      · simp only [if_neg hr] at hcore
        by_cases hp : acceptedPublicationPresent before closing message targets = true ∧
            acceptedToolsPresent before message = true
        · simp only [if_pos hp] at hcore
          split at hcore <;> try contradiction
          cases hcore
          exact hbound
        · simp only [if_neg hp] at hcore
          split at hcore <;> try contradiction
          split at hcore <;> try contradiction
          split at hcore <;> try contradiction
          split at hcore <;> try contradiction
          split at hcore <;> try contradiction
          rename_i hsession
          push_neg at hsession
          split at hcore <;> try contradiction
          split at hcore <;> try contradiction
          split at hcore <;> try contradiction
          split at hcore <;> try contradiction
          split at hcore <;> try contradiction
          cases hcore
          apply SequenceBound.of_append (before := before) hbound
          · rfl
          · rfl
          · simp [Transcript.TranscriptState.publishAcceptedAssistant]
          · have hpub : before.transcript.PublishableTurn (messageTurn message) := by
              simp_all
            exact hpub.2.1
    · simp only [if_neg hn] at hcore
      contradiction

set_option maxHeartbeats 2000000 in
set_option maxRecDepth 100000 in
theorem publishAuthored_preserves_sequenceBound
    (before after : World) (generation : Generation) (closing : Segment)
    (message : MessageEnvelope) (hbound : SequenceBound before)
    (h : publishAuthored before generation closing message = .ok after) :
    SequenceBound after := by
  have hcore := checked_core_success _ _ _ h
  simp only [publishAuthoredCore] at hcore
  repeat' first
    | contradiction
    | (solve | cases hcore; exact hbound)
    | split at hcore
  cases hcore
  cases hr : message.header.role <;>
    apply SequenceBound.of_append (before := before) (message := message) hbound <;>
    simp_all [appendAuthoredRow, authoredMessageValid,
      Transcript.TranscriptState.appendUserMessage]

set_option maxHeartbeats 2000000 in
set_option maxRecDepth 100000 in
theorem publishHeaderOnly_preserves_sequenceBound
    (before after : World) (generation : Generation) (message : MessageEnvelope)
    (admissions : List ToolAdmission) (hbound : SequenceBound before)
    (h : publishHeaderOnly before generation message admissions = .ok after) :
    SequenceBound after := by
  have hpredicate := checked_success _ _ _ h
  have hcore := checked_core_success _ _ _ h
  simp only [publishHeaderOnlyCore] at hcore
  repeat' first
    | contradiction
    | (solve | cases hcore; exact hbound)
    | split at hcore
  cases hcore
  cases hr : message.header.role
  · simp [headerOnlyPublicationPresent, hr] at hpredicate
  · apply SequenceBound.of_append (before := before) (message := message) hbound <;>
      simp_all [appendHeaderOnlyRow, headerOnlyMessageValid,
        Transcript.TranscriptState.appendUserMessage,
        Transcript.TranscriptState.publishAcceptedAssistant]
  · apply SequenceBound.of_append (before := before) (message := message) hbound <;>
      simp_all [appendHeaderOnlyRow, headerOnlyMessageValid,
        Transcript.TranscriptState.appendUserMessage,
        Transcript.TranscriptState.publishAcceptedAssistant]

private def RecoverySequenceBound (session : SessionId)
    (prepared : RecoveryPrepared) : Prop :=
  ∀ message ∈ prepared.messages,
    message.header.session = session →
      message.sequence < prepared.transcript.nextSeq

private theorem recovery_append_preserves_bound
    {session : SessionId} {prepared after : RecoveryPrepared} {message : MessageEnvelope}
    (hbound : RecoverySequenceBound session prepared)
    (hmessages : after.messages = prepared.messages ++ [message])
    (htranscript : after.transcript = prepared.transcript.publishPartialAssistant
      message.header.id (messageTurn message))
    (hpublishable : prepared.transcript.PublishableTurn (messageTurn message)) :
    RecoverySequenceBound session after := by
  intro candidate hmem hsession
  rw [hmessages] at hmem
  rcases List.mem_append.mp hmem with hold | hnew
  · rw [htranscript]
    exact Nat.lt_trans (hbound candidate hold hsession) (Nat.lt_succ_self _)
  · simp only [List.mem_singleton] at hnew
    subst candidate
    rw [htranscript]
    change message.sequence < prepared.transcript.nextSeq + 1
    have hs : message.sequence = prepared.transcript.nextSeq := by
      simpa [messageTurn] using hpublishable.2.1
    rw [hs]
    exact Nat.lt_succ_self _

set_option maxHeartbeats 2000000 in
set_option maxRecDepth 100000 in
private theorem prepareRecoveryItems_preserves_sequenceBound
    (world : World) (expected fresh : Generation) (items : List RecoveryItem)
    (prepared after : RecoveryPrepared)
    (hbound : RecoverySequenceBound world.sessionId prepared)
    (h : prepareRecoveryItems world expected fresh items prepared = .ok after) :
    RecoverySequenceBound world.sessionId after := by
  induction items generalizing prepared with
  | nil =>
      simp only [prepareRecoveryItems] at h
      cases h
      exact hbound
  | cons item rest ih =>
      simp only [prepareRecoveryItems] at h
      dsimp only at h
      repeat' split at h <;> try contradiction
      all_goals first
        | exact ih _ (by simpa [RecoverySequenceBound] using hbound) h
        | apply ih _ (recovery_append_preserves_bound hbound rfl rfl (by simp_all)) h

set_option maxHeartbeats 2000000 in
theorem recoverExpiredBatch_preserves_sequenceBound
    (before after : World) (expected fresh : Generation) (duration deadline : Time)
    (items : List RecoveryItem) (hbound : SequenceBound before)
    (h : recoverExpiredBatch before expected fresh duration deadline items = .ok after) :
    SequenceBound after := by
  have hcore := checked_core_success _ _ _ h
  unfold recoverExpiredBatchCore at hcore
  split at hcore
  · cases hcore
    exact hbound
  · split at hcore <;> try contradiction
    rename_i prepared hprepared
    dsimp only at hcore
    split at hcore <;> try contradiction
    cases hcore
    have hinitial : RecoverySequenceBound before.sessionId
        ⟨before.segments, before.messages, before.transcript⟩ := hbound
    unfold prepareRecoveryBatch at hprepared
    split at hprepared <;> try contradiction
    have hpreparedBound := prepareRecoveryItems_preserves_sequenceBound
      before expected fresh items _ prepared hinitial hprepared
    let raw : World := { before with
      segments := prepared.segments
      messages := prepared.messages
      transcript := prepared.transcript }
    change SequenceBound (accountOwnedTools raw expected true)
    apply SequenceBound.of_messages_eq
      (before := raw) (after := accountOwnedTools raw expected true)
    · simpa [SequenceBound, RecoverySequenceBound, raw] using hpreparedBound
    · exact accountOwnedTools_preserves_messages raw expected true
    · exact accountOwnedTools_preserves_sessionId raw expected true
    · rw [accountOwnedTools_preserves_nextSeq]

theorem SequenceBound.of_publicationEffect {before after : World}
    {message : MessageEnvelope} (hbound : SequenceBound before)
    (heffect : ToolDelivery.PublicationEffect before after message) :
    SequenceBound after := by
  rcases heffect with ⟨hsession, replay | fresh⟩
  · exact hbound.of_messages_eq replay.1 hsession (Nat.le_of_eq replay.2.symm)
  · exact hbound.of_append fresh.1 hsession
      (fresh.2.2.symm ▸ Nat.lt_succ_self _) fresh.2.1

theorem ToolDelivery.publishToolDelivery_preserves_sequenceBound
    (before after : World) (document : DocId) (message : MessageEnvelope)
    (hbound : SequenceBound before)
    (h : publishToolDelivery before document message = .ok after) :
    SequenceBound after :=
  hbound.of_publicationEffect (publication_effect before after document message h)

theorem ToolDelivery.publishWakeNotification_preserves_sequenceBound
    (before after : World) (document : DocId) (binding : WakeDocumentBinding)
    (message : MessageEnvelope) (hbound : SequenceBound before)
    (h : publishWakeNotification before document binding message = .ok after) :
    SequenceBound after :=
  hbound.of_publicationEffect
    (wake_notification_effect before after document binding message h)

theorem ToolDelivery.publishGoalNotification_preserves_sequenceBound
    (before after : World) (document : DocId) (binding : GoalNotificationBinding)
    (message : MessageEnvelope) (hbound : SequenceBound before)
    (h : publishGoalNotification before document binding message = .ok after) :
    SequenceBound after :=
  hbound.of_publicationEffect
    (goal_notification_effect before after document binding message h)

theorem ToolDelivery.publishBackgroundReceipt_preserves_sequenceBound
    (before after : World) (document : DocId) (closing : Segment)
    (message : MessageEnvelope) (hbound : SequenceBound before)
    (h : publishBackgroundReceipt before document closing message = .ok after) :
    SequenceBound after :=
  hbound.of_publicationEffect
    (background_receipt_effect before after document closing message h)

theorem retractBeforeRetry_preserves_sequenceBound
    (before after : World) (generation : Generation) (record : Segment)
    (hbound : SequenceBound before)
    (h : retractBeforeRetry before generation record = .ok after) : SequenceBound after := by
  have hcore := checked_core_success _ _ _ h
  have hframe : after.messages = before.messages ∧ after.sessionId = before.sessionId := by
    unfold retractBeforeRetryCore at hcore
    repeat' first | contradiction | (solve | cases hcore; exact ⟨rfl, rfl⟩) | split at hcore
  exact hbound.of_messages_eq hframe.1 hframe.2
    (Nat.le_of_eq (retractBeforeRetry_preserves_nextSeq before after generation record h).symm)

theorem dispatch_preserves_sequenceBound
    (before after : World) (generation : Generation) (permit : DispatchPermit)
    (hbound : SequenceBound before)
    (h : dispatch before generation permit = .ok after) : SequenceBound after := by
  have hcore := checked_core_success _ _ _ h
  have hframe : after.messages = before.messages ∧ after.sessionId = before.sessionId := by
    unfold dispatchCore at hcore
    dsimp only at hcore
    repeat' first | contradiction | (solve | cases hcore; exact ⟨rfl, rfl⟩) | split at hcore
  exact hbound.of_messages_eq hframe.1 hframe.2
    (Nat.le_of_eq (dispatch_preserves_nextSeq before after generation permit h).symm)

theorem admitSpawnedBackground_preserves_sequenceBound
    (before after : World) (generation : Generation) (admission : SpawnedToolAdmission)
    (hbound : SequenceBound before)
    (h : admitSpawnedBackground before generation admission = .ok after) : SequenceBound after := by
  have hcore := checked_core_success _ _ _ h
  have hframe : after.messages = before.messages ∧ after.sessionId = before.sessionId := by
    unfold admitSpawnedBackgroundCore at hcore
    dsimp only at hcore
    repeat' first | contradiction | (solve | cases hcore; exact ⟨rfl, rfl⟩) | split at hcore
  exact hbound.of_messages_eq hframe.1 hframe.2
    (Nat.le_of_eq (admitSpawnedBackground_preserves_nextSeq
      before after generation admission h).symm)

theorem changeToolControl_preserves_sequenceBound
    (before after : World) (generation : Generation) (document : DocId)
    (action : ToolExecution.ToolCallContext.Action) (hbound : SequenceBound before)
    (h : changeToolControl before generation document action = .ok after) : SequenceBound after := by
  have hcore := checked_core_success _ _ _ h
  have hframe : after.messages = before.messages ∧ after.sessionId = before.sessionId := by
    unfold changeToolControlCore at hcore
    repeat' first | contradiction | (solve | cases hcore; exact ⟨rfl, rfl⟩) | split at hcore
  exact hbound.of_messages_eq hframe.1 hframe.2
    (Nat.le_of_eq (changeToolControl_preserves_nextSeq
      before after generation document action h).symm)

theorem revokeCorrupt_preserves_sequenceBound
    (before after : World) (expected fresh : Generation)
    (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection)
    (hbound : SequenceBound before)
    (h : revokeCorrupt before expected fresh outcome selection = .ok after) : SequenceBound after := by
  have hp := checked_success _ _ _ h
  simp only [Bool.and_eq_true, beq_iff_eq] at hp
  have hcore := checked_core_success _ _ _ h
  have hsession := (revokeCorruptCore_preserves_request_identity
    before after expected fresh outcome selection hcore).2
  exact hbound.of_messages_eq hp.2 hsession
    (Nat.le_of_eq (revokeCorrupt_preserves_nextSeq
      before after expected fresh outcome selection h).symm)

theorem terminalize_preserves_sequenceBound
    (before after : World) (generation : Generation)
    (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection)
    (hbound : SequenceBound before)
    (h : terminalize before generation outcome selection = .ok after) : SequenceBound after := by
  have hcore := checked_core_success _ _ _ h
  have hsession := (terminalizeCore_preserves_request_identity
    before after generation outcome selection hcore).2
  have hframe : after.messages = before.messages ∧ after.sessionId = before.sessionId := by
    constructor
    · unfold terminalizeCore at hcore
      dsimp only at hcore
      repeat' first
        | contradiction
        | (solve | cases hcore; first
            | rfl
            | exact accountOwnedTools_preserves_messages before generation
                (outcome != RequestExecutionLease.Outcome.completed))
        | split at hcore
    · exact hsession
  exact hbound.of_messages_eq hframe.1 hframe.2
    (Nat.le_of_eq (terminalize_preserves_nextSeq
      before after generation outcome selection h).symm)

theorem Compaction.advanceCursor_preserves_sequenceBound
    (before after : World) (cursor : Transcript.Sequence)
    (hbound : SequenceBound before)
    (h : advanceCursor? before cursor = some after) : SequenceBound after := by
  rcases advanceCursor_preserves_publications before after cursor h with
    ⟨htranscript, hmessages, _, _⟩
  have hsession : after.sessionId = before.sessionId := by
    unfold advanceCursor? at h
    repeat' first | contradiction | (solve | cases h; rfl) | split at h
  exact hbound.of_messages_eq hmessages hsession
    (Nat.le_of_eq (congrArg Transcript.TranscriptState.nextSeq htranscript).symm)

theorem Gate.evaluate_preserves_sequenceBound
    (operation : Operation) (before after : World) (hbound : SequenceBound before)
    (h : evaluate operation before = .ok after) : SequenceBound after := by
  unfold evaluate at h
  cases operation with
  | renew generation deadline =>
      exact renew_preserves_sequenceBound before after generation deadline hbound
        (mapError_success Error.execution _ _ h)
  | append generation record =>
      exact appendRaw_preserves_sequenceBound before after generation record hbound
        (mapError_success Error.execution _ _ h)
  | retract generation record =>
      exact retractBeforeRetry_preserves_sequenceBound before after generation record hbound
        (mapError_success Error.execution _ _ h)
  | accept generation closing message targets admissions =>
      exact acceptAndPublish_preserves_sequenceBound before after generation closing message
        targets admissions hbound (mapError_success Error.execution _ _ h)
  | authored generation closing message =>
      exact publishAuthored_preserves_sequenceBound before after generation closing message hbound
        (mapError_success Error.execution _ _ h)
  | headerOnly generation message admissions =>
      exact publishHeaderOnly_preserves_sequenceBound before after generation message admissions
        hbound (mapError_success Error.execution _ _ h)
  | dispatch generation permit =>
      exact dispatch_preserves_sequenceBound before after generation permit hbound
        (mapError_success Error.execution _ _ h)
  | admitSpawned generation admission =>
      exact admitSpawnedBackground_preserves_sequenceBound before after generation admission hbound
        (mapError_success Error.execution _ _ h)
  | toolControl generation document action =>
      exact changeToolControl_preserves_sequenceBound before after generation document action hbound
        (mapError_success Error.execution _ _ h)
  | toolAppend document record =>
      have hop := mapError_success Error.delivery _ _ h
      rcases ToolDelivery.append_preserves_publications before after document record hop with
        ⟨hsession, hmessages, hnext⟩
      exact hbound.of_messages_eq hmessages hsession (Nat.le_of_eq hnext.symm)
  | toolClose document authority record =>
      have hop := mapError_success Error.delivery _ _ h
      rcases ToolDelivery.close_preserves_publications before after document authority record hop with
        ⟨hsession, hmessages, hnext⟩
      exact hbound.of_messages_eq hmessages hsession (Nat.le_of_eq hnext.symm)
  | toolDeliver document message =>
      exact ToolDelivery.publishToolDelivery_preserves_sequenceBound before after document message
        hbound (mapError_success Error.delivery _ _ h)
  | toolGoalDeliver document binding message =>
      exact ToolDelivery.publishGoalNotification_preserves_sequenceBound
        before after document binding message hbound (mapError_success Error.delivery _ _ h)
  | backgroundReceipt document closing message =>
      exact ToolDelivery.publishBackgroundReceipt_preserves_sequenceBound
        before after document closing message hbound (mapError_success Error.delivery _ _ h)
  | compact cursor =>
      cases hc : Compaction.advanceCursor? before cursor with
      | none => simp [hc] at h
      | some post =>
          simp [hc] at h
          subst after
          exact Compaction.advanceCursor_preserves_sequenceBound before post cursor hbound hc
  | recover expected fresh duration deadline items =>
      exact recoverExpiredBatch_preserves_sequenceBound before after expected fresh duration deadline
        items hbound (mapError_success Error.execution _ _ h)
  | revoke expected fresh outcome selection =>
      exact revokeCorrupt_preserves_sequenceBound before after expected fresh outcome selection
        hbound (mapError_success Error.execution _ _ h)
  | terminalize generation outcome selection =>
      exact terminalize_preserves_sequenceBound before after generation outcome selection hbound
        (mapError_success Error.execution _ _ h)

theorem Gate.successful_commit_preserves_sequenceBound
    (before after : World) (actor : Actor) (now : Time) (operation : Operation)
    (hbound : SequenceBound before)
    (h : commit before actor now operation = some after) : SequenceBound after := by
  obtain ⟨execution, hevaluate, rfl⟩ := commit_reads_current_world before after actor now operation h
  have htime : SequenceBound (atTime before now) := by
    simpa [SequenceBound, atTime] using hbound
  have hexecution := evaluate_preserves_sequenceBound operation (atTime before now) execution
    htime hevaluate
  simpa [SequenceBound, finishCommit] using hexecution

end CanonicalOutput.Execution
