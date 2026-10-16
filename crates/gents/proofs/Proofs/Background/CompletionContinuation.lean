import Proofs.Session.Properties.Executable
import Proofs.AgentSession
import Proofs.Request
import Proofs.Goals
import Proofs.ToolExecution.State
import Proofs.Transcript.State

/-!
# Background Completion Continuation

The bridge and tool-call models terminalize work, the transcript model appends
messages, and the session model coalesces and claims wake requests: (1) an
assistant wait header and pending call are durably published before dispatch;
(2) only a terminal parent-visible tool may produce a completion notification;
(3) the notification is appended after the reserved assistant row; (4) a
coalesced wake is enqueued only after that append exists, whether or not the
session has a Goal; (5) a claimed wake still carries the transcript containing
both rows in the correct order.
-/

namespace BackgroundCompletion

open ToolExecution

structure TerminalCompletion where
  toolState : ToolCallState
  notificationMessageId : Transcript.MessageId
  wake : SessionQueue.QueueEntry
  deriving Repr

structure NotifiedCompletion where
  completion : TerminalCompletion
  preTranscript : Transcript.TranscriptState
  transcript : Transcript.TranscriptState
  terminal : isTerminal completion.toolState
  /-- Recovery may observe the exact immutable row in a since-advanced
  transcript; then `freshAppend` is false and `durable` is authoritative. -/
  freshAppend : Bool
  appended : freshAppend = true →
    transcript = preTranscript.appendUserMessage
      completion.notificationMessageId Transcript.MessageKind.ordinary
  durable : ∃ row,
    row ∈ transcript.messages ∧
    row.messageId = completion.notificationMessageId ∧
    row.sessionId = transcript.sessionId ∧
    row.role = Transcript.MessageRole.user ∧
    row.kind = Transcript.MessageKind.ordinary

def appendNotification?
    (completion : TerminalCompletion)
    (pre : Transcript.TranscriptState) :
    Option NotifiedCompletion :=
  if h_terminal : isTerminal completion.toolState then
    some
      { completion := completion
      , preTranscript := pre
      , transcript :=
          pre.appendUserMessage
            completion.notificationMessageId
            Transcript.MessageKind.ordinary
      , terminal := h_terminal
      , freshAppend := true
      , appended := fun _ => rfl
      , durable := by
          refine ⟨{ messageId := completion.notificationMessageId
                    , sessionId := pre.sessionId
                    , sequence := pre.nextSeq
                    , role := .user
                    , kind := .ordinary }, ?_, rfl, rfl, rfl, rfl⟩
          simp [Transcript.TranscriptState.appendUserMessage]
      }
  else
    none

structure QueuedCompletion where
  notified : NotifiedCompletion
  preQueue : SessionQueue.SessionQueueState
  queue : SessionQueue.SessionQueueState
  sameSession : preQueue.sessionId = notified.transcript.sessionId
  wakeWellFormed :
    notified.completion.wake.source = .backgroundCompletion ∧
    notified.completion.wake.coalesceWellFormed preQueue.sessionId
  wakeKeyMissing :
    SessionQueue.containsCoalescedQueueKey
      preQueue.pending
      SessionQueue.QueueSource.backgroundCompletion
      preQueue.sessionId = false
  enqueued : SessionQueue.step? preQueue
    (.coalescePending notified.completion.wake) = some queue

/-- Goals and background wakes are independent (wake ownership decision):
no Goal suppresses the background-completion wake. -/
def enqueueWake?
    (notified : NotifiedCompletion)
    (pre : SessionQueue.SessionQueueState) :
    Option QueuedCompletion :=
  if h_session : pre.sessionId = notified.transcript.sessionId then
    if h_well_formed :
        notified.completion.wake.source = .backgroundCompletion ∧
        notified.completion.wake.coalesceWellFormed pre.sessionId then
      if h_missing :
          SessionQueue.containsCoalescedQueueKey
            pre.pending
            SessionQueue.QueueSource.backgroundCompletion
            pre.sessionId = false then
        match
            h_step :
              SessionQueue.step? pre
                (.coalescePending notified.completion.wake) with
        | none => none
        | some post =>
            some
              { notified := notified
              , preQueue := pre
              , queue := post
              , sameSession := h_session
              , wakeWellFormed := h_well_formed
              , wakeKeyMissing := h_missing
              , enqueued := h_step
              }
      else
        none
    else
      none
  else
    none

