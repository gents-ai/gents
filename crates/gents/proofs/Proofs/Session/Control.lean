import Proofs.Session.Interrupt
import Proofs.Goals

namespace SessionQueue.Control

/-- Durable stop intent belongs to the exact canonical session scope. Its
native owner persists the latch on AgentSession and rechecks it at admission
and claim; the single-active-runtime-per-node operating convention remains the
external ordering premise. -/
structure State where
  queue : SessionQueueState
  request : Option RequestContext
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
  if goal.status = .paused then (Goals.step? goal .resume).getD goal else goal

def interruptCurrent (target : AgentSession.Scope) (state : State) : Option State :=
  if state.queue.scope = target then
    match state.request with
    | none => some { state with goal := pauseGoal state.goal }
    | some request => do
        let (request, queue) ← latchInterruptScoped target request state.queue
        some { state with request := some request, queue, goal := pauseGoal state.goal }
  else none

def step? (target : AgentSession.Scope) (state : State) : Action → Option State
  | .interrupt =>
      interruptCurrent target state
  | .stop =>
      if state.queue.scope = target then
        let latched := interruptCurrent target state |>.getD state
        some { latched with queue := latched.queue.drainAllPending,
          goal := pauseGoal latched.goal, stopped := true }
      else none
  | .resume =>
      if state.queue.scope = target ∧
          ((state.request.isNone || state.request.all fun request =>
            decide (request.state ≠ .claimed ∧ request.state ≠ .processing)) = true) then
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

def scope : AgentSession.Scope := ⟨1, 900, some 1⟩

def ordinary : QueueEntry :=
  { requestId := 301, createdAt := 10, source := .user, policy := .append,
    queueKey := none, queuedAfter := none }

def wake : QueueEntry :=
  { requestId := 302, createdAt := 11, source := .backgroundCompletion,
    policy := .coalesce, queueKey := some 900, queuedAfter := none,
    origin := .scheduled }

def initial : State :=
  { queue := { scope := scope, active := some 300, pending := [ordinary, wake],
      terminal := ∅ },
    request := some { state := .processing, origin := .interactive,
      backend := ⟨"stop-test"⟩, admission := .executing, deadline := 100,
      claimTime := 0, currentTime := 10, retryCount := 0, maxRetries := 3,
      messageSeq := 0, persistence := .uncommitted },
    goal := { status := .active, blockedAudits := 1, wrapupRequested := false,
      wrapupCompleted := false }, stopped := false }

def idleInterruptInitial : State :=
  { initial with
    queue := { initial.queue with active := none, pending := [], terminal := ∅ },
    request := none }

def queuedStopInitial : State :=
  { idleInterruptInitial with queue := { idleInterruptInitial.queue with pending := [ordinary] } }

def idleInterruptPost : State :=
  (step? scope idleInterruptInitial .interrupt).getD idleInterruptInitial
def queuedStopPost : State :=
  (step? scope queuedStopInitial .stop).getD queuedStopInitial

example : ((step? scope initial .interrupt).getD initial).queue.pending = [ordinary] ∧
    302 ∈ ((step? scope initial .interrupt).getD initial).queue.terminal ∧
    ((step? scope initial .interrupt).getD initial).goal.status = .paused ∧
    ((step? scope initial .interrupt).getD initial).request.map
      (fun request => request.interruptRequestedAt.isSome) = some true := by
  native_decide

example : queuedStopPost.stopped = true ∧
    queuedStopPost.queue.pending = [] ∧
    queuedStopPost.queue.active = none := by
  native_decide

example : step? scope ((step? scope initial .stop).getD initial) (.enqueue wake) = none := by
  native_decide

example : step? scope ((step? scope initial .stop).getD initial) .resume = none := by
  native_decide

example : ((step? scope
    { initial with stopped := true, goal := { initial.goal with status := .complete },
      request := none } .resume).getD initial).stopped = false := by
  native_decide

example : step? scope ((step? scope initial .stop).getD initial) (.enqueue ordinary) = none := by
  native_decide

example : step? ⟨2, 900, some 1⟩ initial .stop = none := by
  native_decide

end SessionQueue.Control
