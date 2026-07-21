import Proofs.Basic

/-!
# Layer 1: Agent Process Lifecycle

Models the agent process: startup, recovery, normal operation, shutdown.
The key insight is that `recovering` is an explicit state where claims
are blocked until startup recovery and runtime publication are complete.
-/

/-- The 5 states of the agent process lifecycle. -/
inductive ProcessState where
  | uninitialized
  | recovering
  | ready
  | shuttingDown
  | shutdown
  deriving DecidableEq, Repr

namespace ProcessState

/-- String vocabulary persisted in `AgentRuntime.process_state`. -/
def toDefraDB : ProcessState → String
  | .uninitialized => "uninitialized"
  | .recovering => "recovering"
  | .ready => "ready"
  | .shuttingDown => "shuttingDown"
  | .shutdown => "shutdown"

/-- Parse the persisted `AgentRuntime.process_state` vocabulary. -/
def fromDefraDB? : String → Option ProcessState
  | "uninitialized" => some .uninitialized
  | "recovering" => some .recovering
  | "ready" => some .ready
  | "shuttingDown" => some .shuttingDown
  | "shutdown" => some .shutdown
  | _ => none

theorem fromDefraDB_toDefraDB (s : ProcessState) :
    fromDefraDB? s.toDefraDB = some s := by
  cases s <;> rfl

instance : HasTerminal ProcessState where
  isTerminal s := s = .shutdown
  isTerminal_dec s := decEq s .shutdown

/-- A process state accepts new work only when ready. -/
def acceptsWork : ProcessState → Prop
  | .ready => True
  | .uninitialized => False
  | .recovering => False
  | .shuttingDown => False
  | .shutdown => False

instance : DecidablePred acceptsWork := fun s =>
  match s with
  | .ready => isTrue trivial
  | .uninitialized => isFalse (fun h => h)
  | .recovering => isFalse (fun h => h)
  | .shuttingDown => isFalse (fun h => h)
  | .shutdown => isFalse (fun h => h)

/-- Whether there are stuck requests requiring recovery at startup. -/
structure StartupContext where
  hasStuckRequests : Bool
  activeRequestCount : Nat
  deriving DecidableEq, Repr

/-- Process lifecycle transitions. -/
inductive Transition : ProcessState → ProcessState → Prop where
  | startup_recover (ctx : StartupContext) :
      ctx.hasStuckRequests = true →
      Transition .uninitialized .recovering
  | startup_clean (ctx : StartupContext) :
      ctx.hasStuckRequests = false →
      Transition .uninitialized .ready
  | recovery_complete :
      Transition .recovering .ready
  | begin_shutdown :
      Transition .ready .shuttingDown
  | finish_shutdown :
      (activeRequestCount : Nat) →
      activeRequestCount = 0 →
      Transition .shuttingDown .shutdown

/-- Executable process actions mirroring `Transition`. -/
inductive Action where
  | startupRecover (ctx : StartupContext)
  | startupClean (ctx : StartupContext)
  | recoveryComplete
  | beginShutdown
  | finishShutdown (activeRequestCount : Nat)
  deriving DecidableEq, Repr

/-- Executable transition function for the process layer. -/
def step? (pre : ProcessState) : Action → Option ProcessState
  | .startupRecover ctx =>
      if pre = .uninitialized ∧ ctx.hasStuckRequests = true then
        some .recovering
      else
        none
  | .startupClean ctx =>
      if pre = .uninitialized ∧ ctx.hasStuckRequests = false then
        some .ready
      else
        none
  | .recoveryComplete =>
      if pre = .recovering then some .ready else none
  | .beginShutdown =>
      if pre = .ready then some .shuttingDown else none
  | .finishShutdown activeRequestCount =>
      if pre = .shuttingDown ∧ activeRequestCount = 0 then
        some .shutdown
      else
        none

/-- A trace is a sequence of valid process transitions. -/
inductive Trace : ProcessState → ProcessState → Prop where
  | refl {s : ProcessState} : Trace s s
  | step {s₁ s₂ s₃ : ProcessState} :
      Transition s₁ s₂ → Trace s₂ s₃ → Trace s₁ s₃

/-- Replay a finite action list through the executable process semantics. -/
def replay? : ProcessState → List Action → Option ProcessState
  | s, [] => some s
  | s, action :: rest =>
      match step? s action with
      | some s' => replay? s' rest
      | none => none

theorem step_sound
    {pre post : ProcessState}
    {action : Action}
    (h_step : step? pre action = some post) :
    Transition pre post := by
  cases action with
  | startupRecover ctx =>
      simp [step?] at h_step
      rcases h_step with ⟨h_state, h_post⟩
      rcases h_state with ⟨h_pre, h_stuck⟩
      subst pre
      subst post
      simpa using Transition.startup_recover ctx h_stuck
  | startupClean ctx =>
      simp [step?] at h_step
      rcases h_step with ⟨h_state, h_post⟩
      rcases h_state with ⟨h_pre, h_stuck⟩
      subst pre
      subst post
      simpa using Transition.startup_clean ctx h_stuck
  | recoveryComplete =>
      simp [step?] at h_step
      rcases h_step with ⟨h_pre, h_post⟩
      subst pre
      subst post
      simpa using Transition.recovery_complete
  | beginShutdown =>
      simp [step?] at h_step
      rcases h_step with ⟨h_pre, h_post⟩
      subst pre
      subst post
      simpa using Transition.begin_shutdown
  | finishShutdown activeRequestCount =>
      simp [step?] at h_step
      rcases h_step with ⟨h_state, h_post⟩
      rcases h_state with ⟨h_pre, h_zero⟩
      subst pre
      subst post
      simpa using Transition.finish_shutdown activeRequestCount h_zero

theorem transition_complete
    {pre post : ProcessState}
    (h_trans : Transition pre post) :
    ∃ action : Action, step? pre action = some post := by
  cases h_trans with
  | startup_recover ctx h_stuck =>
      refine ⟨.startupRecover ctx, ?_⟩
      simp [step?, h_stuck]
  | startup_clean ctx h_stuck =>
      refine ⟨.startupClean ctx, ?_⟩
      simp [step?, h_stuck]
  | recovery_complete =>
      refine ⟨.recoveryComplete, ?_⟩
      simp [step?]
  | begin_shutdown =>
      refine ⟨.beginShutdown, ?_⟩
      simp [step?]
  | finish_shutdown activeRequestCount h_zero =>
      refine ⟨.finishShutdown activeRequestCount, ?_⟩
      simp [step?, h_zero]

theorem replay_sound
    {pre post : ProcessState}
    {actions : List Action}
    (h_replay : replay? pre actions = some post) :
    Trace pre post := by
  induction actions generalizing pre with
  | nil =>
      simp [replay?] at h_replay
      subst h_replay
      exact Trace.refl
  | cons action rest ih =>
      simp [replay?] at h_replay
      rcases h_step : step? pre action with (_ | next)
      · simp [h_step] at h_replay
      · simp [h_step] at h_replay
        have h_trans : Transition pre next := step_sound h_step
        exact Trace.step h_trans (ih h_replay)

theorem trace_complete
    {pre post : ProcessState}
    (h_trace : Trace pre post) :
    ∃ actions : List Action, replay? pre actions = some post := by
  induction h_trace with
  | refl =>
      exact ⟨[], rfl⟩
  | step h_trans h_trace ih =>
      rcases transition_complete h_trans with ⟨action, h_action⟩
      rcases ih with ⟨actions, h_actions⟩
      refine ⟨action :: actions, ?_⟩
      simp [replay?, h_action, h_actions]

end ProcessState
