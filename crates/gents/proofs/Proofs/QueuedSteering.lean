import Proofs.Goals
import Proofs.Session.Interrupt
import Proofs.Request.Executable
import Proofs.ClientShell.Projection
import Proofs.CanonicalOutput.Execution.Gate
import Proofs.CanonicalOutput.Execution.Handover
import Proofs.CanonicalOutput.Execution.SessionComposition
import Proofs.CanonicalOutput.Execution.Examples
import Proofs.RenderedCapture

namespace QueuedSteering

open CanonicalOutput
open CanonicalOutput.Execution

inductive AgentDelivery where
  | queue | steer | interrupt
  deriving DecidableEq, Repr

def resolveAgentDelivery (delivery : Option AgentDelivery) (legacyInterrupt : Option Bool) :
    Option AgentDelivery :=
  match delivery, legacyInterrupt with
  | none, none | none, some false => some .queue
  | none, some true => some .interrupt
  | some mode, none => some mode
  | some .interrupt, some true => some .interrupt
  | some .queue, some false => some .queue
  | some .steer, some false => some .steer
  | some _, some _ => none

def agentDeliveryCases : List (Option AgentDelivery × Option Bool) :=
  [ (none, none), (none, some false), (none, some true)
  , (some .queue, none), (some .queue, some false), (some .queue, some true)
  , (some .steer, none), (some .steer, some false), (some .steer, some true)
  , (some .interrupt, none), (some .interrupt, some false), (some .interrupt, some true) ]

/-- Select the physical active head before publishing the delivered request.
The request created by this publication cannot become its own interrupt target. -/
def interruptDeliveryTarget (_planned activeBefore : Option Nat) : Option Nat := activeBefore

/-- An interrupt directed at the local session pauses its autonomous goal,
including when no request is active at publication. -/
def interruptDeliveryGoal (localTarget : Bool) (goal : Goals.State) : Goals.State :=
  if localTarget then (Goals.step? goal .pause).getD goal else goal

theorem idle_local_interrupt_pauses_goal :
    (interruptDeliveryGoal true ⟨.active, 0, false, false⟩).status = .paused := by decide

def interruptDeliveryCases : List (Option Nat × Option Nat) :=
  [(none, none), (none, some 1), (some 1, some 1), (some 1, none), (some 1, some 2)]

theorem idle_interrupt_delivery_does_not_interrupt_new_request :
    interruptDeliveryTarget none none = none := by decide

theorem interrupt_delivery_keeps_the_prepublication_head :
    interruptDeliveryTarget (some 1) (some 2) = some 2 := by decide

inductive AgentSteerDisposition where
  | start | steer | refuse
  deriving DecidableEq, Repr

def agentSteerDisposition (active compatible : Bool) : AgentSteerDisposition :=
  if !active then .start else if compatible then .steer else .refuse

def agentSteerDispositionCases : List (Bool × Bool) :=
  [(false, false), (false, true), (true, false), (true, true)]

theorem incompatible_active_agent_steer_refused :
    agentSteerDisposition true false = .refuse := by decide

/-- An agent steer is planned against the active turn it observed and names
that turn as `queued_after`. Only that turn may take it at a tool boundary;
once the turn ends the steer is an ordinary later turn and never enters the
next active turn, whose work the sender did not see. -/
def agentSteerTargets (plannedHead activeHead : Nat) : Bool := plannedHead == activeHead

def agentSteerTargetCases : List (Nat × Nat) := [(1, 1), (1, 2)]

theorem agent_steer_does_not_enter_a_later_turn (planned active : Nat)
    (h : planned ≠ active) : agentSteerTargets planned active = false := by
  simp [agentSteerTargets, h]

/-- An agent steer may pass ordinary queued turns without moving or consuming them.
Earlier steering inputs retain their own order and block later steers when incompatible. -/
inductive BoundaryInputKind where
  | queued | userSteer | agentSteer | blocked
  deriving DecidableEq, Repr

def boundaryCandidateAux (passedQueue : Bool) : List (Nat × BoundaryInputKind) → Option Nat
  | [] => none
  | (_, .queued) :: rest => boundaryCandidateAux true rest
  | (id, .agentSteer) :: _ => some id
  | (id, .userSteer) :: _ => if passedQueue then none else some id
  | (_, .blocked) :: _ => none

def boundaryCandidate (inputs : List (Nat × BoundaryInputKind)) : Option Nat :=
  boundaryCandidateAux false inputs

/-- Earlier compatible steers are consumed in arrival order. An ordinary
queued turn may be passed by an agent steer but not by a user steer. -/
def agentSteerReachable (target : Nat) (inputs : List (Nat × BoundaryInputKind × Bool)) : Bool :=
  let rec walk (passedQueue : Bool) : List (Nat × BoundaryInputKind × Bool) → Bool
    | [] => false
    | (_, .queued, _) :: rest => walk true rest
    | (_, .blocked, _) :: _ => false
    | (id, .userSteer, compatible) :: rest =>
        !passedQueue && compatible && (id == target || walk false rest)
    | (id, .agentSteer, compatible) :: rest =>
        compatible && (id == target || walk passedQueue rest)
  walk false inputs

def agentSteerReachabilityCases : List (List (Nat × BoundaryInputKind × Bool) × Nat) :=
  [ ([(1, .agentSteer, true)], 1)
  , ([(1, .agentSteer, true), (2, .agentSteer, true)], 2)
  , ([(1, .userSteer, true), (2, .agentSteer, true)], 2)
  , ([(1, .queued, true), (2, .agentSteer, true), (3, .agentSteer, true)], 3)
  , ([(1, .queued, true), (2, .userSteer, true), (3, .agentSteer, true)], 3)
  , ([(1, .agentSteer, false), (2, .agentSteer, true)], 2)
  , ([(1, .blocked, true), (2, .agentSteer, true)], 2)
  , ([(1, .agentSteer, false)], 1) ]

