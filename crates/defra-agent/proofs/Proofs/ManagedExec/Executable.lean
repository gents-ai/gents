import Proofs.ManagedExec.Transition

/-!
# Executable Managed Exec Semantics

Finite action surface for conformance generation.
-/

namespace ManagedExecContext

inductive Action where
  | spawn
  | spawnFailed
  | observeExitSuccess (code : Int)
  | observeExitFailure (code : Int)
  | deadlineElapsed
  | cancelRequested
  | killObserved
  | reapFailed
  deriving DecidableEq, Repr

def step? (pre : ManagedExecContext) : Action → Option ManagedExecContext
  | .spawn =>
      if pre.state = .pendingSpawn then
        some { pre with state := .running }
      else
        none
  | .spawnFailed =>
      if pre.state = .pendingSpawn then
        some { pre with state := .spawnFailed }
      else
        none
  | .observeExitSuccess code =>
      if pre.state = .running then
        some { pre with state := .exited, exitCode := some code }
      else
        none
  | .observeExitFailure code =>
      if pre.state = .running then
        some { pre with state := .exited, exitCode := some code }
      else
        none
  | .deadlineElapsed =>
      if pre.state = .running ∧ pre.deadlineExceeded then
        some { pre with state := .killSignaled, killSignaledAt := some pre.now }
      else
        none
  | .cancelRequested =>
      if pre.state = .running then
        some { pre with state := .killSignaled, killSignaledAt := some pre.now }
      else
        none
  | .killObserved =>
      if pre.state = .killSignaled then
        some { pre with state := .killed }
      else
        none
  | .reapFailed =>
      if pre.state = .killSignaled then
        some { pre with state := .reapFailed }
      else
        none

theorem step_refines_transition
    (pre : ManagedExecContext) (a : Action) (post : ManagedExecContext) :
    step? pre a = some post → Transition pre post := by
  intro h_step
  cases a with
  | spawn =>
      simp [step?] at h_step
      rcases h_step with ⟨h_state, h_post⟩
      exact Transition.spawn (h_state := h_state) (h_post := h_post.symm)
  | spawnFailed =>
      simp [step?] at h_step
      rcases h_step with ⟨h_state, h_post⟩
      exact Transition.spawnFailed (h_state := h_state) (h_post := h_post.symm)
  | observeExitSuccess code =>
      simp [step?] at h_step
      rcases h_step with ⟨h_state, h_post⟩
      exact Transition.observeExitSuccess code (h_state := h_state) (h_post := h_post.symm)
  | observeExitFailure code =>
      simp [step?] at h_step
      rcases h_step with ⟨h_state, h_post⟩
      exact Transition.observeExitFailure code (h_state := h_state) (h_post := h_post.symm)
  | deadlineElapsed =>
      simp [step?] at h_step
      rcases h_step with ⟨⟨h_state, h_deadline⟩, h_post⟩
      exact Transition.deadlineElapsed
        (h_state := h_state)
        (h_deadline := h_deadline)
        (h_post := h_post.symm)
  | cancelRequested =>
      simp [step?] at h_step
      rcases h_step with ⟨h_state, h_post⟩
      exact Transition.cancelRequested (h_state := h_state) (h_post := h_post.symm)
  | killObserved =>
      simp [step?] at h_step
      rcases h_step with ⟨h_state, h_post⟩
      exact Transition.killObserved (h_state := h_state) (h_post := h_post.symm)
  | reapFailed =>
      simp [step?] at h_step
      rcases h_step with ⟨h_state, h_post⟩
      exact Transition.reapFailed (h_state := h_state) (h_post := h_post.symm)

end ManagedExecContext