theorem goal_queue_cannot_be_background_wake
    (notified : NotifiedCompletion) (pre : SessionQueue.SessionQueueState)
    (h : notified.completion.wake.source = .goal) : enqueueWake? notified pre = none := by
  simp [enqueueWake?, h]

structure Continuation where
  queued : QueuedCompletion
  queue : SessionQueue.SessionQueueState
  claimed : SessionQueue.step? queued.queue .claimNext = some queue
  activeWake :
    queue.active = some queued.notified.completion.wake.requestId

def claimContinuation? (queued : QueuedCompletion) : Option Continuation :=
  match h_step : SessionQueue.step? queued.queue .claimNext with
  | none => none
  | some post =>
      if h_active :
          post.active = some queued.notified.completion.wake.requestId then
        some
          { queued := queued
          , queue := post
          , claimed := h_step
          , activeWake := h_active
          }
      else
        none

def HasNotification (notified : NotifiedCompletion) : Prop :=
  ∃ row,
    row ∈ notified.transcript.messages ∧
    row.messageId = notified.completion.notificationMessageId ∧
    row.sessionId = notified.transcript.sessionId ∧
    row.role = Transcript.MessageRole.user ∧
    row.kind = Transcript.MessageKind.ordinary

theorem notified_completion_has_durable_message
    (notified : NotifiedCompletion) :
    HasNotification notified := by
  exact notified.durable

theorem claimed_continuation_sees_terminal_notification
    (continuation : Continuation) :
    isTerminal continuation.queued.notified.completion.toolState ∧
    HasNotification continuation.queued.notified ∧
    continuation.queue.active =
      some continuation.queued.notified.completion.wake.requestId :=
  ⟨continuation.queued.notified.terminal,
    notified_completion_has_durable_message continuation.queued.notified,
    continuation.activeWake⟩

theorem claimed_continuation_retains_publication_enqueue_claim_chain
    (continuation : Continuation) :
    isTerminal continuation.queued.notified.completion.toolState ∧
      HasNotification continuation.queued.notified ∧
      continuation.queued.preQueue.sessionId =
        continuation.queued.notified.transcript.sessionId ∧
      (continuation.queued.notified.completion.wake.source = .backgroundCompletion ∧
        continuation.queued.notified.completion.wake.coalesceWellFormed
          continuation.queued.preQueue.sessionId) ∧
      SessionQueue.containsCoalescedQueueKey
        continuation.queued.preQueue.pending
        SessionQueue.QueueSource.backgroundCompletion
        continuation.queued.preQueue.sessionId = false ∧
      SessionQueue.step? continuation.queued.preQueue
        (.coalescePending continuation.queued.notified.completion.wake) =
          some continuation.queued.queue ∧
      SessionQueue.step? continuation.queued.queue .claimNext =
        some continuation.queue ∧
      continuation.queue.active =
        some continuation.queued.notified.completion.wake.requestId ∧
      (continuation.queued.notified.freshAppend = true →
        continuation.queued.notified.transcript =
          continuation.queued.notified.preTranscript.appendUserMessage
            continuation.queued.notified.completion.notificationMessageId
            Transcript.MessageKind.ordinary) := by
  exact ⟨continuation.queued.notified.terminal,
    continuation.queued.notified.durable,
    continuation.queued.sameSession,
    continuation.queued.wakeWellFormed,
    continuation.queued.wakeKeyMissing,
    continuation.queued.enqueued,
    continuation.claimed,
    continuation.activeWake,
    continuation.queued.notified.appended⟩

/-! ## Executable canonical acceptance witness -/

def canonicalTranscript : Transcript.TranscriptState :=
  { sessionId := 900
  , nextSeq := 3
  , messages := []
  , toolCalls := []
  , inFlight := ∅
  }

