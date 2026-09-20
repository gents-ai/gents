import Proofs.CanonicalOutput.Execution.BackgroundGate
import Proofs.Background.CompletionContinuation
import Proofs.Session.Executable

namespace CanonicalOutput.Execution.Handover

structure PhysicalRequestAdmission where
  document : DocId
  entry : SessionQueue.QueueEntry
  agent : Nat
  session : SessionId
  requester : Option Nat
  authenticated : Bool
  deriving DecidableEq, Repr

structure GoalChildReceipt where
  goalDocument : DocId
  request : PhysicalRequestAdmission
  parentPhysical : DocId
  parentLogical : RequestId
  authenticated : Bool
  deriving DecidableEq, Repr

inductive ClaimEvidence where
  | backgroundWake (snapshot : BackgroundCompletion.WakeAttemptSnapshot)
  | ordinary
  | goalChild (receipt : GoalChildReceipt)
  deriving DecidableEq, Repr

structure Activation where
  request : PhysicalRequestAdmission
  evidence : ClaimEvidence
  configuredRoutes : List (DocId × Nat)
  routesAuthenticated : Bool
  generation : Generation
  duration : Time
  deadline : Time
  deriving DecidableEq, Repr

structure ClaimedBinding where
  physicalRequest : DocId
  logicalRequest : RequestId
  session : SessionId
  evidence : ClaimEvidence
  deriving DecidableEq, Repr

structure State where
  paired : BackgroundGate.State
  claimed : Option ClaimedBinding := none

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

def claimAndActivate (state : State) (actor : Gate.Actor) (now : Time)
    (activation : Activation) : Option State :=
  let old := state.paired.gate.execution
  if state.claimed.isSome || !predecessorReady old || activation.request.document == old.requestId ||
      state.paired.gate.owner != some actor || state.paired.gate.schedule.phase != .storage ||
      !StorageWriteGate.pollable state.paired.gate.schedule || now < old.lease.now then none
  else if !activation.request.authenticated || !activation.routesAuthenticated ||
      activation.request.agent != old.principal ||
      activation.request.session != old.sessionId ||
      state.paired.queue.scope.agent != old.principal ||
      state.paired.queue.sessionId != old.sessionId ||
      activation.request.requester != state.paired.queue.scope.requester ||
      !evidenceValid old activation.request activation.evidence then none
  else match SessionQueue.step? state.paired.queue .claimNext with
  | none => none
  | some queue =>
      if queue.active != some activation.request.entry.requestId ||
          state.paired.queue.pending.head? != some activation.request.entry then none
      else
        let base := freshRequestWorld old activation.request.document activation.configuredRoutes now
        match RequestExecutionLease.step? base.lease
            (.claim .mutationWriteGate activation.generation activation.duration activation.deadline) with
        | none => none
        | some lease => some
            { paired :=
                { gate :=
                    { execution := { base with lease := lease }, owner := state.paired.gate.owner
                    , schedule := { state.paired.gate.schedule with phase := .releasable } }
                , queue := queue }
            , claimed := some
                { physicalRequest := activation.request.document
                , logicalRequest := activation.request.entry.requestId
                , session := activation.request.session, evidence := activation.evidence } }

theorem successful_claim_has_exact_binding
    (before after : State) (actor : Gate.Actor) (now : Time) (activation : Activation)
    (h : claimAndActivate before actor now activation = some after) :
    after.claimed = some
      { physicalRequest := activation.request.document
      , logicalRequest := activation.request.entry.requestId
      , session := activation.request.session, evidence := activation.evidence } ∧
    after.paired.gate.execution.requestId = activation.request.document ∧
    after.paired.queue.active = some activation.request.entry.requestId := by
  unfold claimAndActivate at h
  dsimp only at h
  repeat' first | contradiction | split at h
  all_goals cases h
  all_goals simp [freshRequestWorld] at *
  all_goals aesop

