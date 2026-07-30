import Proofs.Session.Properties.Executable
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
4. a canonical coalesced wake is enqueued only after that append exists; and
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

end BackgroundCompletion