/-- The model-visible wait call is persisted before execution blocks: an
independently appended completion notification observes `nextSeq = 4`, not the
assistant row's sequence 3. -/
def canonicalWaitTurn : Transcript.AssistantTurn :=
  { sessionId := canonicalTranscript.sessionId
  , sequence := canonicalTranscript.nextSeq
  , callIds := [51]
  }

def canonicalWaitReservedTranscript : Transcript.TranscriptState :=
  let published := canonicalTranscript.publishAcceptedAssistant 40 canonicalWaitTurn
  published.dispatchToolCall 51

def canonicalWake : SessionQueue.QueueEntry :=
  { requestId := 901
  , createdAt := 10
  , source := .backgroundCompletion
  , policy := .coalesce
  , queueKey := some 900
  , queuedAfter := some 899
  , origin := .scheduled
  }

def canonicalQueue : SessionQueue.SessionQueueState :=
  { scope := ⟨1, 900, none⟩
  , active := none
  , pending := []
  , terminal := ∅
  }

def canonicalCompletion : TerminalCompletion :=
  { toolState := .completed
  , notificationMessageId := 41
  , wake := canonicalWake
  }

/-- Executable four-stage acceptance result used by generated conformance cases. -/
def canonicalContinuationAccepted : Bool :=
  match appendNotification? canonicalCompletion canonicalWaitReservedTranscript with
  | none => false
  | some notified =>
      match enqueueWake? notified canonicalQueue with
      | none => false
      | some queued =>
          match claimContinuation? queued with
          | none => false
          | some continuation =>
              decide
                (continuation.queue.active = some canonicalWake.requestId ∧
                 notified.transcript.messages.length =
                   canonicalTranscript.messages.length + 2 ∧
                 notified.transcript.messages.map
                   (fun row => (row.sequence, row.role)) =
                   [(3, .assistant), (4, .user)])

theorem canonical_completion_appends_then_enqueues_then_continues :
    canonicalContinuationAccepted = true := by
  native_decide

/-! ## Bounded failed-wake redrive

The daemon may create one fresh continuation attempt for a failed scheduled
continuation, but only for the canonical coalesced background queue shape and
only while the persisted retry budget remains open; this is separate from the
interactive client retry modeled by `SessionRecovery`. -/

structure WakeRequest where
  state : RequestState
  origin : ExecutionOrigin
  admission : AdmissionState
  retryCount : Nat
  maxRetries : Nat
  depth : Nat
  parentRequestId : Option RequestId
  deadline : Option Time

structure FailedWake where
  requestId : RequestId
  ctx : WakeRequest
  source : SessionQueue.QueueSource
  policy : SessionQueue.QueuePolicy
  queueKey : Option SessionId

/-- Local lifecycle/budget/queue eligibility only. -/
def CanRedriveWake (wake : FailedWake) : Prop :=
  wake.ctx.state = .failed ∧
    wake.ctx.admission = .released ∧
    wake.ctx.origin = .scheduled ∧
    wake.ctx.retryCount < wake.ctx.maxRetries ∧
    wake.source = .backgroundCompletion ∧
    wake.policy = .coalesce ∧
    wake.queueKey.isSome

instance (wake : FailedWake) : Decidable (CanRedriveWake wake) := by
  unfold CanRedriveWake
  infer_instance

def redrivenWakeContext (wake : FailedWake) : WakeRequest :=
  { wake.ctx with
    state := .pending
    origin := .scheduled
    admission := .released
    retryCount := wake.ctx.retryCount + 1
    parentRequestId := some wake.requestId
    deadline := none }

def redriveWake? (wake : FailedWake) : Option WakeRequest :=
  if _h : CanRedriveWake wake then
    some (redrivenWakeContext wake)
  else
    none