theorem successful_claim_preserves_session_facts
    (before after : State) (actor : Gate.Actor) (now : Time) (activation : Activation)
    (h : claimAndActivate before actor now activation = some after) :
    after.paired.gate.execution.segments = before.paired.gate.execution.segments ∧
    after.paired.gate.execution.messages = before.paired.gate.execution.messages ∧
    after.paired.gate.execution.transcript = before.paired.gate.execution.transcript ∧
    after.paired.gate.execution.compactionCursor = before.paired.gate.execution.compactionCursor ∧
    after.paired.gate.execution.toolContexts = before.paired.gate.execution.toolContexts ∧
    after.paired.gate.execution.delegatedCalls = before.paired.gate.execution.delegatedCalls ∧
    after.paired.gate.execution.terminalSelection = none := by
  unfold claimAndActivate at h
  dsimp only at h
  repeat' first | contradiction |
    (solve | cases h; exact ⟨rfl, rfl, rfl, rfl, rfl, rfl, rfl⟩) | split at h

theorem successful_claim_preserves_nextSeq
    (before after : State) (actor : Gate.Actor) (now : Time) (activation : Activation)
    (h : claimAndActivate before actor now activation = some after) :
    after.paired.gate.execution.transcript.nextSeq =
      before.paired.gate.execution.transcript.nextSeq := by
  exact congrArg (fun transcript : Transcript.TranscriptState => transcript.nextSeq)
    (successful_claim_preserves_session_facts before after actor now activation h).2.2.1

def beginProcessing (state : State) (actor : Gate.Actor) (now : Time)
    (generation : Generation) : Option State :=
  match state.claimed with
  | none => none
  | some claimed =>
    let stored := state.paired.gate.execution
    let world := Gate.atTime stored now
    if now < stored.lease.now ||
      claimed.physicalRequest != world.requestId || claimed.session != world.sessionId ||
      state.paired.queue.active != some claimed.logicalRequest ||
      state.paired.gate.owner != some actor || state.paired.gate.schedule.phase != .storage ||
      !StorageWriteGate.pollable state.paired.gate.schedule then none
    else match RequestExecutionLease.step? world.lease (.begin .mutationWriteGate generation) with
    | none => none
    | some lease =>
      let gate := { state.paired.gate with
        execution := { world with lease := lease }
        schedule := { state.paired.gate.schedule with phase := .releasable } }
      some { state with paired := { state.paired with gate := gate } }

def terminalOutcome? (world : World) : Option RequestExecutionLease.Outcome :=
  match world.lease.lease with
  | .terminal _ outcome => some outcome
  | _ => none

structure FinishResult where
  state : State
  acknowledged : List BackgroundCompletion.NotificationBinding
  evidence : ClaimEvidence
  outcome : RequestExecutionLease.Outcome

theorem finishActive_step_clears_active
    (before after : SessionQueue.SessionQueueState)
    (h : SessionQueue.step? before .finishActive = some after) : after.active = none := by
  cases ha : before.active with
  | none => simp [SessionQueue.step?, ha] at h
  | some logical => simp [SessionQueue.step?, ha] at h; cases h; rfl

def finishAndAcknowledge (state : State) (actor : Gate.Actor) : Option FinishResult :=
  match state.claimed with
  | none => none
  | some claimed =>
    let world := state.paired.gate.execution
    if state.paired.gate.owner != some actor || state.paired.gate.schedule.phase != .storage ||
      !StorageWriteGate.pollable state.paired.gate.schedule ||
      claimed.physicalRequest != world.requestId || claimed.session != world.sessionId ||
      state.paired.queue.active != some claimed.logicalRequest then none
    else match terminalOutcome? world with
    | none => none
    | some outcome =>
      if outcome == .completed &&
      !(match world.terminalSelection with
        | some selection => terminalSelectionValid world selection
        | none => false) then none
      else match SessionQueue.step? state.paired.queue .finishActive with
      | none => none
      | some queue =>
        let acknowledged := match claimed.evidence with
          | .backgroundWake snapshot =>
              if outcome == .completed then snapshot.attemptedBindings else []
          | .ordinary | .goalChild _ => []
        some ⟨
          { paired :=
              { gate := { state.paired.gate with
                  schedule := { state.paired.gate.schedule with phase := .releasable } }
              , queue := queue }
          , claimed := none },
          acknowledged, claimed.evidence, outcome ⟩

end CanonicalOutput.Execution.Handover
