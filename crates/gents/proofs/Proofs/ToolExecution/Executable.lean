import Proofs.ToolExecution.Transition

namespace ToolExecution
namespace ToolCallContext

inductive Action where
  | dispatch
  | spawnFailed (failure : FailureClass)
  | complete
  | fail (failure : FailureClass)
  | timeout
  | background
  | foreground
  | detach
  | cancelBeforeDispatch (cause : CancelCause)
  | cancelDuringRun (cause : CancelCause)
  deriving DecidableEq, Repr

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
  | .background =>
      if pre.state = .running ∧ pre.awaitMode = .foreground then
        some { pre with awaitMode := .background }
      else
        none
  | .foreground =>
      if pre.state = .running ∧ pre.awaitMode = .background then
        some { pre with awaitMode := .foreground }
      else
        none
  | .detach =>
      if (pre.state = .pending ∨ pre.state = .running) ∧
          pre.cancelPolicy = .cascade then
        some { pre with cancelPolicy := .detach }
      else
        none
  | .cancelBeforeDispatch _ =>
      if pre.state = .pending then
        some { pre with state := .cancelled }
      else
        none
  | .cancelDuringRun _ =>
      if pre.state = .running then
        some { pre with state := .cancelled }
      else
        none

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
  | background =>
      simp [step?] at h_step
      rcases h_step with ⟨⟨h_state, h_mode⟩, h_post⟩
      exact Transition.background (h_state := h_state) (h_mode := h_mode)
        (h_post := h_post.symm)
  | foreground =>
      simp [step?] at h_step
      rcases h_step with ⟨⟨h_state, h_mode⟩, h_post⟩
      exact Transition.foreground (h_state := h_state) (h_mode := h_mode)
        (h_post := h_post.symm)
  | detach =>
      simp [step?] at h_step
      rcases h_step with ⟨⟨h_live, h_policy⟩, h_post⟩
      exact Transition.detach (h_live := h_live) (h_pol := h_policy)
        (h_post := h_post.symm)
  | cancelBeforeDispatch cause =>
      simp [step?] at h_step
      rcases h_step with ⟨h_state, h_post⟩
      exact Transition.cancelBeforeDispatch cause (h_state := h_state) (h_post := h_post.symm)
  | cancelDuringRun cause =>
      simp [step?] at h_step
      rcases h_step with ⟨h_state, h_post⟩
      exact Transition.cancelDuringRun cause (h_state := h_state) (h_post := h_post.symm)

end ToolCallContext
end ToolExecution
