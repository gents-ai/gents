import Proofs.ToolExecution.State

/-!
# Tool Call Transitions

Relational transition system for `ToolCallContext`. Seven state-changing
constructors plus two non-state constructors (`timeAdvance`, `persistenceStep`).
-/

namespace ToolExecution
namespace ToolCallContext

inductive Transition : ToolCallContext → ToolCallContext → Prop where

  | dispatch {pre post : ToolCallContext}
      (h_state : pre.state = .pending)
      (h_post  : post = { pre with state := .running
                                 , startedAt := some pre.currentTime })
      : Transition pre post

  | spawnFailed {pre post : ToolCallContext} (failure : FailureClass)
      (h_state : pre.state = .pending)
      (h_post  : post = { pre with state := .failed
                                 , failureClass := some failure })
      : Transition pre post

  | complete {pre post : ToolCallContext}
      (h_state   : pre.state = .running)
      (h_persist : pre.persistence = .committed)
      (h_post    : post = { pre with state := .completed })
      : Transition pre post

  | fail {pre post : ToolCallContext} (failure : FailureClass)
      (h_state : pre.state = .running)
      (h_post  : post = { pre with state := .failed
                                 , failureClass := some failure })
      : Transition pre post

  | timeout {pre post : ToolCallContext}
      (h_state    : pre.state = .running)
      (h_deadline : pre.deadlineExceeded)
      (h_post     : post = { pre with state := .timedOut })
      : Transition pre post

  | cancelBeforeDispatch {pre post : ToolCallContext}
      (h_state : pre.state = .pending)
      (h_post  : post = { pre with state := .cancelled })
      : Transition pre post

  | cancelDuringRun {pre post : ToolCallContext}
      (h_state : pre.state = .running)
      (h_post  : post = { pre with state := .cancelled })
      : Transition pre post

  | timeAdvance {pre post : ToolCallContext} (t : Time)
      (h_le   : pre.currentTime ≤ t)
      (h_post : post = { pre with currentTime := t })
      : Transition pre post

  | persistenceStep {pre post : ToolCallContext}
      (policy : PersistenceState.FailurePolicy)
      (next : PersistenceState)
      (h_p_step : PersistenceState.Transition policy pre.persistence next)
      (h_post   : post = { pre with persistence := next })
      : Transition pre post

/-- A trace is a sequence of valid tool-call transitions. -/
inductive Trace : ToolCallContext → ToolCallContext → Prop where
  | refl {c : ToolCallContext} : Trace c c
  | step {c₁ c₂ c₃ : ToolCallContext} :
      Transition c₁ c₂ → Trace c₂ c₃ → Trace c₁ c₃

end ToolCallContext
end ToolExecution