def boundaryCases : List (List (Nat × BoundaryInputKind)) :=
  [ [], [(1, .queued)], [(1, .agentSteer)], [(1, .userSteer)]
  , [(1, .queued), (2, .agentSteer), (3, .queued)]
  , [(1, .userSteer), (2, .agentSteer)]
  , [(1, .queued), (2, .userSteer), (3, .agentSteer)]
  , [(1, .queued), (2, .queued), (3, .agentSteer), (4, .agentSteer)]
  , [(1, .blocked), (2, .agentSteer)]
  , [(1, .queued), (2, .blocked), (3, .agentSteer)] ]

theorem agent_steer_passes_queued_turns :
    boundaryCandidate [(1, .queued), (2, .agentSteer), (3, .queued)] = some 2 := by decide

theorem earlier_steering_is_not_overtaken :
    boundaryCandidate [(1, .userSteer), (2, .agentSteer)] = some 1 ∧
    boundaryCandidate [(1, .queued), (2, .userSteer), (3, .agentSteer)] = none := by decide

structure FoldedScope where
  node : Nat
  session : Nat
  requester : Option Nat
  agent : Nat
  deriving DecidableEq, Repr

def foldedScopeMatches (caused head : FoldedScope) : Bool := caused == head

theorem foreign_requester_folded_head_rejected :
    foldedScopeMatches ⟨1, 2, some 3, 7⟩ ⟨1, 2, some 4, 7⟩ = false := by decide

def foldedBindingValid (caused head : FoldedScope) (namedHead actualHead : Nat) : Bool :=
  foldedScopeMatches caused head && namedHead == actualHead

structure FoldedBindingCase where
  causedRequester : Nat
  headRequester : Nat
  causedAgent : Nat
  headAgent : Nat
  namedHead : Nat
  actualHead : Nat

def foldedBindingCases : List FoldedBindingCase :=
  [ ⟨3, 3, 7, 7, 4, 4⟩, ⟨3, 4, 7, 7, 4, 4⟩
  , ⟨3, 3, 7, 7, 4, 5⟩, ⟨3, 3, 7, 8, 4, 4⟩ ]

theorem mismatched_folded_head_rejected :
    foldedBindingValid ⟨1, 2, some 3, 7⟩ ⟨1, 2, some 3, 7⟩ 4 5 = false := by decide

theorem foreign_agent_folded_head_rejected :
    foldedBindingValid ⟨1, 2, some 3, 7⟩ ⟨1, 2, some 3, 8⟩ 4 4 = false := by decide

theorem blocked_input_cannot_be_overtaken :
    boundaryCandidate [(1, .blocked), (2, .agentSteer)] = none := by decide

/-- A folded request follows its exact physical head; unrelated supersession stays terminal. -/
inductive AgentMessageOutcome where
  | pending | completed | failed | superseded
  deriving DecidableEq, Repr

def agentMessageOutcome (folded : Bool) (caused head : AgentMessageOutcome) : AgentMessageOutcome :=
  if folded && caused == .superseded then head else caused

def agentMessageOutcomeCases : List (Bool × AgentMessageOutcome × AgentMessageOutcome) :=
  [ (true, .superseded, .pending), (true, .superseded, .completed)
  , (true, .superseded, .failed), (false, .superseded, .completed)
  , (true, .completed, .failed) ]

theorem folded_agent_message_waits_for_its_head :
    agentMessageOutcome true .superseded .pending = .pending ∧
    agentMessageOutcome true .superseded .completed = .completed := by decide

theorem ordinary_supersession_stays_terminal :
    agentMessageOutcome false .superseded .completed = .superseded := by decide

theorem legacy_agent_delivery_defaults :
    resolveAgentDelivery none none = some .queue ∧
    resolveAgentDelivery none (some false) = some .queue ∧
    resolveAgentDelivery none (some true) = some .interrupt := by
  decide

theorem conflicting_agent_delivery_rejected :
    resolveAgentDelivery (some .queue) (some true) = none ∧
    resolveAgentDelivery (some .steer) (some true) = none ∧
    resolveAgentDelivery (some .interrupt) (some false) = none := by
  decide

structure AdmissionInput where
  requestId : RequestId
  requestDocId : Nat
  contentToken : Nat
  deriving DecidableEq, Repr

structure PreparedInput where
  closing : Segment
  message : MessageEnvelope
  deriving DecidableEq, Repr

structure World where
  queue : SessionQueue.SessionQueueState
  request : RequestContext
  execution : Option CanonicalOutput.Execution.World
  input : AdmissionInput
  accepted : Option AdmissionInput
  prepared : Option PreparedInput
  captured : Option RenderedCapture.Scenario
  lastSendAllowed : Option Bool

def hasExactAuthoredOwner (w : World) : Bool :=
  match w.execution, w.prepared with
  | some execution, some prepared =>
      execution.requestId == w.input.requestDocId &&
        authoredPublicationPresent execution prepared.closing prepared.message &&
        (match reconstructMessage execution.segments noDeniedDocuments prepared.message with
          | .ok _ => true
          | .error _ => false)
  | _, _ => false

def canonicalAuthoredCount (w : World) : Nat :=
  match w.execution, w.prepared with
  | some execution, some prepared =>
      (execution.messages.filter fun message =>
        message.header.request == some w.input.requestDocId &&
          message.header.publication == prepared.message.header.publication).length
  | _, _ => 0

def admissionVisible (w : World) : Bool :=
  (w.accepted == some w.input) && projectPendingUserTurn (hasExactAuthoredOwner w)

def enqueue? (w : World) (entry : SessionQueue.QueueEntry) : Option World :=
  if entry.requestId != w.input.requestId || w.request.state != .pending then none
  else match SessionQueue.step? w.queue (.appendPending entry) with
    | none => none
    | some queue => some { w with queue, accepted := some w.input }

def interruptBeforeClaim? (w : World) : Option World := do
  if w.execution.isSome then none
  let request ← RequestContext.step? w.request .interruptBeforeClaim
  pure { w with request }

structure InterruptTarget where
  requestDocId : Nat
  agent : Nat
  requester : Option Nat
  deriving DecidableEq, Repr

def currentInterruptTarget (w : World) : InterruptTarget :=
  ⟨w.input.requestDocId, w.queue.scope.node, w.queue.scope.requester⟩