/-- One publication transaction's result, not another durable request/session;
the DB adapter must publish the outputs atomically. -/
def redriveWakeFromRows?
    (session : AgentSession.Document)
    (rows : List AgentSession.RequestFact) (parentDoc : Nat) (wake : FailedWake)
    (successor : AgentSession.RequestFact) (preview : String) (now : Time) :
    Option (WakeRequest × AgentSession.Document × List AgentSession.RequestFact) := do
  let parent ← AgentSession.latest rows session.scope.node session.scope.session none
  let next ← redriveWake? wake
  let nextRows := rows ++ [successor]
  if parent.observed.docId = parentDoc ∧ parent.observed.requestId = wake.requestId ∧
      parent.observed.state = wake.ctx.state ∧ parent.scope = session.scope ∧
      parent.agent = session.agent ∧ wake.queueKey = some session.scope.session ∧
      successor.scope = session.scope ∧ successor.agent = session.agent ∧
      successor.observed.state = .pending ∧
      rows.all (fun row => row.observed.docId != successor.observed.docId &&
        row.observed.requestId != successor.observed.requestId) ∧
      AgentSession.newer successor parent ∧
      AgentSession.latest nextRows session.scope.node session.scope.session none = some successor then
    some (next, AgentSession.advance session nextRows successor preview now, nextRows)
  else none

theorem redrive_publication_has_one_successor
    (session : AgentSession.Document)
    (rows : List AgentSession.RequestFact) (parentDoc : Nat) (wake : FailedWake)
    (successor : AgentSession.RequestFact) (preview : String) (now : Time)
    (post : WakeRequest × AgentSession.Document × List AgentSession.RequestFact)
    (h : redriveWakeFromRows? session rows parentDoc wake successor preview now = some post) :
    post.2.2 = rows ++ [successor] ∧
    AgentSession.latest post.2.2 session.scope.node session.scope.session none = some successor ∧
    post.2.1.observation.bind (·.latest) = some successor.observed ∧
    successor.observed.state = .pending := by
  cases hp : AgentSession.latest rows session.scope.node session.scope.session none <;>
    cases hn : redriveWake? wake <;>
    simp [redriveWakeFromRows?, hp, hn] at h
  obtain ⟨guard, rfl⟩ := h
  obtain ⟨_, _, _, _, _, _, hs, hb, hstate, _, _, hhead⟩ := guard
  have hscoped := AgentSession.latest_exact_of_session_winner
    (rows ++ [successor]) session.scope.node session.scope.session successor hhead
  simp only [hs] at hscoped
  simp [AgentSession.advance, hs, hb, hhead, hscoped, hstate]

/-- Cached presentation is deliberately absent from every redrive gate. -/
theorem redrive_selection_ignores_cached_head
    (session : AgentSession.Document)
    (rows : List AgentSession.RequestFact) (parentDoc : Nat) (wake : FailedWake)
    (successor : AgentSession.RequestFact) (preview : String) (now : Time)
    (observation : Option AgentSession.Observation) :
    (redriveWakeFromRows? { session with observation } rows parentDoc wake successor preview now).isSome =
    (redriveWakeFromRows? session rows parentDoc wake successor preview now).isSome := by
  cases hp : AgentSession.latest rows session.scope.node session.scope.session none <;>
    cases hn : redriveWake? wake <;>
    simp [redriveWakeFromRows?, hp, hn]
  split <;> simp_all

theorem redriveWake?_bounded
    {wake : FailedWake}
    {post : WakeRequest}
    (h_redrive : redriveWake? wake = some post) :
    post.state = .pending ∧
      post.origin = .scheduled ∧
      post.retryCount = wake.ctx.retryCount + 1 ∧
      post.retryCount ≤ post.maxRetries ∧
      post.depth = wake.ctx.depth ∧
      post.parentRequestId = some wake.requestId ∧ post.deadline = none := by
  simp [redriveWake?] at h_redrive
  rcases h_redrive with ⟨h_can, rfl⟩
  rcases h_can with ⟨_, _, _, h_budget, _, _, _⟩
  simp [redrivenWakeContext, Nat.succ_le_of_lt h_budget]

