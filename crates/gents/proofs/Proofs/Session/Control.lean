import Proofs.Session.Interrupt
import Proofs.Goals

namespace SessionQueue.Control

/-- Durable stop intent belongs to the exact canonical session scope. Its
native owner persists the latch on AgentSession and rechecks it at admission
and claim; the single-active-runtime-per-node operating convention remains the
external ordering premise. -/
structure State where
  queue : SessionQueueState
  goal : Goals.State
  stopped : Bool
  deriving DecidableEq, Repr

inductive Action where
  | interrupt
  | stop
  | resume
  | enqueue (entry : QueueEntry)
  | claim
  deriving DecidableEq, Repr

def pauseGoal (goal : Goals.State) : Goals.State :=
  (Goals.step? goal .pause).getD goal

def resumeGoal (goal : Goals.State) : Goals.State :=
  (Goals.step? goal .resume).getD goal

def step? (target : AgentSession.Scope) (state : State) : Action → Option State
  | .interrupt =>
      if state.queue.scope = target then
        some { state with goal := pauseGoal state.goal }
      else none
  | .stop =>
      if state.queue.scope = target then
        some { state with queue := state.queue.drainAllPending,
          goal := pauseGoal state.goal, stopped := true }
      else none
  | .resume =>
      if state.queue.scope = target ∧ state.stopped then
        some { state with goal := resumeGoal state.goal, stopped := false }
      else none
  | .enqueue entry =>
      if state.queue.scope = target ∧ state.stopped = false then
        match SessionQueue.step? state.queue (.appendPending entry) with
        | some queue => some { state with queue }
        | none => none
      else none
  | .claim =>
      if state.queue.scope = target ∧ state.stopped = false then
        match SessionQueue.step? state.queue .claimNext with
        | some queue => some { state with queue }
        | none => none
      else none

theorem stopped_rejects_enqueue (target : AgentSession.Scope) (state post : State)
    (entry : QueueEntry)
    (hstop : state.stopped = true)
    (h : step? target state (.enqueue entry) = some post) : False := by
  simp [step?, hstop] at h

private def scope : AgentSession.Scope := ⟨1, 900, some 1⟩

private def ordinary : QueueEntry :=
  { requestId := 301, createdAt := 10, source := .user, policy := .append,
    queueKey := none, queuedAfter := none }

private def wake : QueueEntry :=
  { requestId := 302, createdAt := 11, source := .backgroundCompletion,
    policy := .coalesce, queueKey := some 900, queuedAfter := none,
    origin := .scheduled }

private def initial : State :=
  { queue := { scope := scope, active := some 300, pending := [ordinary, wake],
      terminal := ∅ },
    goal := { status := .active, blockedAudits := 1, wrapupRequested := false,
      wrapupCompleted := false }, stopped := false }

example : (step? scope initial .interrupt).map State.queue = some initial.queue ∧
    ((step? scope initial .interrupt).getD initial).goal.status = .paused := by
  native_decide

example : ((step? scope initial .stop).getD initial).stopped = true ∧
    ((step? scope initial .stop).getD initial).queue.pending = [] ∧
    ((step? scope initial .stop).getD initial).queue.active = some 300 := by
  native_decide

example : step? scope ((step? scope initial .stop).getD initial) (.enqueue wake) = none := by
  native_decide

example : ((step? scope ((step? scope initial .stop).getD initial) .resume).getD initial).stopped = false := by
  native_decide

example : step? ⟨2, 900, some 1⟩ initial .stop = none := by
  native_decide

end SessionQueue.Control
