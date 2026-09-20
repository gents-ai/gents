import Proofs.CanonicalOutput.Execution.ToolDelivery
import Proofs.Background.CompletionContinuation

/-!
# Shared background-notification continuation

This adapter does not publish another transcript row. It accepts only the
ordinary notification already committed by the shared canonical tool-delivery
owner, reconstructs the existing `NotifiedCompletion` evidence from the
pre-publication transcript, and requires exact equality with the committed
shared transcript before invoking the existing queue owner.
-/

namespace CanonicalOutput.Execution.BackgroundContinuation

def publishNotification (binding : WakeDocumentBinding) (world : World)
    (document : DocId) (message : MessageEnvelope) : Except ToolDelivery.Error World :=
  ToolDelivery.publishWakeNotification world document binding message

/-- Authenticated projection of the durable AgentRequest row retained after a
background wake leaves the pending queue. Native code supplies this only after
ACP-authorized exact-row lookup; `authenticated` makes that refinement premise
explicit rather than treating a bare active/terminal request id as provenance. -/
def wakeDocumentBindingValid (world : World) (message : MessageEnvelope)
    (queue : SessionQueue.SessionQueueState)
    (notified : BackgroundCompletion.NotifiedCompletion)
    (wake : SessionQueue.QueueEntry) (binding : WakeDocumentBinding) : Bool :=
  binding.authenticated && binding.agent == queue.scope.agent &&
    binding.agent == world.principal && binding.session == queue.sessionId &&
    binding.session == world.sessionId && binding.entry == wake &&
    message.header.request == some binding.wakeDocument &&
    binding.notificationMessageId == notified.completion.notificationMessageId &&
    notified.transcript.messages.any (fun row =>
      row.messageId == binding.notificationMessageId &&
        row.sequence == binding.notificationSequence && row.sessionId == binding.session &&
        row.role == .user && row.kind == .ordinary) &&
    decide (binding.entry.coalesceWellFormed queue.sessionId)

/-- Pending coalescing may accompany a fresh publication. Once the wake has
left pending, only an exact publication replay may reuse its durable receipt. -/
def wakeBindingAcceptsPublication (before : World) (message : MessageEnvelope)
    (queue : SessionQueue.SessionQueueState) (binding : WakeDocumentBinding) : Bool :=
  binding.entry ∈ queue.pending ||
    ((queue.active == some binding.entry.requestId ||
        decide (binding.entry.requestId ∈ queue.terminal)) &&
      decide (message ∈ before.messages))

structure Result where
  before : World
  execution : World
  document : DocId
  message : MessageEnvelope
  binding : WakeDocumentBinding
  published :
    publishNotification binding before document message = .ok execution
  notified : BackgroundCompletion.NotifiedCompletion
  queued : Option BackgroundCompletion.QueuedCompletion
  wakeAlreadyPending : Bool
  sharedTranscript : notified.transcript = execution.transcript

def observeNotification? (completion : BackgroundCompletion.TerminalCompletion)
    (transcript : Transcript.TranscriptState) :
    Option BackgroundCompletion.NotifiedCompletion :=
  if ht : isTerminal completion.toolState then
    if hd : ∃ row,
        row ∈ transcript.messages ∧
        row.messageId = completion.notificationMessageId ∧
        row.sessionId = transcript.sessionId ∧
        row.role = Transcript.MessageRole.user ∧
        row.kind = Transcript.MessageKind.ordinary then
      some { completion := completion, preTranscript := transcript
             transcript := transcript, terminal := ht, freshAppend := false
             appended := by simp, durable := hd }
    else none
  else none

