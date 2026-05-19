import Proofs.Request
import Proofs.ToolExecution
import Proofs.ManagedExec.Properties

/-!
# Managed Exec Composition

Small composed state tying request deadline, tool lifecycle, and managed
executor kill-signaling into the #159 R3 operational theorem shape.
-/

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

end ManagedExec
