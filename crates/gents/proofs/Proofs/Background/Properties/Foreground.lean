import Proofs.Background.Properties.Structure

namespace Subagent
namespace BridgedState

theorem parent_transition_preserves_request_origin_and_message_sequence
    (pre post : BridgedState)
    (h_step   : Transition pre post) :
    pre.parent.request.origin = post.parent.request.origin ∧
    pre.parent.request.messageSeq  = post.parent.request.messageSeq := by
  cases h_step with
  | parent_step h_inner h_child_eq h_bridge_eq _ _ =>
    cases h_inner with
    | request_step h_req _ _ _ _ _ _ =>
      cases h_req with
      | bind_workspace _ _ h_post =>
        constructor <;> rw [h_post]
      | claim _ _ _ h_post =>
        constructor <;> rw [h_post]
      | dedup_lose _ _ h_post =>
        constructor <;> rw [h_post]
      | admission_reject _ _ h_post =>
        constructor <;> rw [h_post]
      | begin_inference h_pre_claimed _ h_post =>
        constructor <;> rw [h_post]
      | continue_processing _ _ h_post =>
        constructor <;> rw [h_post]
      | finish _ _ h_post =>
        constructor <;> rw [h_post]
      | fail _ _ h_post =>
        constructor <;> rw [h_post]
      | fail_before_stream _ _ h_post =>
        constructor <;> rw [h_post]
      | expire _ _ _ _ h_post =>
        constructor <;> rw [h_post]
      | interrupt_before_claim _ _ _ h_post =>
        constructor <;> rw [h_post]
      | interrupt_claimed _ _ _ h_post =>
        constructor <;> rw [h_post]
      | interrupt_processing _ _ _ h_post =>
        constructor <;> rw [h_post]
    | tool_step _ _ _ h_req_eq _ _ _ _ _ =>
      constructor <;> rw [h_req_eq]
    | process_step _ h_req _ _ _ =>
      constructor <;> rw [h_req]
    | slot_acquire _ _ h_req _ _ _ _ =>
      constructor <;> simp [h_req]
    | request_interrupt _ h_req _ _ _ _ =>
      constructor <;> simp [h_req]
    | clock_advance _ _ h_req _ _ _ _ =>
      constructor <;> simp [h_req]
    | persistence_step _ _ _ h_req _ _ _ _ =>
      constructor <;> rw [h_req]
    | call_step _ h_req _ _ _ =>
      constructor <;> rw [h_req]
    | tool_spawn _ _ _ h_req _ _ _ _ _ _ =>
      constructor <;> rw [h_req]
  | child_step _ h_parent_eq _ _ _ =>
    constructor <;> rw [h_parent_eq]
  | bridge_spawn _ _ _ _ _ _ _ _ h_request_eq _ _ =>
    constructor <;> rw [h_request_eq]
  | bridge_complete _ _ _ _ _ _ _ _ _ _ h_request_eq _ _ _ =>
    constructor <;> rw [h_request_eq]
  | bridge_failure _ _ _ _ _ _ _ _ _ h_request_eq _ _ _ =>
    constructor <;> rw [h_request_eq]
  | bridge_cancel_cascade _ _ _ h_parent_eq _ _ _ _ _ _ =>
    constructor <;> rw [h_parent_eq]

/-- The composed request-step guard rejects the owned loop's continuation
admission while a foreground tool is live. This is separate from generic
request identity preservation and does not claim that an admitted continuation
will be scheduled or produce output. -/
theorem foreground_blocks_processing_continuation_admission
    (pre post : ComposedState)
    (h_fg : ∃ t ∈ pre.tools, t.awaitMode = .foreground ∧ ¬ isTerminal t.state)
    (h_continue : RequestContext.step? pre.request .continueProcessing = some post.request)
    (h_no_block :
      ((pre.request.state = .processing ∧ post.request.state = .processing) ∨
        (pre.request.state = .claimed ∧ post.request.state = .processing) →
        ¬ ∃ t ∈ pre.tools, t.awaitMode = .foreground ∧ ¬ isTerminal t.state)) :
    False := by
  simp [RequestContext.step?] at h_continue
  rcases h_continue with ⟨⟨h_processing, _⟩, h_post⟩
  apply h_no_block
  · exact Or.inl ⟨h_processing, by simpa [← h_post] using h_processing⟩
  · exact h_fg

theorem steer_subagent_interrupt_preserves_link_symmetry
    {pre post : BridgedState}
    {childSessionId : SessionId}
    {queuePre queueDrained queuePost : SessionQueue.SessionQueueState}
    {transcriptPre transcriptPost : Transcript.TranscriptState}
    {childRequestId steeringRequestId : RequestId}
    {message : String}
    (h_step : SteerWithInterrupt
      pre post
      childSessionId
      queuePre queueDrained queuePost
      transcriptPre transcriptPost
      childRequestId steeringRequestId message)
    (h_pre  : pre.linked) :
    post.linked := by
  rcases h_step.h_bridge_compose with
    ⟨cascaded, interrupted, h_cascade, h_interrupt, _h_child_id, h_tail⟩
  have h_trace : Trace pre post :=
    Trace.step
      (BridgeCancelCascadeStep.to_transition h_cascade)
      (Trace.step (ChildInterruptStep.to_transition h_interrupt) h_tail)
  exact bridge_link_symmetric pre post h_pre h_trace

end BridgedState
end Subagent