def publishAndEnqueue? (before : World) (document : DocId)
    (message : MessageEnvelope) (wake : SessionQueue.QueueEntry)
    (binding : WakeDocumentBinding) (queue : SessionQueue.SessionQueueState) : Option Result :=
  match hp : publishNotification binding before document message with
  | .error _ => none
  | .ok execution => do
      let tool ← ownedToolByDocument? execution document
      if tool.context.awaitMode != .background then none
      if ToolDelivery.deliveryShape? message document != some .backgroundNotification then none
      if queue.sessionId != execution.sessionId || queue.scope.agent != execution.principal then none
      if binding.entry != wake || wake.source != .backgroundCompletion ||
          !wake.coalesceWellFormed queue.sessionId then none
      let completion : BackgroundCompletion.TerminalCompletion :=
        { toolState := tool.context.state
        , notificationMessageId := message.header.id
        , wake := wake }
      let notified ← observeNotification? completion execution.transcript
      if hshared : notified.transcript = execution.transcript then
        let existing := wakeDocumentBindingValid execution message queue notified wake binding &&
          wakeBindingAcceptsPublication before message queue binding
        let queued ← match BackgroundCompletion.enqueueWake? notified queue with
          | some value => some (some value)
          | none => if existing then some none else none
        some
          { before := before
          , execution := execution
          , document := document
          , message := message
          , binding := binding
          , published := hp
          , notified := notified
          , queued := queued
          , wakeAlreadyPending := existing
          , sharedTranscript := hshared }
      else none

theorem successful_enqueue_preserves_active
    (before : World) (document : DocId) (message : MessageEnvelope)
    (wake : SessionQueue.QueueEntry) (binding : WakeDocumentBinding)
    (queue : SessionQueue.SessionQueueState) (result : Result)
    (h : publishAndEnqueue? before document message wake binding queue = some result) :
    (match result.queued with
      | some value => value.queue.active
      | none => queue.active) = queue.active := by
  unfold publishAndEnqueue? at h
  split at h
  · contradiction
  next execution hp =>
    cases ht : ownedToolByDocument? execution document with
    | none => simp [ht] at h
    | some tool =>
        simp [ht] at h
        rcases h with ⟨_, _, _, _, h⟩
        cases ho : observeNotification?
            { toolState := tool.context.state
            , notificationMessageId := message.header.id, wake := wake }
            execution.transcript with
        | none => simp [ho] at h
        | some notified =>
          simp [ho] at h
          repeat' split at h <;> try contradiction
          all_goals rcases h with ⟨_, h⟩
          all_goals try contradiction
          all_goals cases h
          all_goals simp_all [BackgroundCompletion.enqueueWake?, SessionQueue.step?]
          all_goals rename_i value
          all_goals rcases value with ⟨_, _, _, hvalue⟩
          all_goals repeat' split at hvalue <;> try contradiction
          all_goals cases hvalue
          all_goals apply SessionQueue.coalescePending_preserves_active
          all_goals assumption

def claimContinuation? (result : Result) : Option BackgroundCompletion.Continuation :=
  result.queued.bind BackgroundCompletion.claimContinuation?

theorem successful_composition_uses_one_shared_notification
    (before : World) (document : DocId) (message : MessageEnvelope)
    (wake : SessionQueue.QueueEntry) (binding : WakeDocumentBinding)
    (queue : SessionQueue.SessionQueueState)
    (result : Result)
    (_h : publishAndEnqueue? before document message wake binding queue = some result) :
    publishNotification result.binding result.before result.document result.message =
        .ok result.execution ∧
      result.notified.transcript = result.execution.transcript ∧
      BackgroundCompletion.HasNotification result.notified := by
  exact ⟨result.published, result.sharedTranscript,
    BackgroundCompletion.notified_completion_has_durable_message result.notified⟩

theorem claimed_composition_sees_published_notification
    (result : Result) (continuation : BackgroundCompletion.Continuation)
    (h : claimContinuation? result = some continuation) :
    isTerminal continuation.queued.notified.completion.toolState ∧
      BackgroundCompletion.HasNotification continuation.queued.notified ∧
      continuation.queue.active =
        some continuation.queued.notified.completion.wake.requestId := by
  unfold claimContinuation? at h
  cases hqueued : result.queued with
  | none => simp [hqueued] at h
  | some queued =>
      simp [hqueued] at h
      cases hc : BackgroundCompletion.claimContinuation? queued with
      | none => simp [hc] at h
      | some value =>
          simp [hc] at h
          cases h
          exact BackgroundCompletion.claimed_continuation_sees_terminal_notification continuation

end CanonicalOutput.Execution.BackgroundContinuation