def failedWakeFixture
    (state : RequestState := .failed)
    (origin : ExecutionOrigin := .scheduled)
    (retryCount : Nat := 0)
    (maxRetries : Nat := 3)
    (source : SessionQueue.QueueSource := .backgroundCompletion)
    (policy : SessionQueue.QueuePolicy := .coalesce)
    (queueKey : Option SessionId := some 900) : FailedWake :=
  { requestId := 901
  , ctx :=
      { state := state
      , origin := origin
      , admission := .released
      , deadline := some 1
      , depth := 2
      , parentRequestId := some 899
      , retryCount := retryCount
      , maxRetries := maxRetries
      }
  , source := source
  , policy := policy
  , queueKey := queueKey
  }

def canonicalFailedWakeRedriveAccepted : Bool :=
  match redriveWake? (failedWakeFixture (retryCount := 1)) with
  | none => false
  | some post =>
      decide
        (post.state = .pending ∧
         post.origin = .scheduled ∧
         post.retryCount = 2 ∧
         post.maxRetries = 3 ∧ post.depth = 2 ∧
         post.parentRequestId = some 901 ∧ post.deadline = none)

theorem canonical_failed_wake_redrive_is_bounded :
    canonicalFailedWakeRedriveAccepted = true := by
  native_decide

def wakeRetryBaseSeconds : Nat := 5
def wakeRetryMaxSeconds : Nat := 60

def wakeRetryDelaySeconds (retryCount : Nat) : Nat :=
  min wakeRetryMaxSeconds (wakeRetryBaseSeconds * 2 ^ retryCount)

theorem wake_retry_delay_positive (retryCount : Nat) :
    0 < wakeRetryDelaySeconds retryCount := by
  have hpow : 0 < 2 ^ retryCount :=
    Nat.pow_pos_iff.mpr (Or.inl (by decide))
  simp [wakeRetryDelaySeconds, wakeRetryMaxSeconds, wakeRetryBaseSeconds, hpow]

theorem wake_retry_delay_bounded (retryCount : Nat) :
    wakeRetryDelaySeconds retryCount ≤ wakeRetryMaxSeconds := by
  simp [wakeRetryDelaySeconds]

def canonicalWakeRetryDelayAccepted : Bool :=
  decide
    (wakeRetryDelaySeconds 0 = 5 ∧
     wakeRetryDelaySeconds 1 = 10 ∧
     wakeRetryDelaySeconds 4 = 60 ∧
     wakeRetryDelaySeconds 20 = 60)

theorem canonical_wake_retry_backoff_is_bounded :
    canonicalWakeRetryDelayAccepted = true := by
  native_decide

/-! ## Aged wake admission

Aged background-completion wakes precede ordinary descendant work at the
bounded agent-executor queue: the runtime's weak-fairness assumption made
executable. -/

def completionWakeAgingThresholdSeconds : Nat := 30

structure AdmissionCandidate where
  requestId : RequestId
  ageSeconds : Nat
  source : SessionQueue.QueueSource
  deriving DecidableEq, Repr

def AdmissionCandidate.isAgedCompletionWake
    (candidate : AdmissionCandidate) : Bool :=
  candidate.source = .backgroundCompletion &&
    completionWakeAgingThresholdSeconds ≤ candidate.ageSeconds

def admissionPriority (candidate : AdmissionCandidate) : Nat :=
  if candidate.isAgedCompletionWake then 0 else 1

def servesBefore
    (left right : AdmissionCandidate) : Bool :=
  admissionPriority left < admissionPriority right

theorem aged_completion_wake_precedes_descendant
    (wake descendant : AdmissionCandidate)
    (h_wake_source : wake.source = .backgroundCompletion)
    (h_wake_age : completionWakeAgingThresholdSeconds ≤ wake.ageSeconds)
    (h_descendant_source : descendant.source ≠ .backgroundCompletion) :
    servesBefore wake descendant = true := by
  simp [servesBefore, admissionPriority,
    AdmissionCandidate.isAgedCompletionWake, h_wake_source, h_wake_age,
    h_descendant_source]

theorem fresh_completion_wake_preserves_fifo_priority
    (wake : AdmissionCandidate)
    (h_wake_source : wake.source = .backgroundCompletion)
    (h_wake_age : wake.ageSeconds < completionWakeAgingThresholdSeconds) :
    admissionPriority wake = 1 := by
  simp [admissionPriority, AdmissionCandidate.isAgedCompletionWake,
    h_wake_source, Nat.not_le.mpr h_wake_age]

