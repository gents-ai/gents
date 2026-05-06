import Proofs.InferenceCall.State

/-!
# Inference Call Transitions

Relational semantics for a single `InferenceCall` row.
-/

namespace InferenceCall

/-- Cancellation-only transitions.

The `cancelled` call state is a terminal vocabulary value used by more than
request interrupts. Cross-layer theorems add request-interrupt preconditions
where they need that stronger meaning. -/
inductive CancellationTransition : InferenceCall → InferenceCall → Prop where
  | cancel_before_stream {pre post : InferenceCall} :
      pre.state = .queued →
      post = pre.cancel →
      CancellationTransition pre post
  | cancel_during_stream {pre post : InferenceCall} :
      pre.state = .running →
      post = pre.cancel →
      CancellationTransition pre post

/-- All legal inference-call lifecycle transitions. -/
inductive Transition : InferenceCall → InferenceCall → Prop where
  | start {pre post : InferenceCall} :
      pre.state = .queued →
      post = { pre with state := .running } →
      Transition pre post
  | complete {pre post : InferenceCall} :
      pre.state = .running →
      post = { pre with state := .completed } →
      Transition pre post
  | fail {pre post : InferenceCall} :
      pre.state = .running →
      post = { pre with state := .failed } →
      Transition pre post
  | cancel {pre post : InferenceCall} :
      CancellationTransition pre post →
      Transition pre post

/-- A trace is a sequence of valid call transitions. -/
inductive Trace : InferenceCall → InferenceCall → Prop where
  | refl {s : InferenceCall} : Trace s s
  | step {s₁ s₂ s₃ : InferenceCall} :
      Transition s₁ s₂ → Trace s₂ s₃ → Trace s₁ s₃

theorem cancel_before_stream_transition
    {pre post : InferenceCall}
    (h_state : pre.state = .queued)
    (h_post : post = pre.cancel) :
    Transition pre post :=
  Transition.cancel (CancellationTransition.cancel_before_stream h_state h_post)

theorem cancel_during_stream_transition
    {pre post : InferenceCall}
    (h_state : pre.state = .running)
    (h_post : post = pre.cancel) :
    Transition pre post :=
  Transition.cancel (CancellationTransition.cancel_during_stream h_state h_post)

/-- Any live call has a one-step model path to `cancelled`. -/
theorem live_trace_to_cancelled
    (call : InferenceCall)
    (h_live : call.cancellable) :
    Trace call call.cancel := by
  cases h_live with
  | inl h_queued =>
      exact Trace.step
        (cancel_before_stream_transition h_queued rfl)
        Trace.refl
  | inr h_running =>
      exact Trace.step
        (cancel_during_stream_transition h_running rfl)
        Trace.refl

end InferenceCall