def latchInterrupt? (w : World) (target : InterruptTarget) : Option World :=
  if target != currentInterruptTarget w then none
  else do
    let (request, queue) ← SessionQueue.latchInterruptScoped
      w.queue.scope w.request w.queue
    let execution ← match w.execution with
      | none => some none
      | some current =>
          if current.queue != w.queue then none
          else some (some { current with queue })
    pure { w with request, queue, execution }

theorem mismatched_interrupt_target_rejected (w : World) (target : InterruptTarget)
    (hmismatch : target ≠ currentInterruptTarget w) :
    latchInterrupt? w target = none := by
  simp [latchInterrupt?, hmismatch]

theorem latch_interrupt_preserves_existing_stamp
    {before after : World} {target : InterruptTarget} {stamp : Time}
    (hstamp : before.request.interruptRequestedAt = some stamp)
    (hlatch : latchInterrupt? before target = some after) :
    after.request.interruptRequestedAt = some stamp := by
  by_cases htarget : target = currentInterruptTarget before
  · have hscope : SessionQueue.latchInterruptScoped before.queue.scope
        before.request before.queue = some (before.request, before.queue) := by
      simp [SessionQueue.latchInterruptScoped,
        SessionQueue.latchInterruptObservedScoped,
        SessionQueue.latchInterruptObserved, hstamp]
    simp only [latchInterrupt?, htarget, ↓reduceIte, hscope, Option.bind_some,
      Option.map_some, id_eq] at hlatch
    cases hexecution : before.execution with
    | none =>
        simp [hexecution] at hlatch
        cases hlatch
        exact hstamp
    | some current =>
        by_cases hqueue : current.queue = before.queue
        · simp [hexecution, hqueue] at hlatch
          cases hlatch
          exact hstamp
        · simp [hexecution, hqueue] at hlatch
  · simp [latchInterrupt?, htarget] at hlatch

theorem latch_interrupt_preserves_execution_join
    {before after : World} {target : InterruptTarget}
    (hlatch : latchInterrupt? before target = some after) :
    (∀ execution, after.execution = some execution → execution.queue = after.queue) ∧
      after.execution.map (·.lease) = before.execution.map (·.lease) ∧
      after.execution.map (·.requestId) = before.execution.map (·.requestId) ∧
      after.input = before.input ∧ after.accepted = before.accepted := by
  unfold latchInterrupt? at hlatch
  split at hlatch
  · contradiction
  · cases hscope : SessionQueue.latchInterruptScoped before.queue.scope
        before.request before.queue with
    | none => simp [hscope] at hlatch
    | some result =>
        rcases result with ⟨request, queue⟩
        simp only [hscope, Option.bind_some, Option.map_some, id_eq] at hlatch
        cases hexecution : before.execution with
        | none =>
            simp [hexecution] at hlatch
            cases hlatch
            simp
        | some current =>
            by_cases hqueue : current.queue = before.queue
            · simp [hexecution, hqueue] at hlatch
              cases hlatch
              simp
            · simp [hexecution, hqueue] at hlatch

def terminateBeforeStart? (w : World) (action : RequestContext.Action) : Option World := do
  if w.execution.isSome then none
  let request ← RequestContext.step? w.request action
  if !decide (isTerminal request.state) then none
  let queue ← if w.queue.active == some w.input.requestId then
      SessionQueue.step? w.queue .finishActive else some w.queue
  pure { w with request, queue }

private structure OwnedTerminalHandoff where
  request : RequestContext
  execution : CanonicalOutput.Execution.World
  outcome : RequestExecutionLease.Outcome
  queueCleared : execution.queue.active = none

private def ownedTerminalHandoff? (w : World) (actor now generation : Nat)
    (action : RequestContext.Action) : Option OwnedTerminalHandoff := do
  if w.prepared.isSome then none
  let outcome ← match action with
    | .fail => some RequestExecutionLease.Outcome.failed
    | .interruptProcessing => some RequestExecutionLease.Outcome.interrupted
    | _ => none
  let request ← RequestContext.step? w.request action
  let current ← w.execution
  if current.queue != w.queue || current.requestId != w.input.requestDocId then none
  let released ← Gate.scheduling current actor .release
  let held ← Gate.acquire released actor true
  let terminal ← Gate.commit held actor now (.terminalize generation outcome .noMessage)
  let finishReleased ← Gate.scheduling terminal actor .release
  let finishHeld ← Gate.acquire finishReleased actor true
  if hnormal : finishHeld.purpose = .normal then
    match hfinish : Handover.finishAndAcknowledge finishHeld actor with
    | none => none
    | some finished =>
        if finished.outcome != outcome then none
        else some ⟨request, finished.state, outcome,
          (Handover.successful_finish_clears_claim_control finishHeld finished actor hfinish).2.1 hnormal⟩
  else none

def terminateOwnedBeforePublication? (w : World) (actor now generation : Nat)
    (action : RequestContext.Action) : Option World := do
  let handoff ← ownedTerminalHandoff? w actor now generation action
  if handoff.request.state != handoff.outcome.requestState ||
      handoff.execution.lease.request != handoff.request.state ||
      handoff.execution.lease.lease != .terminal generation handoff.outcome then none
  pure { w with
    request := handoff.request,
    execution := some (handoff.execution),
    queue := handoff.execution.queue }

theorem owned_terminal_before_publication_retains_input_and_finishes_queue
    {before after : World} {actor now generation : Nat} {action : RequestContext.Action}
    (hp : before.prepared = none)
    (ha : before.accepted = some before.input)
    (ht : terminateOwnedBeforePublication? before actor now generation action = some after) :
    after.input = before.input ∧ after.accepted = some before.input ∧
      after.prepared = none ∧ after.queue.active = none ∧
      ∃ execution outcome, after.execution = some execution ∧
        execution.lease.lease = .terminal generation outcome ∧
        execution.lease.request = after.request.state ∧
        after.request.state = outcome.requestState := by
  unfold terminateOwnedBeforePublication? at ht
  cases hh : ownedTerminalHandoff? before actor now generation action with
  | none => simp [hh] at ht
  | some handoff =>
      simp [hh] at ht
      rcases ht with ⟨⟨⟨hrequest, hleaseRequest⟩, hleaseTerminal⟩, rfl⟩
      exact ⟨rfl, ha, hp, handoff.queueCleared, handoff.execution, handoff.outcome,
        rfl, hleaseTerminal, hleaseRequest, hrequest⟩

