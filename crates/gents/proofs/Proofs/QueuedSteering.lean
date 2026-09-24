import Proofs.Session.Executable
import Proofs.Request.Executable
import Proofs.RequestExecutionLease.Transition
import Proofs.ClientShell.Projection

/-! Composition of existing queue, request, execution-lease, and client owners.
`AuthoredPublication` is only the exact physical binding bridged here; canonical
message construction remains owned by CanonicalOutput. -/
namespace QueuedSteering

structure AdmissionInput where
  requestId : RequestId
  requestDocId : Nat
  contentToken : Nat
  deriving DecidableEq, Repr

structure AuthoredPublication where
  requestDocId : Nat
  contentToken : Nat
  deriving DecidableEq, Repr

structure World where
  queue : SessionQueue.SessionQueueState
  request : RequestContext
  execution : Option (RequestExecutionLease.World Nat)
  input : AdmissionInput
  accepted : Option AdmissionInput
  authored : Option AuthoredPublication
  deriving Repr

def hasExactAuthoredOwner (w : World) : Bool :=
  w.authored == some { requestDocId := w.input.requestDocId, contentToken := w.input.contentToken }

def admissionVisible (w : World) : Bool := projectPendingUserTurn (hasExactAuthoredOwner w)

def enqueue? (w : World) (entry : SessionQueue.QueueEntry) : Option World :=
  if entry.requestId != w.input.requestId || w.request.state != .pending then none
  else match SessionQueue.step? w.queue (.appendPending entry) with
    | none => none
    | some queue => some { w with queue, accepted := some w.input }

def interruptBeforeClaim? (w : World) : Option World := do
  if w.execution.isSome then none
  let request ← RequestContext.step? w.request .interruptBeforeClaim
  pure { w with request }

def claimAndBegin? (w : World) (generation : Nat) : Option World := do
  if w.execution.isSome then none
  let queue ← SessionQueue.step? w.queue .claimNext
  if queue.active != some w.input.requestId then none
  let requestClaimed ← RequestContext.step? w.request .claim
  let requestAcquired := { requestClaimed with admission := .acquired }
  let request ← RequestContext.step? requestAcquired .beginInference
  let lease := { RequestExecutionLease.initial Nat with now := w.request.currentTime }
  let claimed ← RequestExecutionLease.step? lease
    (.claim .mutationWriteGate generation 10 20)
  let execution ← RequestExecutionLease.step? claimed (.begin .mutationWriteGate generation)
  if execution.request != request.state then none
  pure { w with queue, request, execution := some execution }

def publishAtExecutionStart? (w : World) (generation requestDocId contentToken : Nat) : Option World := do
  if requestDocId != w.input.requestDocId || contentToken != w.input.contentToken then none
  if w.request.state != .processing then none
  let current ← w.execution
  if current.request != w.request.state then none
  let execution ← RequestExecutionLease.step? current
    (.authorizeProducerDecision .mutationWriteGate generation .acceptAndPublish)
  pure { w with execution := some execution, authored := some { requestDocId, contentToken } }

theorem interrupted_before_claim_retains_exact_input
    {before queued interrupted : World} {entry : SessionQueue.QueueEntry}
    (ha : before.authored = none)
    (hx : before.execution = none)
    (he : enqueue? before entry = some queued)
    (hi : interruptBeforeClaim? queued = some interrupted) :
    interrupted.request.state = .interrupted ∧ interrupted.accepted = some before.input ∧
      interrupted.authored = none ∧ admissionVisible interrupted = true := by
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
      simp [hstate, ha, admissionVisible, hasExactAuthoredOwner, projectPendingUserTurn]