def agedWakeFixture : AdmissionCandidate :=
  { requestId := 901
  , ageSeconds := completionWakeAgingThresholdSeconds
  , source := .backgroundCompletion
  }

def descendantFixture : AdmissionCandidate :=
  { requestId := 902
  , ageSeconds := 0
  , source := .user
  }

def freshWakeFixture : AdmissionCandidate :=
  { requestId := 903
  , ageSeconds := completionWakeAgingThresholdSeconds - 1
  , source := .backgroundCompletion
  }

theorem canonical_aged_wake_admission_accepted :
    servesBefore agedWakeFixture descendantFixture = true := by
  native_decide

theorem canonical_fresh_wake_does_not_bypass_fifo :
    servesBefore freshWakeFixture descendantFixture = false := by
  native_decide

/-! ## Attempt snapshots and acknowledgement

Claim snapshots the transcript's last sequence in the same transaction that
claims the wake; a later notification cannot enter the active attempt's
provider input. -/
structure NotificationBinding where
  messageId : Transcript.MessageId
  sequence : Nat
  wakeRequestId : RequestId
  deriving DecidableEq, Repr

structure WakeAttemptSnapshot where
  wakeRequestId : RequestId
  throughSequence : Nat
  bindings : List NotificationBinding
  deriving DecidableEq, Repr

def WakeAttemptSnapshot.attemptedBindings
    (snapshot : WakeAttemptSnapshot) : List NotificationBinding :=
  snapshot.bindings.filter fun binding =>
    binding.wakeRequestId = snapshot.wakeRequestId &&
      binding.sequence ≤ snapshot.throughSequence

def WakeAttemptSnapshot.acknowledgedBindings
    (snapshot : WakeAttemptSnapshot) (terminalState : RequestState) : List NotificationBinding :=
  if terminalState = .completed then snapshot.attemptedBindings else []

theorem successor_binding_after_cutoff_not_attempted
    (snapshot : WakeAttemptSnapshot)
    (binding : NotificationBinding)
    (h_after : snapshot.throughSequence < binding.sequence) :
    binding ∉ snapshot.attemptedBindings := by
  simp [WakeAttemptSnapshot.attemptedBindings, Nat.not_le.mpr h_after]

theorem completed_attempt_acknowledges_exact_snapshot
    (snapshot : WakeAttemptSnapshot) (terminalState : RequestState)
    (h_completed : terminalState = .completed) :
    snapshot.acknowledgedBindings terminalState = snapshot.attemptedBindings := by
  simp [WakeAttemptSnapshot.acknowledgedBindings, h_completed]

theorem failed_attempt_acknowledges_nothing
    (snapshot : WakeAttemptSnapshot) (terminalState : RequestState)
    (h_failed : terminalState = .failed) :
    snapshot.acknowledgedBindings terminalState = [] := by
  simp [WakeAttemptSnapshot.acknowledgedBindings, h_failed]

def attemptedBindingFixture : NotificationBinding :=
  { messageId := 41, sequence := 4, wakeRequestId := 901 }

def successorBindingFixture : NotificationBinding :=
  { messageId := 42, sequence := 6, wakeRequestId := 902 }

def canonicalSnapshotFixture : WakeAttemptSnapshot :=
  { wakeRequestId := 901
  , throughSequence := 5
  , bindings := [attemptedBindingFixture, successorBindingFixture]
  }

theorem canonical_completed_snapshot_acknowledges_owned_notification :
    canonicalSnapshotFixture.acknowledgedBindings .completed = [attemptedBindingFixture] := by
  native_decide

theorem canonical_successor_notification_excluded_from_active_snapshot :
    successorBindingFixture ∉ canonicalSnapshotFixture.attemptedBindings := by
  native_decide

theorem canonical_failed_snapshot_retains_unacknowledged_notification :
    canonicalSnapshotFixture.acknowledgedBindings .failed = [] ∧
      canonicalSnapshotFixture.attemptedBindings = [attemptedBindingFixture] := by
  native_decide

/-! ## Crash-boundary recovery

