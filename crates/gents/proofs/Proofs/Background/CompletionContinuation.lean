import Proofs.Session.Properties.Executable
import Proofs.Request
import Proofs.Goals
import Proofs.ToolExecution.State
import Proofs.Transcript.State

/-!
# Background Completion Continuation

The bridge and tool-call models terminalize work, the transcript model appends
messages, and the session model coalesces and claims wake requests. This module
composes those seams into the model-facing background-completion contract:

1. an in-flight assistant wait call is durably reserved before it can block;
2. only a terminal parent-visible tool may produce a completion notification;
3. the notification is appended after the reserved assistant row;
4. without a canonical Goal, a coalesced wake is enqueued only after that append exists; and
5. when that wake is claimed, the continuation still carries the transcript
   containing both rows in the correct order.

Both local subagents and native background processes converge on a terminal
parent `ToolCallState`, so they share this continuation model.
-/

namespace BackgroundCompletion

open ToolExecution

/-- One parent-visible terminal completion and the wake row it owes. -/
structure TerminalCompletion where
  toolState : ToolCallState
  notificationMessageId : Transcript.MessageId
  wake : SessionQueue.QueueEntry
  deriving Repr

/-- Evidence that the terminal completion has become a durable user-role
message in the parent transcript. The exact append shape makes ordering
explicit: later stages can only be constructed from this value. -/
structure NotifiedCompletion where
  completion : TerminalCompletion
  preTranscript : Transcript.TranscriptState
  transcript : Transcript.TranscriptState
  terminal : isTerminal completion.toolState
  appended :
    transcript =
      preTranscript.appendUserMessage
        completion.notificationMessageId
        Transcript.MessageKind.ordinary

/-- Append the model-visible notification. Live work cannot enter this stage. -/
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
      , appended := rfl
      }
  else
    none

/-- Evidence that the canonical wake was enqueued after notification
persistence. The queue transition is the executable session semantics, not a
hand-written post-state. -/
structure QueuedCompletion where
  notified : NotifiedCompletion
  preQueue : SessionQueue.SessionQueueState
  queue : SessionQueue.SessionQueueState
  sameSession : preQueue.sessionId = notified.transcript.sessionId
  wakeWellFormed :
    notified.completion.wake.coalesceWellFormed preQueue.sessionId
  wakeKeyMissing :
    SessionQueue.containsCoalescedQueueKey
      preQueue.pending
      SessionQueue.QueueSource.backgroundCompletion
      preQueue.sessionId = false
  enqueued : SessionQueue.Transition preQueue queue

/-- Enqueue the first coalesced completion wake for this parent session.
Subsequent terminal completions reuse the session queue's separately-proved
coalescing path while each still gets its own transcript notification. -/
def enqueueWake?
    (notified : NotifiedCompletion)
    (pre : SessionQueue.SessionQueueState) :
    Option QueuedCompletion :=
  if h_session : pre.sessionId = notified.transcript.sessionId then
    if h_well_formed :
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
              , enqueued := SessionQueue.step?_sound h_step
              }
      else
        none
    else
      none
  else
    none

/-! Canonical Goal presence selects the existing Goal continuation owner,
including paused and terminal Goals. This is not another budget/status policy.
The observation is the canonical same-principal/session Goal in the existing
publication transaction; no absent-scan phantom protection is claimed.
Existing pending wakes are not canceled by this projection. -/
def enqueueWakeForOwner?
    (goal : Option Goals.Status)
    (notified : NotifiedCompletion)
    (pre : SessionQueue.SessionQueueState) : Option QueuedCompletion :=
  match goal with
  | some _ => none
  | none => enqueueWake? notified pre

theorem goal_owned_notification_does_not_enqueue
    (status : Goals.Status) (notified : NotifiedCompletion)
    (pre : SessionQueue.SessionQueueState) :
    enqueueWakeForOwner? (some status) notified pre = none := by
  rfl

theorem non_goal_enqueue_preserves_existing_policy
    (notified : NotifiedCompletion) (pre : SessionQueue.SessionQueueState) :
    enqueueWakeForOwner? none notified pre = enqueueWake? notified pre := by
  rfl