def claimWithoutBegin? (w : World) : Option World := do
  if w.execution.isSome then none
  let queue ← SessionQueue.step? w.queue .claimNext
  if queue.active != some w.input.requestId then none
  let request ← RequestContext.step? w.request .claim
  pure { w with queue, request }

theorem claim_without_begin_activates_and_preserves_input
    {before claimed : World}
    (hc : claimWithoutBegin? before = some claimed) :
    claimed.queue.active = some before.input.requestId ∧
      claimed.input = before.input ∧ claimed.accepted = before.accepted ∧
      claimed.execution = none ∧ claimed.prepared = before.prepared := by
  unfold claimWithoutBegin? at hc
  cases hx : before.execution with
  | some execution => simp [hx] at hc
  | none =>
      cases hqueue : SessionQueue.step? before.queue .claimNext with
      | none => simp [hx, hqueue] at hc
      | some queue =>
          by_cases hactive : queue.active = some before.input.requestId
          · cases hrequest : RequestContext.step? before.request .claim with
            | none => simp [hx, hqueue, hactive, hrequest] at hc
            | some request =>
                simp [hx, hqueue, hactive, hrequest] at hc
                cases hc
                simp [hactive, hx]
          · simp [hx, hqueue, hactive] at hc

def claimAndBegin? (w : World) (generation : Nat)
    (execution : CanonicalOutput.Execution.World) : Option World := do
  if w.execution.isSome then none
  let queue ← SessionQueue.step? w.queue .claimNext
  if queue.active != some w.input.requestId then none
  let requestClaimed ← RequestContext.step? w.request .claim
  let requestAcquired := { requestClaimed with admission := .acquired }
  let request ← RequestContext.step? requestAcquired .beginInference
  if execution.requestId != w.input.requestDocId || execution.queue != queue ||
      execution.lease.request != request.state ||
      execution.currentGeneration? != some generation ||
      execution.claimed.map (·.logicalRequest) != some w.input.requestId then none
  pure { w with queue, request, execution := some execution }

private theorem terminal_request_claim_step_none (w : World)
    (hterminal : isTerminal w.request.state) :
    RequestContext.step? w.request .claim = none := by
  have hnotPending : w.request.state ≠ .pending := by
    intro hp
    rw [hp] at hterminal
    change RequestState.pending = .completed ∨ RequestState.pending = .failed ∨
      RequestState.pending = .superseded ∨ RequestState.pending = .dead ∨
      RequestState.pending = .interrupted at hterminal
    simp at hterminal
  simp [RequestContext.step?, hnotPending]

theorem terminal_request_cannot_claim_without_begin (w : World)
    (hterminal : isTerminal w.request.state) : claimWithoutBegin? w = none := by
  simp [claimWithoutBegin?, terminal_request_claim_step_none w hterminal]

theorem terminal_request_cannot_claim_and_begin (w : World) (generation : Nat)
    (execution : CanonicalOutput.Execution.World)
    (hterminal : isTerminal w.request.state) :
    claimAndBegin? w generation execution = none := by
  simp [claimAndBegin?, terminal_request_claim_step_none w hterminal]

def publishAtExecutionStart? (w : World) (actor now generation : Nat)
    (candidate : Option PreparedInput) : Option World := do
  let prepared ← candidate
  if w.request.state != .processing then none
  let current ← w.execution
  if current.requestId != w.input.requestDocId ||
      current.lease.request != w.request.state ||
      prepared.message.header.request != some w.input.requestDocId then none
  let released ← Gate.scheduling current actor .release
  let held ← Gate.acquire released actor true
  let execution ← Gate.commit held actor now
    (.authored generation prepared.closing prepared.message)
  pure { w with execution := some execution, prepared := some prepared }

/-- Authored-input and capture fence only: not a general provider-lease
authorization, and not a proof that opaque serialized bytes encode `prepared`
(`RenderedCapture` fences those separately). -/
def providerSendPermitted (w : World) (prepared : PreparedInput)
    (capture : RenderedCapture.Scenario) : Bool :=
  (w.request.state == .processing) &&
    (w.prepared == some prepared) && hasExactAuthoredOwner w &&
    (match w.execution with
      | none => false
      | some execution =>
          capture.key.requestId == w.input.requestDocId &&
            capture.key.sessionId == execution.sessionId &&
            capture.key.nodeDid == execution.nodeDid &&
            capture.sendPermitted)

theorem interrupted_before_claim_retains_exact_input
    {before queued interrupted : World} {entry : SessionQueue.QueueEntry}
    (hx : before.execution = none)
    (hp : before.prepared = none)
    (he : enqueue? before entry = some queued)
    (hi : interruptBeforeClaim? queued = some interrupted) :
    interrupted.request.state = .interrupted ∧ interrupted.accepted = some before.input ∧
      interrupted.prepared = none ∧ admissionVisible interrupted = true := by
  unfold enqueue? at he
  split at he <;> try contradiction
  split at he <;> try contradiction
  rename_i queue hqueue
  cases he
  unfold interruptBeforeClaim? at hi
  simp [hx] at hi
  generalize hs : RequestContext.step? before.request .interruptBeforeClaim = result at hi
  cases result with
  | none => simp at hi
  | some request =>
      simp at hi
      cases hi
      have hstate : request.state = .interrupted := by
        simp [RequestContext.step?] at hs
        rcases hs with ⟨_, rfl⟩
        rfl
      simp [hstate, hx, hp, admissionVisible, hasExactAuthoredOwner,
        projectPendingUserTurn]

