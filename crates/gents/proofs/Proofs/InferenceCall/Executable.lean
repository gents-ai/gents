import Proofs.InferenceCall.Transition

/-!
# Executable Inference Call Semantics

Executable actions, step function, replay, and equivalence with the relational
transition model for a single persisted `InferenceCall` row.
-/

namespace InferenceCall

/-- Executable inference-call actions mirroring `Transition`. -/
inductive Action where
  | start
  | complete
  | fail
  | cancel
  deriving DecidableEq, Repr

/-- Executable transition function for the inference-call layer. -/
def step? (pre : InferenceCall) : Action → Option InferenceCall
  | .start =>
      if pre.state = .queued then
        some { pre with state := .running }
      else
        none
  | .complete =>
      if pre.state = .running then
        some { pre with state := .completed }
      else
        none
  | .fail =>
      if pre.state = .running then
        some { pre with state := .failed }
      else
        none
  | .cancel =>
      if pre.state = .queued ∨ pre.state = .running then
        some pre.cancel
      else
        none

/-- Replay a finite action list through the executable call semantics. -/
def replay? : InferenceCall → List Action → Option InferenceCall
  | s, [] => some s
  | s, action :: rest =>
      match step? s action with
      | some s' => replay? s' rest
      | none => none

theorem step_sound
    {pre post : InferenceCall}
    {action : Action}
    (h_step : step? pre action = some post) :
    Transition pre post := by
  cases action with
  | start =>
      simp [step?] at h_step
      rcases h_step with ⟨h_state, h_post⟩
      exact Transition.start h_state h_post.symm
  | complete =>
      simp [step?] at h_step
      rcases h_step with ⟨h_state, h_post⟩
      exact Transition.complete h_state h_post.symm
  | fail =>
      simp [step?] at h_step
      rcases h_step with ⟨h_state, h_post⟩
      exact Transition.fail h_state h_post.symm
  | cancel =>
      simp [step?] at h_step
      rcases h_step with ⟨h_live, h_post⟩
      cases h_live with
      | inl h_queued =>
          exact Transition.cancel
            (CancellationTransition.cancel_before_stream h_queued h_post.symm)
      | inr h_running =>
          exact Transition.cancel
            (CancellationTransition.cancel_during_stream h_running h_post.symm)

theorem transition_complete
    {pre post : InferenceCall}
    (h_trans : Transition pre post) :
    ∃ action : Action, step? pre action = some post := by
  cases h_trans with
  | start h_state h_post =>
      exact ⟨.start, by simp [step?, h_state, h_post]⟩
  | complete h_state h_post =>
      exact ⟨.complete, by simp [step?, h_state, h_post]⟩
  | fail h_state h_post =>
      exact ⟨.fail, by simp [step?, h_state, h_post]⟩
  | cancel h_cancel =>
      cases h_cancel with
      | cancel_before_stream h_state h_post =>
          exact ⟨.cancel, by simp [step?, h_state, h_post]⟩
      | cancel_during_stream h_state h_post =>
          exact ⟨.cancel, by simp [step?, h_state, h_post]⟩

theorem replay_sound
    {pre post : InferenceCall}
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
    {pre post : InferenceCall}
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

end InferenceCall
