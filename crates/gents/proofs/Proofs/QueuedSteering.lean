import Proofs.Session.Executable
import Proofs.Request.Executable
import Proofs.ClientShell.Projection
import Proofs.CanonicalOutput.Execution.Gate
import Proofs.CanonicalOutput.Execution.Handover
import Proofs.CanonicalOutput.Execution.SessionComposition
import Proofs.CanonicalOutput.Execution.Examples
import Proofs.RenderedCapture

/-! Composition of existing queue, request, canonical execution, client, and
provider-capture owners. Raw admission input is retained independently of the
fallible prepared message; this module does not render or template it. -/
namespace QueuedSteering

open CanonicalOutput
open CanonicalOutput.Execution

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

/-! A terminal decision before the owned execution starts retains the signed
admission input. It does not fabricate a transcript header. -/
def terminateBeforeStart? (w : World) (action : RequestContext.Action) : Option World := do
  if w.execution.isSome then none
  let request ← RequestContext.step? w.request action
  if !decide (isTerminal request.state) then none
  let queue ← if w.queue.active == some w.input.requestId then
      SessionQueue.step? w.queue .finishActive else some w.queue
  pure { w with request, queue }

/-! Once the owned execution has begun, a preparation failure or interruption
is a real terminal commit followed by the existing queue handoff. No authored
header is invented for the input accepted at admission. -/
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
  match hfinish : Handover.finishAndAcknowledge finishHeld actor with
  | none => none
  | some finished =>
      if finished.outcome != outcome then none
      else some ⟨request, finished.state, outcome,
        (Handover.successful_finish_clears_claim_control finishHeld finished actor hfinish).2⟩

def terminateOwnedBeforePublication? (w : World) (actor now generation : Nat)
    (action : RequestContext.Action) : Option World := do
  let handoff ← ownedTerminalHandoff? w actor now generation action
  -- Queue clearance is proved by Handover. The two lease/request equalities are
  -- explicit coherence checks at this composition boundary, not new transitions.
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

/-! The canonical execution owner is supplied by the claim/begin handoff. It
must be the same physical request, logical queue head, and processing lease;
this check does not implement a second lease transition. -/
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

/-! Preparation is a fallible existing runtime boundary. Its selected message
may be templated or wrapped; raw admission text is not equated with it. -/
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

/-! This is the authored-input and capture fence, not a general provider-lease
authorization or proof that opaque serialized bytes encode `prepared`. A resumed
attempt may reuse its exact canonical publication without creating a new row.
`RenderedCapture` separately fences the opaque serialized request bytes. -/
def providerSendPermitted (w : World) (prepared : PreparedInput)
    (capture : RenderedCapture.Scenario) : Bool :=
  (w.request.state == .processing) &&
    (w.prepared == some prepared) && hasExactAuthoredOwner w &&
    (match w.execution with
      | none => false
      | some execution =>
          capture.key.requestId == w.input.requestDocId &&
            capture.key.sessionId == execution.sessionId &&
            capture.key.agentDid == execution.principal &&
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
  { queue := ({ scope := { agent := 1, session := 7, requester := some 2 }, active := none, pending := [], terminal := ∅ } : SessionQueue.SessionQueueState)
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
      { document := 101, entry, agent := 1, session := 7,
        requester := some 2, authenticated := true }
    evidence := .ordinary, configuredRoutes := [], routesAuthenticated := true,
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

/-! The case's owner result is produced by the existing claim and begin steps,
not by constructing a processing lease in the steering model. -/
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
  { key := { agentDid := 1, sessionId := 7, requestId := 101,
             turnIndex := 0, attempt := 0 },
    request := ⟨700⟩, priorBinding := prior }

private def runAction (script : Script) (w : World) : Action → Option World
  | .enqueue => enqueue? w script.entry
  | .claimWithoutBegin => claimWithoutBegin? w
  | .claimAndBegin => do
      let owner ← begunOwner? w
      claimAndBegin? w 91 owner
  -- An external durable interrupt intent becomes observable at this point.
  -- RequestContext owns the subsequent legal terminal transition.
  | .latchInterrupt =>
      some { w with request := { w.request with interruptRequestedAt := some w.request.currentTime } }
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

def wrongHeadObservation : GuardObservation :=
  let withWrongHead := { base none with queue := (base none).queue.appendPending wrongHead }
  let queued? := enqueue? withWrongHead entry
  let admitted := (do
    let queued ← queued?
    let owner ← begunOwner? queued
    claimAndBegin? queued 91 owner).isSome
  { name := "queued_steering_cannot_claim_a_different_queue_head",
    prefixAdmitted := queued?.isSome, admitted }

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
  [wrongHeadObservation, interruptedPublishObservation]

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
theorem interrupted_publish_is_rejected : interruptedPublishObservation.admitted = false := by native_decide
theorem wrong_head_prefix_is_reachable : wrongHeadObservation.prefixAdmitted = true := by native_decide
theorem interrupted_publish_prefix_is_reachable :
    interruptedPublishObservation.prefixAdmitted = true := by native_decide

end QueuedSteering