theorem provider_send_requires_canonical_prepared_input
    (w : World) (prepared : PreparedInput) (capture : RenderedCapture.Scenario)
    (h : providerSendPermitted w prepared capture = true) :
    hasExactAuthoredOwner w = true ∧
      w.prepared = some prepared ∧ capture.sendPermitted = true := by
  simp only [providerSendPermitted, Bool.and_eq_true] at h
  rcases h with ⟨⟨⟨_, hprepared⟩, howner⟩, hcapture⟩
  cases he : w.execution with
  | none => simp [hasExactAuthoredOwner, he] at howner
  | some execution =>
      simp only [he, Bool.and_eq_true] at hcapture
      exact ⟨howner, by simpa using hprepared, hcapture.2⟩

theorem provider_send_implies_physical_authored_publication
    (w : World) (prepared : PreparedInput) (capture : RenderedCapture.Scenario)
    (h : providerSendPermitted w prepared capture = true) :
    ∃ execution, w.execution = some execution ∧
      authoredPublicationPresent execution prepared.closing prepared.message = true ∧
      capture.sendPermitted = true := by
  obtain ⟨howner, hprepared, hcapture⟩ :=
    provider_send_requires_canonical_prepared_input w prepared capture h
  cases he : w.execution with
  | none => simp [hasExactAuthoredOwner, he] at howner
  | some execution =>
      refine ⟨execution, ?_, ?_, hcapture⟩
      · simp only [he]
      simp [hasExactAuthoredOwner, he, hprepared, Bool.and_eq_true] at howner
      exact howner.1.2

theorem provider_send_implies_reconstructible_prepared_input
    (w : World) (prepared : PreparedInput) (capture : RenderedCapture.Scenario)
    (h : providerSendPermitted w prepared capture = true) :
    ∃ execution native, w.execution = some execution ∧
      reconstructMessage execution.segments noDeniedDocuments prepared.message = .ok native := by
  obtain ⟨howner, hprepared, _⟩ :=
    provider_send_requires_canonical_prepared_input w prepared capture h
  cases he : w.execution with
  | none => simp [hasExactAuthoredOwner, he] at howner
  | some execution =>
      simp [hasExactAuthoredOwner, he, hprepared, Bool.and_eq_true] at howner
      cases hr : reconstructMessage execution.segments noDeniedDocuments prepared.message with
      | error error => simp [hr] at howner
      | ok native => exact ⟨execution, native, by simp only [he], hr⟩

theorem prestart_cannot_send
    (w : World) (prepared : PreparedInput) (capture : RenderedCapture.Scenario)
    (h : w.execution = none) :
    providerSendPermitted w prepared capture = false := by
  simp [providerSendPermitted, hasExactAuthoredOwner, h]

theorem terminal_before_start_retains_admission_input
    {before after : World} {action : RequestContext.Action}
    (hx : before.execution = none)
    (hp : before.prepared = none)
    (ha : before.accepted = some before.input)
    (ht : terminateBeforeStart? before action = some after) :
    isTerminal after.request.state ∧
      after.accepted = some before.input ∧ after.input = before.input ∧
      after.execution = none ∧ after.prepared = none ∧
      admissionVisible after = true ∧ canonicalAuthoredCount after = 0 := by
  unfold terminateBeforeStart? at ht
  simp [hx] at ht
  cases hs : RequestContext.step? before.request action with
  | none => simp [hs] at ht
  | some request =>
      by_cases hterm : isTerminal request.state
      · by_cases hactive : before.queue.active = some before.input.requestId
        · simp [hs, hterm, hactive] at ht
          cases hqueue : SessionQueue.step? before.queue .finishActive with
          | none => simp [hqueue] at ht
          | some queue =>
              simp [hqueue] at ht
              cases ht
              simp [hterm, hx, hp, ha, admissionVisible, canonicalAuthoredCount,
                hasExactAuthoredOwner, projectPendingUserTurn]
        · simp [hs, hterm, hactive] at ht
          cases ht
          simp [hterm, hx, hp, ha, admissionVisible, canonicalAuthoredCount,
            hasExactAuthoredOwner, projectPendingUserTurn]
      · simp [hs, hterm] at ht

theorem terminal_before_start_clears_owned_active
    {before after : World} {action : RequestContext.Action}
    (hactive : before.queue.active = some before.input.requestId)
    (ht : terminateBeforeStart? before action = some after) :
    after.queue.active = none := by
  cases hx : before.execution with
  | some execution => simp [terminateBeforeStart?, hx] at ht
  | none =>
      cases hs : RequestContext.step? before.request action with
      | none => simp [terminateBeforeStart?, hx, hs] at ht
      | some request =>
          by_cases hterminal : isTerminal request.state
          · have hfinish : SessionQueue.step? before.queue .finishActive =
                some (before.queue.finishActive before.input.requestId) := by
              simp [SessionQueue.step?, hactive]
            simp [terminateBeforeStart?, hx, hs, hterminal, hactive, hfinish] at ht
            cases ht
            rfl
          · simp [terminateBeforeStart?, hx, hs, hterminal] at ht

theorem claimed_prestart_terminal_retains_input_and_clears_queue
    {before claimed after : World} {action : RequestContext.Action}
    (hp : before.prepared = none)
    (ha : before.accepted = some before.input)
    (hc : claimWithoutBegin? before = some claimed)
    (ht : terminateBeforeStart? claimed action = some after) :
    after.queue.active = none ∧ after.accepted = some before.input ∧
      after.input = before.input ∧ canonicalAuthoredCount after = 0 := by
  obtain ⟨hactive, hinput, haccepted, hexecution, hprepared⟩ :=
    claim_without_begin_activates_and_preserves_input hc
  have hclaimedAccepted : claimed.accepted = some claimed.input := by
    rw [haccepted, ha, hinput]
  have hclaimedPrepared : claimed.prepared = none := by rw [hprepared, hp]
  have hclaimedActive : claimed.queue.active = some claimed.input.requestId := by
    simpa [hinput] using hactive
  obtain ⟨_, hafterAccepted, hafterInput, _, _, _, hcount⟩ :=
    terminal_before_start_retains_admission_input hexecution hclaimedPrepared
      hclaimedAccepted ht
  exact ⟨terminal_before_start_clears_owned_active hclaimedActive ht,
    by simpa [hinput] using hafterAccepted,
    by simpa [hinput] using hafterInput, hcount⟩

