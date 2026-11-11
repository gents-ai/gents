import Proofs.InferenceCall.Transition

namespace InferenceCall

theorem transition_preserves_requestId
    {pre post : InferenceCall}
    (h_trans : Transition pre post) :
    post.requestId = pre.requestId := by
  cases h_trans with
  | start _ h_post =>
      rw [h_post]
  | complete _ h_post =>
      rw [h_post]
  | fail _ h_post =>
      rw [h_post]
  | cancel h_cancel =>
      cases h_cancel with
      | cancel_before_stream _ h_post =>
          rw [h_post]
          exact cancel_preserves_requestId pre
      | cancel_during_stream _ h_post =>
          rw [h_post]
          exact cancel_preserves_requestId pre

theorem transition_preserves_backend
    {pre post : InferenceCall}
    (h_trans : Transition pre post) :
    post.backend = pre.backend := by
  cases h_trans with
  | start _ h_post =>
      rw [h_post]
  | complete _ h_post =>
      rw [h_post]
  | fail _ h_post =>
      rw [h_post]
  | cancel h_cancel =>
      cases h_cancel with
      | cancel_before_stream _ h_post =>
          rw [h_post]
          exact cancel_preserves_backend pre
      | cancel_during_stream _ h_post =>
          rw [h_post]
          exact cancel_preserves_backend pre

theorem cancellation_transition_to_cancelled
    {pre post : InferenceCall}
    (h_cancel : CancellationTransition pre post) :
    post.state = .cancelled := by
  cases h_cancel with
  | cancel_before_stream _ h_post =>
      rw [h_post]
      exact cancel_state pre
  | cancel_during_stream _ h_post =>
      rw [h_post]
      exact cancel_state pre

theorem cancelled_has_no_outgoing
    {pre post : InferenceCall}
    (h_trans : Transition pre post)
    (h_cancelled : pre.state = .cancelled) :
    False := by
  cases h_trans with
  | start h_state _ =>
      rw [h_cancelled] at h_state
      cases h_state
  | complete h_state _ =>
      rw [h_cancelled] at h_state
      cases h_state
  | fail h_state _ =>
      rw [h_cancelled] at h_state
      cases h_state
  | cancel h_cancel =>
      cases h_cancel with
      | cancel_before_stream h_state _ =>
          rw [h_cancelled] at h_state
          cases h_state
      | cancel_during_stream h_state _ =>
          rw [h_cancelled] at h_state
          cases h_state

theorem cancelled_trace_stays_cancelled
    {pre post : InferenceCall}
    (h_cancelled : pre.state = .cancelled)
    (h_trace : Trace pre post) :
    post.state = .cancelled := by
  cases h_trace with
  | refl =>
      exact h_cancelled
  | step h_step _ =>
      exact False.elim (cancelled_has_no_outgoing h_step h_cancelled)

theorem cancelled_trace_not_running
    {pre post : InferenceCall}
    (h_cancelled : pre.state = .cancelled)
    (h_trace : Trace pre post) :
    post.state ≠ .running := by
  intro h_running
  have h_stays := cancelled_trace_stays_cancelled h_cancelled h_trace
  rw [h_running] at h_stays
  cases h_stays

theorem trace_preserves_requestId
    {pre post : InferenceCall}
    (h_trace : Trace pre post) :
    post.requestId = pre.requestId := by
  induction h_trace with
  | refl =>
      rfl
  | step h_step _ ih =>
      exact Eq.trans ih (transition_preserves_requestId h_step)

theorem trace_preserves_backend
    {pre post : InferenceCall}
    (h_trace : Trace pre post) :
    post.backend = pre.backend := by
  induction h_trace with
  | refl =>
      rfl
  | step h_step _ ih =>
      exact Eq.trans ih (transition_preserves_backend h_step)

end InferenceCall
