import Proofs.ToolExecution.Transition

/-!
# Executable Tool-Call Semantics

Executable actions, step function, and refinement theorem connecting
`step?` to the relational `Transition`. Mirrors `Proofs/Request/Executable.lean`.
-/

namespace ToolExecution
namespace ToolCallContext

/-- Executable tool-call actions mirroring the state-changing constructors of
    `Transition`. The two non-state constructors (`timeAdvance`,
    `persistenceStep`) are not exposed here; they are internal to trace
    construction in liveness proofs. -/
inductive Action where
  | dispatch
  | spawnFailed (failure : FailureClass)
  | complete
  | fail (failure : FailureClass)
  | timeout
  | cancelBeforeDispatch
  | cancelDuringRun
  deriving DecidableEq, Repr

/-- Executable transition function for the tool-call layer. -/
def step? (pre : ToolCallContext) : Action → Option ToolCallContext
  | .dispatch =>
      if pre.state = .pending then
        some { pre with state := .running, startedAt := some pre.currentTime }
      else
        none
  | .spawnFailed failure =>
      if pre.state = .pending then
        some { pre with state := .failed, failureClass := some failure }
      else
        none
  | .complete =>
      if pre.state = .running ∧ pre.persistence = .committed ∧ pre.childRequestId = none then
        some { pre with state := .completed }
      else
        none
  | .fail failure =>
      if pre.state = .running then
        some { pre with state := .failed, failureClass := some failure }
      else
        none
  | .timeout =>
      if pre.state = .running ∧ pre.deadlineExceeded then
        some { pre with state := .timedOut }
      else
        none
  | .cancelBeforeDispatch =>
      if pre.state = .pending then
        some { pre with state := .cancelled }
      else
        none
  | .cancelDuringRun =>
      if pre.state = .running then
        some { pre with state := .cancelled }
      else
        none

/-- Refinement: every successful `step?` corresponds to a relational `Transition`. -/
theorem step_refines_transition
    (pre : ToolCallContext) (a : Action) (post : ToolCallContext) :
    step? pre a = some post → Transition pre post := by
  intro h_step
  cases a with
  | dispatch =>
      simp [step?] at h_step
      rcases h_step with ⟨h_state, h_post⟩
      exact Transition.dispatch (h_state := h_state) (h_post := h_post.symm)
  | spawnFailed failure =>
      simp [step?] at h_step
      rcases h_step with ⟨h_state, h_post⟩
      exact Transition.spawnFailed failure (h_state := h_state) (h_post := h_post.symm)
  | complete =>
      simp [step?] at h_step
      rcases h_step with ⟨⟨h_state, h_persist, h_native⟩, h_post⟩
      exact Transition.complete (h_state := h_state) (h_persist := h_persist) (h_native := h_native) (h_post := h_post.symm)
  | fail failure =>
      simp [step?] at h_step
      rcases h_step with ⟨h_state, h_post⟩
      exact Transition.fail failure (h_state := h_state) (h_post := h_post.symm)
  | timeout =>
      simp [step?] at h_step
      rcases h_step with ⟨⟨h_state, h_deadline⟩, h_post⟩
      exact Transition.timeout (h_state := h_state) (h_deadline := h_deadline) (h_post := h_post.symm)
  | cancelBeforeDispatch =>
      simp [step?] at h_step
      rcases h_step with ⟨h_state, h_post⟩
      exact Transition.cancelBeforeDispatch (h_state := h_state) (h_post := h_post.symm)
  | cancelDuringRun =>
      simp [step?] at h_step
      rcases h_step with ⟨h_state, h_post⟩
      exact Transition.cancelDuringRun (h_state := h_state) (h_post := h_post.symm)

end ToolCallContext
end ToolExecution