inductive Action where
  | enqueue
  | claimWithoutBegin
  | claimAndBegin
  | latchInterrupt
  | terminate (transition : RequestContext.Action)
  | publish
  | prepareFails
  | capture
  | send
  deriving DecidableEq, Repr

structure Script where
  name : String
  input : AdmissionInput
  entry : SessionQueue.QueueEntry
  interruptAt : Option Time
  candidate : Option PreparedInput
  capture : RenderedCapture.Scenario
  actions : List Action
  deriving Repr

structure TraceObservation where
  name : String
  caseScript : Script
  requestId : Nat
  requestDocId : Nat
  contentToken : Nat
  lifecycleState : String
  acceptedInput : Bool
  queueActive : Option RequestId
  admissionVisible : Bool
  canonicalAuthoredCount : Nat
  providerSendPermitted : Bool
  deriving Repr

private def request (interrupt : Option Time := none) : RequestContext :=
  { state := .pending
  , origin := .interactive
  , backend := ⟨"steering"⟩
  , admission := .released
  , deadline := 20
  , claimTime := 0
  , currentTime := 10
  , retryCount := 0
  , maxRetries := 3
  , messageSeq := 0
  , persistence := .uncommitted
  , interruptRequestedAt := interrupt }

private def base (interrupt : Option Time := none) : World :=
  { queue := ({ scope := { node := 1, session := 7, requester := some 2 }, active := none, pending := [], terminal := ∅ } : SessionQueue.SessionQueueState)
  , request := request interrupt
  , execution := none
  , input := { requestId := 11, requestDocId := 101, contentToken := 501 }
  , accepted := none
  , prepared := none
  , captured := none
  , lastSendAllowed := none }

private def entry : SessionQueue.QueueEntry :=
  { requestId := 11, createdAt := 10, source := .steering, policy := .append,
    queueKey := none, queuedAfter := some 10 }

private def activation : Handover.Activation :=
  { request :=
      { document := 101, entry, node := 1, session := 7,
        requester := some 2, authenticated := true }
    evidence := .ordinary,
    generation := 91, duration := 10, deadline := 20 }

private def budget : CompletionRetry.Budget :=
  { transportRetries := 0, resampleRetries := 0, allowRepair := false }

private def canonicalBefore (queue : SessionQueue.SessionQueueState) :
    CanonicalOutput.Execution.World :=
  { Examples.world 10 with
    sessionId := 7,
    transcript := { Examples.transcript with sessionId := 7 },
    lease := { RequestExecutionLease.initial Nat with now := 10 },
    queue }

private def begunOwner? (queued : World) : Option CanonicalOutput.Execution.World := do
  let gate ← Gate.acquire (Gate.initial (canonicalBefore queued.queue)) 1 true
  let claimed ← SessionComposition.activate gate 1 10 activation 0 budget (some 20)
  let released ← Gate.scheduling claimed 1 .release
  let held ← Gate.acquire released 1 true
  Handover.beginProcessing held 1 10 91

private def preparedInput : PreparedInput :=
  { closing :=
      { Examples.authored with
        coordinate := ⟨101, .authored 0⟩, writer := .request 91,
        createdAt := 10 }
    message :=
      { Examples.authoredMessage with
        header :=
          { Examples.authoredMessage.header with
            session := 7, request := some 101, publication := .requestExecution 91 },
        createdAt := 10 } }

private def capture (prior : Option RenderedCapture.CanonicalRequest := none) :
    RenderedCapture.Scenario :=
  { key := { nodeDid := 1, sessionId := 7, requestId := 101,
             turnIndex := 0, attempt := 0 },
    request := ⟨700⟩, priorBinding := prior }

private def runAction (script : Script) (w : World) : Action → Option World
  | .enqueue => enqueue? w script.entry
  | .claimWithoutBegin => claimWithoutBegin? w
  | .claimAndBegin => do
      let owner ← begunOwner? w
      claimAndBegin? w 91 owner
  | .latchInterrupt =>
      latchInterrupt? w (currentInterruptTarget w)
  | .terminate transition =>
      if w.execution.isSome then terminateOwnedBeforePublication? w 1 10 91 transition
      else terminateBeforeStart? w transition
  | .publish => publishAtExecutionStart? w 1 10 91 script.candidate
  | .prepareFails =>
      if script.candidate.isNone &&
          (publishAtExecutionStart? w 1 10 91 script.candidate).isNone then some w else none
  | .capture => some { w with captured := some script.capture }
  | .send =>
      let allowed := match script.candidate, w.captured with
        | some prepared, some attempt => providerSendPermitted w prepared attempt
        | _, _ => false
      some { w with lastSendAllowed := some allowed }

private def runScript (script : Script) : Option World :=
  script.actions.foldlM (runAction script)
    { base script.interruptAt with input := script.input }

private def observe (script : Script) : Option TraceObservation := do
  let w ← runScript script
  pure ⟨script.name, script, w.input.requestId, w.input.requestDocId,
    w.input.contentToken, w.request.state.toDefraDB,
    w.accepted == some w.input, w.queue.active, admissionVisible w,
    canonicalAuthoredCount w, w.lastSendAllowed.getD false⟩

private def fixture (name : String) (actions : List Action)
    (interruptAt : Option Time := none)
    (candidate : Option PreparedInput := some preparedInput)
    (attempt : RenderedCapture.Scenario := capture) : Script :=
  { name, input := (base none).input, entry, interruptAt,
    candidate, capture := attempt, actions }

def interruptedBeforeClaimObservation : Option TraceObservation :=
  observe (fixture "queued_steering_interrupted_before_claim_retains_admission_input_without_transcript"
    [.enqueue, .terminate .interruptBeforeClaim] (some 10))

def admissionRejectedObservation : Option TraceObservation :=
  observe (fixture "queued_steering_admission_rejected_retains_input_without_transcript"
    [.enqueue, .terminate .admissionReject])

def claimedFailureObservation : Option TraceObservation :=
  observe (fixture "queued_steering_claimed_failure_before_execution_retains_input_without_transcript"
    [.enqueue, .claimWithoutBegin, .terminate .failBeforeStream])

