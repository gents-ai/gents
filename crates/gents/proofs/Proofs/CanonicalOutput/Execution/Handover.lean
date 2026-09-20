import Proofs.CanonicalOutput.Execution.BackgroundGate
import Proofs.Background.CompletionContinuation
import Proofs.Session.Executable

namespace CanonicalOutput.Execution.Handover

def notificationBinding (logical : RequestId) (message : MessageEnvelope) :
    BackgroundCompletion.NotificationBinding :=
  { messageId := message.header.id, sequence := message.sequence, wakeRequestId := logical }

def canonicalWakeBindings (world : World) (request : PhysicalRequestAdmission)
    (throughSequence : Transcript.Sequence) : List BackgroundCompletion.NotificationBinding :=
  (world.messages.filterMap fun message =>
    if message.header.session != request.session ||
        message.header.request != some request.document || message.sequence > throughSequence then none
    else match message.header.publication with
    | .toolDelivery document => match ownedToolByDocument? world document with
      | none => none
      | some tool =>
          let binding : WakeDocumentBinding :=
            { entry := request.entry, agent := request.agent, session := request.session
            , notificationMessageId := message.header.id, notificationSequence := message.sequence
            , wakeDocument := request.document, authenticated := request.authenticated }
          if ToolDelivery.wakeNotificationHeaderValid world tool binding message then
            some (notificationBinding request.entry.requestId message)
          else none
    | _ => none).dedup

def evidenceValid (world : World) (request : PhysicalRequestAdmission) : ClaimEvidence → Bool
  | .backgroundWake snapshot =>
      request.entry.source == .backgroundCompletion &&
        snapshot.wakeRequestId == request.entry.requestId &&
        snapshot.throughSequence + 1 == world.transcript.nextSeq &&
        snapshot.bindings == canonicalWakeBindings world request snapshot.throughSequence &&
        !snapshot.bindings.isEmpty
  | .ordinary => request.entry.source == .user || request.entry.source == .steering
  | .goalChild receipt =>
      receipt.request == request && receipt.authenticated && request.authenticated &&
        request.entry.source == .goal &&
        request.entry.queuedAfter == some receipt.parentLogical &&
        request.document != receipt.parentPhysical

def predecessorReady (world : World) : Bool :=
  match world.lease.lease with
  | .vacant => world.lease.request == .pending && world.terminalSelection.isNone
  | .terminal _ _ => world.terminalSelection.isSome
  | _ => false

def freshRequestWorld (old : World) (physical : DocId) (routes : List (DocId × Nat))
    (now : Time) : World :=
  { old with
    requestId := physical
    remoteRoutes := routes
    lease := { RequestExecutionLease.initial Generation with now := now }
    terminalSelection := none }

/-- Claim-owner kernel, not a complete application step. `SessionComposition`
initializes the new request's retry state in the same atomic activation; its
trace never admits this intermediate result on its own. -/
def claimAndActivate (state : World) (actor : Gate.Actor) (now : Time)
    (activation : Activation) : Option World :=
  let old := state
  if state.claimed.isSome || !predecessorReady old || activation.request.document == old.requestId ||
      state.gateOwner != some actor || state.gateSchedule.phase != .storage ||
      !StorageWriteGate.pollable state.gateSchedule || now < old.lease.now then none
  else if !activation.request.authenticated || !activation.routesAuthenticated ||
      activation.request.agent != old.principal ||
      activation.request.session != old.sessionId ||
      state.queue.scope.agent != old.principal ||
      state.queue.sessionId != old.sessionId ||
      activation.request.requester != state.queue.scope.requester ||
      !evidenceValid old activation.request activation.evidence then none
  else match SessionQueue.step? state.queue .claimNext with
  | none => none
  | some queue =>
      if queue.active != some activation.request.entry.requestId ||
          state.queue.pending.head? != some activation.request.entry then none
      else
        let base := freshRequestWorld old activation.request.document activation.configuredRoutes now
        match RequestExecutionLease.step? base.lease
            (.claim .mutationWriteGate activation.generation activation.duration activation.deadline) with
        | none => none
        | some lease => some
            { base with
              lease := lease
              gateOwner := state.gateOwner
              gateSchedule := { state.gateSchedule with phase := .releasable }
              queue := queue
              claimed := some
                { physicalRequest := activation.request.document
                , logicalRequest := activation.request.entry.requestId
                , session := activation.request.session, evidence := activation.evidence } }