/-- A claimed continuation retains the notified transcript by construction. -/
structure Continuation where
  queued : QueuedCompletion
  queue : SessionQueue.SessionQueueState
  claimed : SessionQueue.Transition queued.queue queue
  activeWake :
    queue.active = some queued.notified.completion.wake.requestId

/-- Claim the completion wake when it reaches the head of the same-session
queue. Earlier foreground/user work may run first; this action becomes
executable precisely when the completion wake is the claimable head. -/
def claimContinuation? (queued : QueuedCompletion) : Option Continuation :=
  match h_step : SessionQueue.step? queued.queue .claimNext with
  | none => none
  | some post =>
      if h_active :
          post.active = some queued.notified.completion.wake.requestId then
        some
          { queued := queued
          , queue := post
          , claimed := SessionQueue.step?_sound h_step
          , activeWake := h_active
          }
      else
        none

/-- The exact durable row shape the next provider turn must see. -/
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
  let row : Transcript.MessageRow :=
    { messageId := notified.completion.notificationMessageId
    , sessionId := notified.preTranscript.sessionId
    , sequence := notified.preTranscript.nextSeq
    , role := .user
    , kind := .ordinary
    }
  refine ⟨row, ?_, rfl, ?_, rfl, rfl⟩
  · rw [notified.appended]
    simp [Transcript.TranscriptState.appendUserMessage, row]
  · rw [notified.appended]
    rfl

theorem goal_owned_delivery_retains_notification_without_wake
    (status : Goals.Status) (notified : NotifiedCompletion)
    (pre : SessionQueue.SessionQueueState) :
    HasNotification notified ∧ enqueueWakeForOwner? (some status) notified pre = none :=
  ⟨notified_completion_has_durable_message notified,
    goal_owned_notification_does_not_enqueue status notified pre⟩

/-- Acceptance theorem: once a background wake is claimed, its parent-visible
terminal state and durable notification are still available to provider-input
assembly for the continuation turn. -/
theorem claimed_continuation_sees_terminal_notification
    (continuation : Continuation) :
    isTerminal continuation.queued.notified.completion.toolState ∧
    HasNotification continuation.queued.notified ∧
    continuation.queue.active =
      some continuation.queued.notified.completion.wake.requestId :=
  ⟨continuation.queued.notified.terminal,
    notified_completion_has_durable_message continuation.queued.notified,
    continuation.activeWake⟩

/-! ## Executable canonical acceptance witness -/

def canonicalTranscript : Transcript.TranscriptState :=
  { sessionId := 900
  , nextSeq := 3
  , messages := []
  , toolCalls := []
  , inFlight := ∅
  , assistantTurn := none
  }

/-- The model-visible wait call is persisted before execution blocks. This is
the durable form of the assistant sequence reservation: an independently
appended completion notification must observe `nextSeq = 4`, not reuse the
assistant row's sequence 3. -/
def canonicalWaitTurn : Transcript.AssistantTurn :=
  { sessionId := canonicalTranscript.sessionId
  , sequence := canonicalTranscript.nextSeq
  , callIds := {51}
  }

def canonicalWaitReservedTranscript : Transcript.TranscriptState :=
  let started := canonicalTranscript.beginAssistantToolCall 51
  started.persistAssistantMessage 40 canonicalWaitTurn

def canonicalWake : SessionQueue.QueueEntry :=
  { requestId := 901
  , createdAt := 10
  , source := .backgroundCompletion
  , policy := .coalesce
  , queueKey := some 900
  , queuedAfter := some 899
  }

def canonicalQueue : SessionQueue.SessionQueueState :=
  { sessionId := 900
  , active := none
  , pending := []
  , terminal := ∅
  }

def canonicalCompletion : TerminalCompletion :=
  { toolState := .completed
  , notificationMessageId := 41
  , wake := canonicalWake
  }

/-- Executable four-stage acceptance result used by generated conformance
cases. Any guard drift in terminality, transcript append, wake metadata,
coalescing, or claim behavior flips this value. -/
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

/-- Regression for the live wait/completion race: the assistant wait row and
the background notification have distinct durable sequences, the assistant
row remains assistant-role, and it precedes the user-role notification. -/
def canonicalWaitOrderingAccepted : Bool :=
  match appendNotification?
      canonicalCompletion canonicalWaitReservedTranscript with
  | none => false
  | some notified =>
      decide
        (notified.transcript.messages.map
          (fun row => (row.sequence, row.role)) =
          [(3, .assistant), (4, .user)])