def beforePublicationObservation : Option TraceObservation :=
  observe (fixture "queued_steering_owned_execution_cannot_send_before_publication"
    [.enqueue, .claimAndBegin, .capture, .send])

def publicationHandoffObservation : Option TraceObservation :=
  observe (fixture "queued_steering_publishes_once_only_after_owned_execution_start"
    [.enqueue, .claimAndBegin, .publish, .capture, .send])

def preparationFailureObservation : Option TraceObservation :=
  observe (fixture "queued_steering_failed_preparation_cannot_publish_or_send"
    [.enqueue, .claimAndBegin, .prepareFails, .terminate .fail, .capture, .send] none none)

def interruptedDuringPreparationObservation : Option TraceObservation :=
  observe (fixture "queued_steering_interrupted_during_preparation_retains_input_without_transcript"
    [.enqueue, .claimAndBegin, .prepareFails, .latchInterrupt,
      .terminate .interruptProcessing, .capture, .send]
    none none)

def captureConflictObservation : Option TraceObservation :=
  observe (fixture "queued_steering_conflicting_capture_blocks_send"
    [.enqueue, .claimAndBegin, .publish, .capture, .send]
    none (some preparedInput) (capture (some ⟨701⟩)))

def publicationReplayObservation : Option TraceObservation :=
  observe (fixture "queued_steering_exact_publication_replay_has_one_authored_owner"
    [.enqueue, .claimAndBegin, .publish, .publish, .capture, .send])

structure GuardObservation where
  name : String
  prefixAdmitted : Bool
  admitted : Bool
  deriving DecidableEq, Repr

private def wrongHead : SessionQueue.QueueEntry :=
  { requestId := 12, createdAt := 9, source := .user, policy := .append,
    queueKey := none, queuedAfter := none }

private def scheduledWake (requestId : RequestId) (createdAt : Time) :
    SessionQueue.QueueEntry :=
  { requestId, createdAt, source := .backgroundCompletion, policy := .coalesce,
    queueKey := some 7, queuedAfter := some 11, origin := .scheduled }

private def queuedWithWake? : Option World := do
  let queued ← enqueue? (base none) entry
  let queue ← SessionQueue.step? queued.queue (.coalescePending (scheduledWake 31 10))
  pure { queued with queue }

private def firstWakeLatched? : Option World := do
  let queued ← queuedWithWake?
  latchInterrupt? queued (currentInterruptTarget queued)

private def repeatedWakeLatched? : Option World := do
  let first ← firstWakeLatched?
  let queue ← SessionQueue.step? first.queue (.coalescePending (scheduledWake 32 12))
  let later := { first with request := { first.request with currentTime := 12 }, queue }
  latchInterrupt? later (currentInterruptTarget later)

private def ownedWakeInterrupted? : Option World := do
  let queued ← queuedWithWake?
  let owner ← begunOwner? queued
  let begun ← claimAndBegin? queued 91 owner
  let latched ← latchInterrupt? begun (currentInterruptTarget begun)
  terminateOwnedBeforePublication? latched 1 10 91 .interruptProcessing

private def ownedWakeReplayInterrupted? : Option World := do
  let queued ← queuedWithWake?
  let owner ← begunOwner? queued
  let begun ← claimAndBegin? queued 91 owner
  let first ← latchInterrupt? begun (currentInterruptTarget begun)
  let execution ← first.execution
  if execution.queue != first.queue then none
  let queue ← SessionQueue.step? first.queue (.coalescePending (scheduledWake 32 12))
  let request := { first.request with currentTime := 12 }
  let joined := { execution with queue := queue }
  let later := { first with request := request, queue := queue, execution := some joined }
  let replayed ← latchInterrupt? later (currentInterruptTarget later)
  terminateOwnedBeforePublication? replayed 1 12 91 .interruptProcessing

private def firstLatched? : Option World := do
  let queued ← enqueue? (base none) entry
  latchInterrupt? queued (currentInterruptTarget queued)

private def repeatedLatched? : Option World := do
  let first ← firstLatched?
  let later := { first with request := { first.request with currentTime := 11 } }
  latchInterrupt? later (currentInterruptTarget later)

private def wrongInterruptTarget? : Option World := do
  let queued ← enqueue? (base none) entry
  latchInterrupt? queued
    { currentInterruptTarget queued with requestDocId := queued.input.requestDocId + 1 }

def wrongHeadObservation : GuardObservation :=
  let withWrongHead := { base none with queue := (base none).queue.appendPending wrongHead }
  let ready? := do
    let correctQueued ← enqueue? (base none) entry
    let owner ← begunOwner? correctQueued
    let wrongQueued ← enqueue? withWrongHead entry
    some (wrongQueued, owner)
  let admitted := (do
    let (wrongQueued, owner) ← ready?
    claimAndBegin? wrongQueued 91 owner).isSome
  { name := "queued_steering_cannot_claim_a_different_queue_head",
    prefixAdmitted := ready?.isSome, admitted }

def incoherentOwnerObservation : GuardObservation :=
  let ready? := do
    let queued ← enqueue? (base none) entry
    let owner ← begunOwner? queued
    some (queued, owner)
  let admitted := (do
    let (queued, owner) ← ready?
    claimAndBegin? queued 91 { owner with requestId := owner.requestId + 1 }).isSome
  { name := "queued_steering_rejects_incoherent_supplied_owner",
    prefixAdmitted := ready?.isSome, admitted }

def interruptedPublishObservation : GuardObservation :=
  let interrupted? := do
    let queued ← enqueue? (base (some 10)) entry
    interruptBeforeClaim? queued
  let admitted := do
    let interrupted ← interrupted?
    publishAtExecutionStart? interrupted 1 10 91 (some preparedInput)
  { name := "interrupted_queued_steering_cannot_publish",
    prefixAdmitted := interrupted?.isSome, admitted := admitted.isSome }

def guardObservations : List GuardObservation :=
  [wrongHeadObservation, incoherentOwnerObservation, interruptedPublishObservation]

