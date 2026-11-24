import Proofs.Shared.Basic

inductive ProcessState where
  | uninitialized
  | recovering
  | ready
  | shuttingDown
  | shutdown
  deriving DecidableEq, Repr

namespace ProcessState

def toDefraDB : ProcessState → String
  | .uninitialized => "uninitialized"
  | .recovering => "recovering"
  | .ready => "ready"
  | .shuttingDown => "shuttingDown"
  | .shutdown => "shutdown"

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

structure StartupContext where
  hasStuckRequests : Bool
  deriving DecidableEq, Repr

inductive Transition : ProcessState → ProcessState → Prop where
  | startup_recover (ctx : StartupContext) :
      ctx.hasStuckRequests = true →
      Transition .uninitialized .recovering
  | startup_clean (ctx : StartupContext) :
      ctx.hasStuckRequests = false →
      Transition .uninitialized .ready
  | recovery_complete :
      Transition .recovering .ready
  /-- Shutdown begins from any state that has not begun it: a startup that
  fails, or a signal that arrives while the process is still uninitialized or
  recovering, shuts the process down as surely as a signal to a ready one. -/
  | begin_shutdown (pre : ProcessState) :
      pre ≠ .shuttingDown → pre ≠ .shutdown →
      Transition pre .shuttingDown
  | finish_shutdown :
      (activeRequestCount : Nat) →
      activeRequestCount = 0 →
      Transition .shuttingDown .shutdown
  /-- A newly started process adopts the durable state its predecessor left
  behind. A runtime killed without shutdown leaves its last durable state in
  place; the store has no liveness signal and document age is not a lease, so
  the successor's own claim is the only fact that separates the two processes.
  A cleanly shut-down state is already fail-closed and is not reclaimed. -/
  | restart_claim (pre : ProcessState) :
      pre ≠ .shutdown →
      Transition pre .recovering

inductive Action where
  | startupRecover (ctx : StartupContext)
  | startupClean (ctx : StartupContext)
  | recoveryComplete
  | beginShutdown
  | finishShutdown (activeRequestCount : Nat)
  | restartClaim
  deriving DecidableEq, Repr

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
      if pre = .shuttingDown ∨ pre = .shutdown then none else some .shuttingDown
  | .finishShutdown activeRequestCount =>
      if pre = .shuttingDown ∧ activeRequestCount = 0 then
        some .shutdown
      else
        none
  | .restartClaim =>
      if pre = .shutdown then none else some .recovering

/-- The process lifecycle actions without their parameters: the runtime passes
whether startup found stuck requests and whether the active request count is
zero beside the action. -/
inductive ActionTag where
  | startupRecover
  | startupClean
  | recoveryComplete
  | beginShutdown
  | finishShutdown
  | restartClaim
  deriving DecidableEq, Repr

/-- The state a durable process-state write moves to, or `none` when the write
is refused and the durable state stays `pre`. `pre` is the state this process
last persisted; a process starts uninitialized. `stuckRequests` is whether
startup recovery repaired work a predecessor left, or could not tell.
`activeIsZero` is whether no request this process admitted can still be
running: its admission gate never opened, or its ordered teardown joined every
task that runs requests. A teardown cut by its deadline aborts those tasks and
leaves their requests to the next start's recovery, so it never writes
Shutdown. -/
def next (pre : ProcessState) (action : ActionTag) (stuckRequests activeIsZero : Bool) :
    Option ProcessState :=
  step? pre <| match action with
    | .startupRecover => .startupRecover { hasStuckRequests := stuckRequests }
    | .startupClean => .startupClean { hasStuckRequests := stuckRequests }
    | .recoveryComplete => .recoveryComplete
    | .beginShutdown => .beginShutdown
    | .finishShutdown => .finishShutdown (if activeIsZero then 0 else 1)
    | .restartClaim => .restartClaim

/-- Ready is reached only from startup or recovery, never from Ready itself. -/
theorem ready_is_not_rewritten (action : ActionTag) (stuck active : Bool) :
    next .ready action stuck active ≠ some .ready := by
  cases action <;> cases stuck <;> cases active <;> decide

/-- Shutdown is reached only from ShuttingDown with no active request. -/
theorem shutdown_follows_drained_shutting_down (pre : ProcessState) (action : ActionTag)
    (stuck active : Bool) (h : next pre action stuck active = some .shutdown) :
    pre = .shuttingDown ∧ active = true := by
  cases pre <;> cases action <;> cases stuck <;> cases active <;> simp_all [next, step?]

/-- Every state that has not begun shutdown can begin it. -/
theorem shutdown_begins_before_it_has_begun (pre : ProcessState) (stuck active : Bool)
    (h_shutting : pre ≠ .shuttingDown) (h_shutdown : pre ≠ .shutdown) :
    next pre .beginShutdown stuck active = some .shuttingDown := by
  cases pre <;> simp_all [next, step?]

/-- A restart claim can never assert readiness: it is the fail-closed half of
adoption, so no durable predecessor state lets a claiming process report work
it has not yet admitted. -/
theorem restart_claim_never_ready (pre : ProcessState) :
    step? pre .restartClaim ≠ some .ready := by
  cases pre <;> simp [step?]

/-- Claiming is idempotent, so a process that claims an already-recovering row
performs no further state change and the publisher owes no extra write. -/
theorem restart_claim_is_idempotent :
    step? .recovering .restartClaim = some .recovering := by
  simp [step?]

/-- A cleanly shut-down row is already fail-closed and carries its predecessor's
final picture; claiming leaves it untouched. -/
theorem shutdown_is_absorbing_under_claim :
    step? .shutdown .restartClaim = none := by
  simp [step?]

inductive Trace : ProcessState → ProcessState → Prop where
  | refl {s : ProcessState} : Trace s s
  | step {s₁ s₂ s₃ : ProcessState} :
      Transition s₁ s₂ → Trace s₂ s₃ → Trace s₁ s₃

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
      rcases h_step with ⟨⟨h_not_shutting, h_not_shutdown⟩, h_post⟩
      subst post
      exact Transition.begin_shutdown pre h_not_shutting h_not_shutdown
  | finishShutdown activeRequestCount =>
      simp [step?] at h_step
      rcases h_step with ⟨h_state, h_post⟩
      rcases h_state with ⟨h_pre, h_zero⟩
      subst pre
      subst post
      simpa using Transition.finish_shutdown activeRequestCount h_zero
  | restartClaim =>
      by_cases h_shutdown : pre = .shutdown
      · simp [step?, h_shutdown] at h_step
      · simp [step?, h_shutdown] at h_step
        subst h_step
        exact Transition.restart_claim pre h_shutdown

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
  | begin_shutdown pre h_not_shutting h_not_shutdown =>
      refine ⟨.beginShutdown, ?_⟩
      simp [step?, h_not_shutting, h_not_shutdown]
  | finish_shutdown activeRequestCount h_zero =>
      refine ⟨.finishShutdown activeRequestCount, ?_⟩
      simp [step?, h_zero]
  | restart_claim pre h_shutdown =>
      refine ⟨.restartClaim, ?_⟩
      simp [step?, h_shutdown]

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