theorem canonical_wait_call_precedes_completion_notification :
    canonicalWaitOrderingAccepted = true := by
  native_decide

/-! ## Bounded failed-wake redrive

Completion notifications remain durable when the scheduled continuation that
was supposed to consume them fails.  The daemon may therefore create one
fresh continuation attempt, but only for the canonical coalesced background
queue shape and only while the persisted retry budget remains open.  This is a
separate capability from the interactive client retry modeled by
`SessionRecovery`: a generic scheduled request is not retryable merely because
it was scheduled.
-/

structure FailedWake where
  ctx : RequestContext
  source : SessionQueue.QueueSource
  policy : SessionQueue.QueuePolicy
  queueKey : Option SessionId

def CanRedriveWake (wake : FailedWake) : Prop :=
  wake.ctx.state = .failed ∧
    wake.ctx.admission = .released ∧
    wake.ctx.origin = .scheduled ∧
    wake.ctx.isLatest = true ∧
    wake.ctx.retryCount < wake.ctx.maxRetries ∧
    wake.source = .backgroundCompletion ∧
    wake.policy = .coalesce ∧
    wake.queueKey.isSome

instance (wake : FailedWake) : Decidable (CanRedriveWake wake) := by
  unfold CanRedriveWake
  infer_instance

def redrivenWakeContext (ctx : RequestContext) : RequestContext :=
  { state := .pending
  , origin := .scheduled
  , backend := ctx.backend
  , admission := .released
  , deadline := ctx.currentTime + 1
  , claimTime := ctx.currentTime
  , currentTime := ctx.currentTime
  , retryCount := ctx.retryCount + 1
  , maxRetries := ctx.maxRetries
  , progressSeq := 0
  , messageSeq := 0
  , isLatest := true
  , persistence := .uncommitted
  }

def redriveWake? (wake : FailedWake) : Option RequestContext :=
  if _h : CanRedriveWake wake then
    some (redrivenWakeContext wake.ctx)
  else
    none

/-- Legacy failed wakes also defer to the canonical Goal owner. This guard
is applied at publication, not merely during candidate discovery. -/
def redriveWakeForOwner?
    (goal : Option Goals.Status) (wake : FailedWake) : Option RequestContext :=
  match goal with
  | some _ => none
  | none => redriveWake? wake

theorem goal_owned_failed_wake_cannot_redrive
    (status : Goals.Status) (wake : FailedWake) :
    redriveWakeForOwner? (some status) wake = none := by
  rfl

theorem non_goal_redrive_preserves_existing_policy (wake : FailedWake) :
    redriveWakeForOwner? none wake = redriveWake? wake := by
  rfl

theorem redriveWake?_bounded
    {wake : FailedWake}
    {post : RequestContext}
    (h_redrive : redriveWake? wake = some post) :
    post.state = .pending ∧
      post.origin = .scheduled ∧
      post.backend = wake.ctx.backend ∧
      post.retryCount = wake.ctx.retryCount + 1 ∧
      post.retryCount ≤ post.maxRetries := by
  simp [redriveWake?] at h_redrive
  rcases h_redrive with ⟨h_can, rfl⟩
  rcases h_can with ⟨_, _, _, _, h_budget, _, _, _⟩
  simp [redrivenWakeContext, Nat.succ_le_of_lt h_budget]