def traceObservations : List TraceObservation :=
  match interruptedBeforeClaimObservation, admissionRejectedObservation,
      claimedFailureObservation, beforePublicationObservation, publicationHandoffObservation,
      preparationFailureObservation, interruptedDuringPreparationObservation,
      captureConflictObservation, publicationReplayObservation with
  | some interrupted, some rejected, some failed, some before, some published,
      some preparationFailed, some interruptedPreparation, some captureBlocked, some replayed =>
      [interrupted, rejected, failed, before, published, preparationFailed,
        interruptedPreparation, captureBlocked, replayed]
  | _, _, _, _, _, _, _, _, _ => []

theorem interrupted_trace_is_derived : interruptedBeforeClaimObservation.isSome = true := by native_decide
theorem publication_trace_is_derived : publicationHandoffObservation.isSome = true := by native_decide
theorem admission_rejection_trace_is_derived : admissionRejectedObservation.isSome = true := by native_decide
theorem claimed_failure_trace_is_derived : claimedFailureObservation.isSome = true := by native_decide
theorem before_publication_trace_is_derived : beforePublicationObservation.isSome = true := by native_decide
theorem preparation_failure_trace_is_derived : preparationFailureObservation.isSome = true := by native_decide
theorem interrupted_preparation_trace_is_derived :
    interruptedDuringPreparationObservation.isSome = true := by native_decide
theorem capture_conflict_trace_is_derived : captureConflictObservation.isSome = true := by native_decide
theorem publication_replay_trace_is_derived : publicationReplayObservation.isSome = true := by native_decide
theorem nine_traces_exported : traceObservations.length = 9 := by native_decide
theorem prepublication_send_is_rejected :
    (beforePublicationObservation.map (·.providerSendPermitted)) = some false := by native_decide
theorem published_send_is_permitted :
    (publicationHandoffObservation.map (·.providerSendPermitted)) = some true := by native_decide
theorem failed_preparation_blocks_send :
    (preparationFailureObservation.map (·.providerSendPermitted)) = some false := by native_decide
theorem failed_preparation_retains_input_after_terminal_commit :
    (preparationFailureObservation.map fun observation =>
      (observation.lifecycleState, observation.acceptedInput,
        observation.queueActive, observation.admissionVisible,
        observation.canonicalAuthoredCount)) = some ("failed", true, none, true, 0) := by native_decide
theorem interrupted_preparation_retains_input_after_terminal_commit :
    (interruptedDuringPreparationObservation.map fun observation =>
      (observation.lifecycleState, observation.acceptedInput,
        observation.queueActive, observation.admissionVisible,
        observation.canonicalAuthoredCount)) = some ("interrupted", true, none, true, 0) := by native_decide
theorem claimed_failure_finishes_queue_and_retains_input :
    (claimedFailureObservation.map fun observation =>
      (observation.lifecycleState, observation.acceptedInput,
        observation.queueActive, observation.admissionVisible,
        observation.canonicalAuthoredCount)) = some ("failed", true, none, true, 0) := by native_decide
theorem capture_conflict_blocks_send :
    (captureConflictObservation.map (·.providerSendPermitted)) = some false := by native_decide
theorem exact_replay_does_not_duplicate_authored_owner :
    (publicationReplayObservation.map (·.canonicalAuthoredCount)) = some 1 := by native_decide
theorem wrong_head_is_rejected : wrongHeadObservation.admitted = false := by native_decide
theorem incoherent_owner_is_rejected :
    incoherentOwnerObservation.admitted = false := by native_decide
theorem interrupted_publish_is_rejected : interruptedPublishObservation.admitted = false := by native_decide
theorem wrong_head_prefix_is_reachable : wrongHeadObservation.prefixAdmitted = true := by native_decide
theorem incoherent_owner_prefix_is_reachable :
    incoherentOwnerObservation.prefixAdmitted = true := by native_decide
theorem interrupted_publish_prefix_is_reachable :
    interruptedPublishObservation.prefixAdmitted = true := by native_decide
theorem first_interrupt_latch_stamps_selected_request :
    firstLatched?.map (·.request.interruptRequestedAt) = some (some 10) := by native_decide
theorem first_interrupt_drains_scheduled_wake_and_preserves_steering :
    (firstWakeLatched?.map fun world =>
      (world.queue.pending.map (·.requestId),
        decide (31 ∈ world.queue.terminal), world.request.interruptRequestedAt)) =
      some ([11], true, some 10) := by native_decide
theorem repeated_interrupt_preserves_newer_same_key_wake :
    (repeatedWakeLatched?.map fun world =>
      (world.queue.pending.map (·.requestId),
        decide (31 ∈ world.queue.terminal),
        decide (32 ∈ world.queue.terminal), world.request.interruptRequestedAt)) =
      some ([11, 32], true, false, some 10) := by native_decide
theorem in_flight_latch_drain_keeps_owned_terminal_handoff_reachable :
    (ownedWakeInterrupted?.map fun world =>
      (world.request.state, world.queue.active,
        decide (31 ∈ world.queue.terminal),
        decide (world.execution.map (·.queue) = some world.queue))) =
      some (.interrupted, none, true, true) := by native_decide
theorem in_flight_replay_retains_later_wake_through_owned_terminal_handoff :
    (ownedWakeReplayInterrupted?.map fun world =>
      (world.request.state, world.request.interruptRequestedAt, world.queue.active)) =
        some (.interrupted, some 10, none) ∧
    (ownedWakeReplayInterrupted?.map fun world =>
      (world.queue.pending.map (·.requestId),
        decide (31 ∈ world.queue.terminal), decide (32 ∈ world.queue.terminal))) =
        some ([32], true, false) ∧
    (ownedWakeReplayInterrupted?.map fun world =>
      decide (world.execution.map (·.queue) = some world.queue)) = some true := by
  constructor
  · native_decide
  constructor <;> native_decide
theorem repeated_interrupt_latch_preserves_original_timestamp :
    repeatedLatched?.map (·.request.interruptRequestedAt) = some (some 10) := by native_decide
theorem mismatched_interrupt_target_cannot_latch :
    wrongInterruptTarget?.isNone = true := by native_decide

end QueuedSteering