theorem successful_claim_has_exact_binding
    (before after : World) (actor : Gate.Actor) (now : Time) (activation : Activation)
    (h : claimAndActivate before actor now activation = some after) :
    after.claimed = some
      { physicalRequest := activation.request.document
      , logicalRequest := activation.request.entry.requestId
      , session := activation.request.session, evidence := activation.evidence } ∧
    after.requestId = activation.request.document ∧
    after.queue.active = some activation.request.entry.requestId := by
  unfold claimAndActivate at h
  dsimp only at h
  repeat' first | contradiction | split at h
  all_goals cases h
  all_goals simp [freshRequestWorld] at *
  all_goals aesop

theorem successful_claim_preserves_session_facts
    (before after : World) (actor : Gate.Actor) (now : Time) (activation : Activation)
    (h : claimAndActivate before actor now activation = some after) :
    after.segments = before.segments ∧
    after.messages = before.messages ∧
    after.transcript = before.transcript ∧
    after.compactionCursor = before.compactionCursor ∧
    after.toolContexts = before.toolContexts ∧
    after.delegatedCalls = before.delegatedCalls ∧
    after.terminalSelection = none := by
  unfold claimAndActivate at h
  dsimp only at h
  repeat' first | contradiction |
    (solve | cases h; exact ⟨rfl, rfl, rfl, rfl, rfl, rfl, rfl⟩) | split at h

theorem successful_claim_preserves_nextSeq
    (before after : World) (actor : Gate.Actor) (now : Time) (activation : Activation)
    (h : claimAndActivate before actor now activation = some after) :
    after.transcript.nextSeq =
      before.transcript.nextSeq := by
  exact congrArg (fun transcript : Transcript.TranscriptState => transcript.nextSeq)
    (successful_claim_preserves_session_facts before after actor now activation h).2.2.1

def beginProcessing (state : World) (actor : Gate.Actor) (now : Time)
    (generation : Generation) : Option World :=
  match state.claimed with
  | none => none
  | some claimed =>
    let stored := state
    let world := Gate.atTime stored now
    if now < stored.lease.now ||
      claimed.physicalRequest != world.requestId || claimed.session != world.sessionId ||
      state.queue.active != some claimed.logicalRequest ||
      state.gateOwner != some actor || state.gateSchedule.phase != .storage ||
      !StorageWriteGate.pollable state.gateSchedule then none
    else match RequestExecutionLease.step? world.lease (.begin .mutationWriteGate generation) with
    | none => none
    | some lease =>
      some { world with lease := lease, gateSchedule :=
        { state.gateSchedule with phase := .releasable } }

def terminalOutcome? (world : World) : Option RequestExecutionLease.Outcome :=
  match world.lease.lease with
  | .terminal _ outcome => some outcome
  | _ => none

structure FinishResult where
  state : World
  acknowledged : List BackgroundCompletion.NotificationBinding
  evidence : ClaimEvidence
  outcome : RequestExecutionLease.Outcome

theorem finishActive_step_clears_active
    (before after : SessionQueue.SessionQueueState)
    (h : SessionQueue.step? before .finishActive = some after) : after.active = none := by
  cases ha : before.active with
  | none => simp [SessionQueue.step?, ha] at h
  | some logical => simp [SessionQueue.step?, ha] at h; cases h; rfl

def finishAndAcknowledge (state : World) (actor : Gate.Actor) : Option FinishResult :=
  match state.claimed with
  | none => none
  | some claimed =>
    let world := state
    if state.gateOwner != some actor || state.gateSchedule.phase != .storage ||
      !StorageWriteGate.pollable state.gateSchedule ||
      claimed.physicalRequest != world.requestId || claimed.session != world.sessionId ||
      state.queue.active != some claimed.logicalRequest then none
    else match terminalOutcome? world with
    | none => none
    | some outcome =>
      if outcome == .completed &&
      !(match world.terminalSelection with
        | some selection => terminalSelectionValid world selection
        | none => false) then none
      else match SessionQueue.step? state.queue .finishActive with
      | none => none
      | some queue =>
        let acknowledged := match claimed.evidence with
          | .backgroundWake snapshot =>
              if outcome == .completed then snapshot.attemptedBindings else []
          | .ordinary | .goalChild _ => []
        some ⟨
          { state with
            gateSchedule := { state.gateSchedule with phase := .releasable }
            queue := queue
            claimed := none },
          acknowledged, claimed.evidence, outcome ⟩

end CanonicalOutput.Execution.Handover
