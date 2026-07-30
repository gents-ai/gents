import Proofs.ManagedExec.State

namespace ManagedExecContext

inductive Transition : ManagedExecContext → ManagedExecContext → Prop where
  | spawn {pre post : ManagedExecContext}
      (h_state : pre.state = .pendingSpawn)
      (h_post : post = { pre with state := .running })
      : Transition pre post

  | spawnFailed {pre post : ManagedExecContext}
      (h_state : pre.state = .pendingSpawn)
      (h_post : post = { pre with state := .spawnFailed })
      : Transition pre post

  | observeExitSuccess {pre post : ManagedExecContext} (code : Int)
      (h_state : pre.state = .running)
      (h_post : post = { pre with state := .exited, exitCode := some code })
      : Transition pre post

  | observeExitFailure {pre post : ManagedExecContext} (code : Int)
      (h_state : pre.state = .running)
      (h_post : post = { pre with state := .exited, exitCode := some code })
      : Transition pre post

  | deadlineElapsed {pre post : ManagedExecContext}
      (h_state : pre.state = .running)
      (h_deadline : pre.deadlineExceeded)
      (h_post : post = { pre with state := .killSignaled
                                , killSignaledAt := some pre.now })
      : Transition pre post

  | cancelRequested {pre post : ManagedExecContext}
      (h_state : pre.state = .running)
      (h_post : post = { pre with state := .killSignaled
                                , killSignaledAt := some pre.now })
      : Transition pre post

  | killObserved {pre post : ManagedExecContext}
      (h_state : pre.state = .killSignaled)
      (h_post : post = { pre with state := .killed })
      : Transition pre post

  | reapFailed {pre post : ManagedExecContext}
      (h_state : pre.state = .killSignaled)
      (h_post : post = { pre with state := .reapFailed })
      : Transition pre post

  | timeAdvance {pre post : ManagedExecContext} (t : Time)
      (h_le : pre.now ≤ t)
      (h_post : post = { pre with now := t })
      : Transition pre post

inductive Trace : ManagedExecContext → ManagedExecContext → Prop where
  | refl {c : ManagedExecContext} : Trace c c
  | step {c₁ c₂ c₃ : ManagedExecContext} :
      Transition c₁ c₂ → Trace c₂ c₃ → Trace c₁ c₃

inductive BoundedTrace : ManagedExecContext → ManagedExecContext → Nat → Prop where
  | refl {c : ManagedExecContext} : BoundedTrace c c 0
  | step {c₁ c₂ c₃ : ManagedExecContext} {n : Nat} :
      Transition c₁ c₂ → BoundedTrace c₂ c₃ n → BoundedTrace c₁ c₃ (n + 1)

end ManagedExecContext