theorem publication_handoff_preserves_exact_input
    {before after : World} {generation requestDocId contentToken : Nat}
    (hr : requestDocId = before.input.requestDocId)
    (hc : contentToken = before.input.contentToken)
    (h : publishAtExecutionStart? before generation requestDocId contentToken = some after) :
    after.accepted = before.accepted ∧ after.input = before.input ∧
      after.authored = some ({ requestDocId := before.input.requestDocId, contentToken := before.input.contentToken } : AuthoredPublication) ∧
      admissionVisible after = false := by
  subst requestDocId
  subst contentToken
  unfold publishAtExecutionStart? at h
  simp at h
  rcases h with ⟨_, h⟩
  generalize ho : before.execution = owner at h
  cases owner with
  | none => simp at h
  | some current =>
    simp at h
    rcases h with ⟨hcoherent, h⟩
    generalize hx : RequestExecutionLease.step? current
      (.authorizeProducerDecision .mutationWriteGate generation .acceptAndPublish) = result at h
    cases result with
    | none => simp at h
    | some execution =>
      simp at h
      cases h
      simp [admissionVisible, hasExactAuthoredOwner, projectPendingUserTurn]

structure TraceObservation where
  name : String
  requestId : Nat
  requestDocId : Nat
  contentToken : Nat
  lifecycleState : String
  admissionVisible : Bool
  canonicalAuthoredCount : Nat
  deriving DecidableEq, Repr

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
  , authored := none }

private def entry : SessionQueue.QueueEntry :=
  { requestId := 11, createdAt := 10, source := .steering, policy := .append,
    queueKey := none, queuedAfter := some 10 }

def interruptedBeforeClaimObservation : Option TraceObservation := do
  let queued ← enqueue? (base (some 10)) entry
  let done ← interruptBeforeClaim? queued
  pure ({ name := "queued_steering_interrupted_before_claim_retains_admission_input_without_transcript", requestId := done.input.requestId, requestDocId := done.input.requestDocId, contentToken := done.input.contentToken, lifecycleState := done.request.state.toDefraDB, admissionVisible := admissionVisible done, canonicalAuthoredCount := done.authored.toList.length } : TraceObservation)

def publicationHandoffObservation : Option TraceObservation := do
  let queued ← enqueue? (base none) entry
  let running ← claimAndBegin? queued 91
  let done ← publishAtExecutionStart? running 91 101 501
  pure ({ name := "queued_steering_publishes_once_only_after_owned_execution_start", requestId := done.input.requestId, requestDocId := done.input.requestDocId, contentToken := done.input.contentToken, lifecycleState := done.request.state.toDefraDB, admissionVisible := admissionVisible done, canonicalAuthoredCount := done.authored.toList.length } : TraceObservation)

structure GuardObservation where
  name : String
  admitted : Bool
  deriving DecidableEq, Repr

private def wrongHead : SessionQueue.QueueEntry :=
  { requestId := 12, createdAt := 9, source := .user, policy := .append,
    queueKey := none, queuedAfter := none }

def wrongHeadObservation : GuardObservation :=
  let withWrongHead := { base none with queue := (base none).queue.appendPending wrongHead }
  let admitted := (enqueue? withWrongHead entry).bind (claimAndBegin? · 91) |>.isSome
  { name := "queued_steering_cannot_claim_a_different_queue_head", admitted }

def interruptedPublishObservation : GuardObservation :=
  let admitted := do
    let queued ← enqueue? (base (some 10)) entry
    let interrupted ← interruptBeforeClaim? queued
    publishAtExecutionStart? interrupted 91 101 501
  { name := "interrupted_queued_steering_cannot_publish", admitted := admitted.isSome }

def guardObservations : List GuardObservation :=
  [wrongHeadObservation, interruptedPublishObservation]

def traceObservations : List TraceObservation :=
  match interruptedBeforeClaimObservation, publicationHandoffObservation with
  | some interrupted, some published => [interrupted, published]
  | _, _ => []

theorem interrupted_trace_is_derived : interruptedBeforeClaimObservation.isSome = true := by native_decide
theorem publication_trace_is_derived : publicationHandoffObservation.isSome = true := by native_decide
theorem both_traces_exported : traceObservations.length = 2 := by native_decide
theorem wrong_head_is_rejected : wrongHeadObservation.admitted = false := by native_decide
theorem interrupted_publish_is_rejected : interruptedPublishObservation.admitted = false := by native_decide

end QueuedSteering
