import Proofs.Session.Request.State
import Proofs.Session.ToolExecution.State
import Proofs.Session.ManagedExec.Properties

namespace ManagedExec

structure ManagedExecComposedState where
  request : RequestContext
  tool : ToolExecution.ToolCallContext
  exec : ManagedExecContext
  now : Time
  deriving Repr

inductive Transition : ManagedExecComposedState → ManagedExecComposedState → Prop where
  | deadlineElapsed {pre post : ManagedExecComposedState}
      (h_tool : pre.tool.state = .running)
      (h_exec : pre.exec.state = .running)
      (h_deadline : pre.request.deadline < pre.now)
      (h_post : post =
        { pre with
          tool := { pre.tool with state := .timedOut }
        , exec := { pre.exec with state := .killSignaled
                                  , now := pre.now
                                  , killSignaledAt := some pre.now }
        })
      : Transition pre post

  | cancelRequested {pre post : ManagedExecComposedState}
      (h_tool : pre.tool.state = .running)
      (h_exec : pre.exec.state = .running)
      (h_post : post =
        { pre with
          tool := { pre.tool with state := .cancelled }
        , exec := { pre.exec with state := .killSignaled
                                  , now := pre.now
                                  , killSignaledAt := some pre.now }
        })
      : Transition pre post

inductive BoundedTrace : ManagedExecComposedState → ManagedExecComposedState → Nat → Prop where
  | refl {s : ManagedExecComposedState} : BoundedTrace s s 0
  | step {s₁ s₂ s₃ : ManagedExecComposedState} {n : Nat} :
      Transition s₁ s₂ → BoundedTrace s₂ s₃ n → BoundedTrace s₁ s₃ (n + 1)

def maxTimeoutSteps : Nat := 1

theorem running_tool_times_out_after_deadline_bounded
    (pre : ManagedExecComposedState)
    (h_tool : pre.tool.state = .running)
    (h_exec : pre.exec.state = .running)
    (h_deadline : pre.request.deadline < pre.now) :
    exists post,
      BoundedTrace pre post maxTimeoutSteps
      ∧ post.tool.state = .timedOut
      ∧ post.exec.state = .killSignaled := by
  let post : ManagedExecComposedState :=
    { pre with
      tool := { pre.tool with state := .timedOut }
    , exec := { pre.exec with state := .killSignaled
                              , now := pre.now
                              , killSignaledAt := some pre.now }
    }
  have h_step : Transition pre post :=
    Transition.deadlineElapsed
      (h_tool := h_tool)
      (h_exec := h_exec)
      (h_deadline := h_deadline)
      (h_post := rfl)
  exact ⟨post, BoundedTrace.step h_step BoundedTrace.refl, rfl, rfl⟩

theorem running_tool_cancelled_in_bounded_steps
    (pre : ManagedExecComposedState)
    (h_tool : pre.tool.state = .running)
    (h_exec : pre.exec.state = .running) :
    exists post,
      BoundedTrace pre post maxTimeoutSteps
      ∧ post.tool.state = .cancelled
      ∧ post.exec.state = .killSignaled := by
  let post : ManagedExecComposedState :=
    { pre with
      tool := { pre.tool with state := .cancelled }
    , exec := { pre.exec with state := .killSignaled
                              , now := pre.now
                              , killSignaledAt := some pre.now }
    }
  have h_step : Transition pre post :=
    Transition.cancelRequested
      (h_tool := h_tool)
      (h_exec := h_exec)
      (h_post := rfl)
  exact ⟨post, BoundedTrace.step h_step BoundedTrace.refl, rfl, rfl⟩

/-- What ends a running managed command: its deadline, a cancel, or the
child's own exit with a zero or nonzero status. -/
inductive Trigger where
  | deadlineElapsed
  | cancelRequested
  | observeExitSuccess
  | observeExitFailure
  deriving DecidableEq, Repr

/-- One composed step for a trigger on a running tool and command. A deadline
or cancel signals the process tree (the `Transition` steps); an exit leaves
nothing to signal and settles the tool by the exit status. -/
def composedStep? (pre : ManagedExecComposedState) : Trigger → Option ManagedExecComposedState
  | .deadlineElapsed =>
      if pre.tool.state = .running ∧ pre.exec.state = .running ∧ pre.request.deadline < pre.now then
        some { pre with
          tool := { pre.tool with state := .timedOut }
        , exec := { pre.exec with state := .killSignaled, now := pre.now
                                  , killSignaledAt := some pre.now } }
      else none
  | .cancelRequested =>
      if pre.tool.state = .running ∧ pre.exec.state = .running then
        some { pre with
          tool := { pre.tool with state := .cancelled }
        , exec := { pre.exec with state := .killSignaled, now := pre.now
                                  , killSignaledAt := some pre.now } }
      else none
  | .observeExitSuccess =>
      if pre.tool.state = .running ∧ pre.exec.state = .running then
        some { pre with
          tool := { pre.tool with state := .completed }
        , exec := { pre.exec with state := .exited, exitCode := some 0 } }
      else none
  | .observeExitFailure =>
      if pre.tool.state = .running ∧ pre.exec.state = .running then
        some { pre with
          tool := { pre.tool with state := .failed }
        , exec := { pre.exec with state := .exited, exitCode := some 1 } }
      else none

theorem composedStep?_deadline_sound (pre post : ManagedExecComposedState)
    (h : composedStep? pre .deadlineElapsed = some post) : Transition pre post := by
  simp only [composedStep?] at h
  split at h
  · next hs => exact .deadlineElapsed hs.1 hs.2.1 hs.2.2 (Option.some.inj h).symm
  · contradiction

theorem composedStep?_cancel_sound (pre post : ManagedExecComposedState)
    (h : composedStep? pre .cancelRequested = some post) : Transition pre post := by
  simp only [composedStep?] at h
  split at h
  · next hs => exact .cancelRequested hs.1 hs.2 (Option.some.inj h).symm
  · contradiction

end ManagedExec