def failedWakeFixture
    (state : RequestState := .failed)
    (origin : ExecutionOrigin := .scheduled)
    (retryCount : Nat := 0)
    (maxRetries : Nat := 3)
    (isLatest : Bool := true)
    (source : SessionQueue.QueueSource := .backgroundCompletion)
    (policy : SessionQueue.QueuePolicy := .coalesce)
    (queueKey : Option SessionId := some 900) : FailedWake :=
  { ctx :=
      { state := state
      , origin := origin
      , backend := { val := "background-wake-backend" }
      , admission := .released
      , deadline := 1
      , claimTime := 0
      , currentTime := 10
      , retryCount := retryCount
      , maxRetries := maxRetries
      , progressSeq := 0
      , messageSeq := 0
      , isLatest := isLatest
      , persistence := .committed
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
         post.maxRetries = 3)

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

The watcher preserves FIFO until a background-completion wake reaches the
aging threshold.  Once aged, it precedes ordinary descendant work at the
bounded behavior-executor queue.  This is the runtime's weak-fairness
assumption made executable: an ongoing descendant storm may fill the finite
queue ahead of a wake, but new descendants cannot continue overtaking it.
-/

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

/-- The behavior executor admits only a finite predecessor set.  Once an aged
wake is selected ahead of new descendants, its remaining wait is bounded by
the already-running workers plus the fixed dispatcher queue. -/
def predecessorBound (executorCapacity queueCapacity : Nat) : Nat :=
  executorCapacity + queueCapacity

theorem aged_wake_predecessors_bounded
    (executorCapacity queueCapacity predecessors : Nat)
    (h_bounded : predecessors ≤ executorCapacity + queueCapacity) :
    predecessors ≤ predecessorBound executorCapacity queueCapacity := by
  simpa [predecessorBound] using h_bounded

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

Each notification message is durably bound to the wake request created or
reused by the atomic enqueue transaction.  Claim snapshots the transcript's
last sequence in the same transaction that changes the wake from pending to
claimed.  A later notification is therefore owned by a successor epoch and
cannot enter the active attempt's provider input.  Successful terminalization
acknowledges exactly the attempted bindings; failure retains them for redrive.
-/

structure NotificationBinding where
  messageId : Transcript.MessageId
  sequence : Nat
  wakeRequestId : RequestId
  deriving DecidableEq, Repr

structure WakeAttemptSnapshot where
  wakeRequestId : RequestId
  throughSequence : Nat
  bindings : List NotificationBinding
  terminalState : RequestState
  deriving DecidableEq, Repr

def WakeAttemptSnapshot.attemptedBindings
    (snapshot : WakeAttemptSnapshot) : List NotificationBinding :=
  snapshot.bindings.filter fun binding =>
    binding.wakeRequestId = snapshot.wakeRequestId &&
      binding.sequence ≤ snapshot.throughSequence

def WakeAttemptSnapshot.acknowledgedBindings
    (snapshot : WakeAttemptSnapshot) : List NotificationBinding :=
  if snapshot.terminalState = .completed then snapshot.attemptedBindings else []

theorem successor_binding_after_cutoff_not_attempted
    (snapshot : WakeAttemptSnapshot)
    (binding : NotificationBinding)
    (h_after : snapshot.throughSequence < binding.sequence) :
    binding ∉ snapshot.attemptedBindings := by
  simp [WakeAttemptSnapshot.attemptedBindings, Nat.not_le.mpr h_after]

theorem completed_attempt_acknowledges_exact_snapshot
    (snapshot : WakeAttemptSnapshot)
    (h_completed : snapshot.terminalState = .completed) :
    snapshot.acknowledgedBindings = snapshot.attemptedBindings := by
  simp [WakeAttemptSnapshot.acknowledgedBindings, h_completed]

theorem failed_attempt_acknowledges_nothing
    (snapshot : WakeAttemptSnapshot)
    (h_failed : snapshot.terminalState = .failed) :
    snapshot.acknowledgedBindings = [] := by
  simp [WakeAttemptSnapshot.acknowledgedBindings, h_failed]

def attemptedBindingFixture : NotificationBinding :=
  { messageId := 41, sequence := 4, wakeRequestId := 901 }

def successorBindingFixture : NotificationBinding :=
  { messageId := 42, sequence := 6, wakeRequestId := 902 }

def completedSnapshotFixture : WakeAttemptSnapshot :=
  { wakeRequestId := 901
  , throughSequence := 5
  , bindings := [attemptedBindingFixture, successorBindingFixture]
  , terminalState := .completed
  }

def failedSnapshotFixture : WakeAttemptSnapshot :=
  { completedSnapshotFixture with terminalState := .failed }

theorem canonical_completed_snapshot_acknowledges_owned_notification :
    completedSnapshotFixture.acknowledgedBindings = [attemptedBindingFixture] := by
  native_decide

theorem canonical_successor_notification_excluded_from_active_snapshot :
    successorBindingFixture ∉ completedSnapshotFixture.attemptedBindings := by
  native_decide

theorem canonical_failed_snapshot_retains_unacknowledged_notification :
    failedSnapshotFixture.acknowledgedBindings = [] ∧
      failedSnapshotFixture.attemptedBindings = [attemptedBindingFixture] := by
  native_decide

/-! ## Crash-boundary recovery

Acknowledgement is not a second mutable protocol step.  It is a projection of
the durable claim snapshot and the recovered request terminal state.  This
closes the four crash boundaries in the delivery protocol: before claim there
is no attempted snapshot to acknowledge; an inference failure retains the
snapshot for bounded redrive; a committed successful response repairs the
request to completed; and a crash while a reader projects acknowledgement
cannot create a partially acknowledged state.
-/

inductive DeliveryCrashPoint where
  | beforeClaim
  | duringInference
  | afterResponsePersistence
  | duringAcknowledgement
  deriving DecidableEq, Repr

structure WakeRecoveryProjection where
  requestState : RequestState
  attemptedBindings : List NotificationBinding
  acknowledgedBindings : List NotificationBinding
  retryEligible : Bool
  deriving DecidableEq, Repr

inductive DurableResponseState where
  | absent
  | completed
  | failed
  deriving DecidableEq, Repr

structure WakeRecoveryInput where
  requestState : RequestState
  claimSnapshot : Option WakeAttemptSnapshot
  responseState : DurableResponseState
  deriving DecidableEq, Repr

def attemptedFromSnapshot : Option WakeAttemptSnapshot → List NotificationBinding
  | none => []
  | some snapshot => snapshot.attemptedBindings

/-- Recovery is computed only from durable facts.  A committed response wins
over a stale processing request; otherwise a durable claim snapshot proves an
attempt occurred and remains retryable.  With neither fact, the pending wake
is still unconsumed. -/
def recoverWakeDelivery (input : WakeRecoveryInput) : WakeRecoveryProjection :=
  let attempted := attemptedFromSnapshot input.claimSnapshot
  match input.responseState with
  | .completed =>
      { requestState := .completed
      , attemptedBindings := attempted
      , acknowledgedBindings := attempted
      , retryEligible := false
      }
  | .failed =>
      { requestState := .failed
      , attemptedBindings := attempted
      , acknowledgedBindings := []
      , retryEligible := input.claimSnapshot.isSome
      }
  | .absent =>
      match input.claimSnapshot with
      | none =>
          { requestState := .pending
          , attemptedBindings := []
          , acknowledgedBindings := []
          , retryEligible := false
          }
      | some snapshot =>
          { requestState := .failed
          , attemptedBindings := snapshot.attemptedBindings
          , acknowledgedBindings := []
          , retryEligible := true
          }

def deliveryCrashInput : DeliveryCrashPoint → WakeRecoveryInput
  | .beforeClaim =>
      { requestState := .pending
      , claimSnapshot := none
      , responseState := .absent
      }
  | .duringInference =>
      { requestState := .processing
      , claimSnapshot := some failedSnapshotFixture
      , responseState := .absent
      }
  | .afterResponsePersistence =>
      { requestState := .processing
      , claimSnapshot := some completedSnapshotFixture
      , responseState := .completed
      }
  | .duringAcknowledgement =>
      { requestState := .completed
      , claimSnapshot := some completedSnapshotFixture
      , responseState := .completed
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
        (recovered.requestState = .failed ∧
         recovered.attemptedBindings = [attemptedBindingFixture] ∧
         recovered.acknowledgedBindings = [] ∧
         recovered.retryEligible = true)
  | .afterResponsePersistence =>
      let recovered := recoverDeliveryCrash .afterResponsePersistence
      decide
        (recovered.requestState = .completed ∧
         recovered.acknowledgedBindings = recovered.attemptedBindings ∧
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

theorem inference_failure_retains_snapshot_for_redrive :
    deliveryCrashRecoveryAccepted .duringInference = true := by
  native_decide

theorem committed_response_recovers_exact_acknowledgement :
    deliveryCrashRecoveryAccepted .afterResponsePersistence = true := by
  native_decide

theorem acknowledgement_projection_has_no_partial_crash_state :
    deliveryCrashRecoveryAccepted .duringAcknowledgement = true := by
  native_decide

end BackgroundCompletion
