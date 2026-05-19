import Proofs.ManagedExec.Executable

/-!
# Managed Exec Liveness Properties
-/

namespace ManagedExecContext

def maxKillSignalSteps : Nat := 1

theorem deadline_running_exec_reaches_kill_signaled
    (pre : ManagedExecContext)
    (h_running : pre.state = .running)
    (h_deadline : pre.deadline < pre.now) :
    exists post,
      BoundedTrace pre post maxKillSignalSteps
      ∧ post.state = .killSignaled
      ∧ post.killSignaledAt = some pre.now := by
  let post : ManagedExecContext :=
    { pre with state := .killSignaled, killSignaledAt := some pre.now }
  have h_step : Transition pre post :=
    Transition.deadlineElapsed
      (h_state := h_running)
      (h_deadline := h_deadline)
      (h_post := rfl)
  exact ⟨post, BoundedTrace.step h_step BoundedTrace.refl, rfl, rfl⟩

theorem cancel_running_exec_reaches_kill_signaled
    (pre : ManagedExecContext)
    (h_running : pre.state = .running) :
    exists post,
      BoundedTrace pre post maxKillSignalSteps
      ∧ post.state = .killSignaled
      ∧ post.killSignaledAt = some pre.now := by
  let post : ManagedExecContext :=
    { pre with state := .killSignaled, killSignaledAt := some pre.now }
  have h_step : Transition pre post :=
    Transition.cancelRequested
      (h_state := h_running)
      (h_post := rfl)
  exact ⟨post, BoundedTrace.step h_step BoundedTrace.refl, rfl, rfl⟩

theorem signaled_executor_reaches_terminal
    (pre : ManagedExecContext)
    (h_signaled : pre.state = .killSignaled) :
    exists post,
      BoundedTrace pre post maxKillSignalSteps
      ∧ (post.state = .killed ∨ post.state = .reapFailed) := by
  let post : ManagedExecContext := { pre with state := .killed }
  have h_step : Transition pre post :=
    Transition.killObserved (h_state := h_signaled) (h_post := rfl)
  exact ⟨post, BoundedTrace.step h_step BoundedTrace.refl, Or.inl rfl⟩

end ManagedExecContext