Acknowledgement is a projection of the durable claim snapshot and the
authoritative request terminal state; a crash while a reader projects
acknowledgement cannot create a partially acknowledged state. -/
inductive DeliveryCrashPoint where
  | beforeClaim
  | duringInference
  | duringAcknowledgement
  deriving DecidableEq, Repr

structure WakeRecoveryProjection where
  requestState : RequestState
  attemptedBindings : List NotificationBinding
  acknowledgedBindings : List NotificationBinding
  retryEligible : Bool
  deriving DecidableEq, Repr

structure WakeRecoveryInput where
  requestState : RequestState
  claimSnapshot : Option WakeAttemptSnapshot
  deriving DecidableEq, Repr

def attemptedFromSnapshot : Option WakeAttemptSnapshot → List NotificationBinding
  | none => []
  | some snapshot => snapshot.attemptedBindings

def recoverWakeDelivery (input : WakeRecoveryInput) : WakeRecoveryProjection :=
  let attempted := attemptedFromSnapshot input.claimSnapshot
  { requestState := input.requestState
  , attemptedBindings := attempted
  , acknowledgedBindings := if input.requestState = .completed then attempted else []
  , retryEligible := input.requestState == .failed && input.claimSnapshot.isSome
  }

theorem recovery_preserves_request (input : WakeRecoveryInput) :
    (recoverWakeDelivery input).requestState = input.requestState := by
  rfl

theorem live_claim_acknowledges_nothing_and_cannot_redrive
    (input : WakeRecoveryInput)
    (h : input.requestState = .claimed ∨ input.requestState = .processing) :
    (recoverWakeDelivery input).acknowledgedBindings = [] ∧
      (recoverWakeDelivery input).retryEligible = false := by
  rcases h with h | h <;> simp [recoverWakeDelivery, h]

theorem recovery_does_not_retry_cancelled_wake (input : WakeRecoveryInput)
    (h : input.requestState = .interrupted ∨ input.requestState = .superseded ∨
      input.requestState = .dead) :
    (recoverWakeDelivery input).retryEligible = false := by
  rcases h with h | h | h <;> simp [recoverWakeDelivery, h, isTerminal]

def deliveryCrashInput : DeliveryCrashPoint → WakeRecoveryInput
  | .beforeClaim =>
      { requestState := .pending
      , claimSnapshot := none
      }
  | .duringInference =>
      { requestState := .processing
      , claimSnapshot := some canonicalSnapshotFixture
      }
  | .duringAcknowledgement =>
      { requestState := .completed
      , claimSnapshot := some canonicalSnapshotFixture
      }

def recoverDeliveryCrash (point : DeliveryCrashPoint) : WakeRecoveryProjection :=
  recoverWakeDelivery (deliveryCrashInput point)

def deliveryCrashRecoveryAccepted : DeliveryCrashPoint → Bool
  | .beforeClaim =>
      decide
        (recoverDeliveryCrash .beforeClaim =
          { requestState := .pending
          , attemptedBindings := []
          , acknowledgedBindings := []
          , retryEligible := false
          })
  | .duringInference =>
      let recovered := recoverDeliveryCrash .duringInference
      decide
        (recovered.requestState = .processing ∧
         recovered.attemptedBindings = [attemptedBindingFixture] ∧
         recovered.acknowledgedBindings = [] ∧
         recovered.retryEligible = false)
  | .duringAcknowledgement =>
      let recovered := recoverDeliveryCrash .duringAcknowledgement
      decide
        (recovered.requestState = .completed ∧
         recovered.acknowledgedBindings = recovered.attemptedBindings ∧
         recovered.retryEligible = false)

theorem restart_before_claim_preserves_pending_delivery :
    deliveryCrashRecoveryAccepted .beforeClaim = true := by
  native_decide

theorem live_inference_retains_snapshot_without_redrive :
    deliveryCrashRecoveryAccepted .duringInference = true := by
  native_decide

theorem acknowledgement_projection_has_no_partial_crash_state :
    deliveryCrashRecoveryAccepted .duringAcknowledgement = true := by
  native_decide

end BackgroundCompletion
