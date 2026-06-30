import Proofs.Background.Transition

/-! Structural bridge invariants for subagent depth, link symmetry, and lineage-field preservation. -/

namespace Subagent
namespace BridgedState

/-! ### Helper lemmas: lineage-field preservation across composed transitions

The bridged invariants reduce to: `subagentDepth`, `causedByParentRequestId`,
and `causedByParentToolCallId` are preserved across any inner
`ComposedState.Transition`. None of the request-, admission-, clock-, or
persistence-layer constructors touch these fields (they are submitter/spawn-set;
the runtime treats them read-only after `bridge_spawn`). -/

private theorem request_subagentDepth_preserved
    {pre post : RequestContext}
    (h : RequestContext.Transition pre post) :
    post.subagentDepth = pre.subagentDepth := by
  cases h <;> simp_all

private theorem request_causedByParentRequestId_preserved
    {pre post : RequestContext}
    (h : RequestContext.Transition pre post) :
    post.causedByParentRequestId = pre.causedByParentRequestId := by
  cases h <;> simp_all

private theorem request_causedByParentToolCallId_preserved
    {pre post : RequestContext}
    (h : RequestContext.Transition pre post) :
    post.causedByParentToolCallId = pre.causedByParentToolCallId := by
  cases h <;> simp_all

/-- Any ComposedState transition preserves the request's `subagentDepth`. -/
private theorem composed_subagentDepth_preserved
    {pre post : ComposedState}
    (h : ComposedState.Transition pre post) :
    post.request.subagentDepth = pre.request.subagentDepth := by
  cases h with
  | process_step _ h_req _ _ _ => rw [h_req]
  | request_step h_inner _ _ _ _ _ _ =>
    exact request_subagentDepth_preserved h_inner
  | slot_acquire _ _ h_req _ _ _ _ => simp [h_req]
  | request_interrupt _ h_req _ _ _ _ => simp [h_req]
  | clock_advance _ _ h_req _ _ _ _ => simp [h_req]
  | persistence_step _ _ _ h_req _ _ _ _ => rw [h_req]
  | call_step _ h_req _ _ _ => rw [h_req]
  | tool_spawn _ _ _ h_req _ _ _ _ _ _ => rw [h_req]
  | tool_step _ _ _ h_req _ _ _ _ _ => rw [h_req]

/-- Any ComposedState transition preserves the request's `causedByParentRequestId`. -/
private theorem composed_causedByParentRequestId_preserved
    {pre post : ComposedState}
    (h : ComposedState.Transition pre post) :
    post.request.causedByParentRequestId = pre.request.causedByParentRequestId := by
  cases h with
  | process_step _ h_req _ _ _ => rw [h_req]
  | request_step h_inner _ _ _ _ _ _ =>
    exact request_causedByParentRequestId_preserved h_inner
  | slot_acquire _ _ h_req _ _ _ _ => simp [h_req]
  | request_interrupt _ h_req _ _ _ _ => simp [h_req]
  | clock_advance _ _ h_req _ _ _ _ => simp [h_req]
  | persistence_step _ _ _ h_req _ _ _ _ => rw [h_req]
  | call_step _ h_req _ _ _ => rw [h_req]
  | tool_spawn _ _ _ h_req _ _ _ _ _ _ => rw [h_req]
  | tool_step _ _ _ h_req _ _ _ _ _ => rw [h_req]

/-- Any ComposedState transition preserves the request's `causedByParentToolCallId`. -/
private theorem composed_causedByParentToolCallId_preserved
    {pre post : ComposedState}
    (h : ComposedState.Transition pre post) :
    post.request.causedByParentToolCallId = pre.request.causedByParentToolCallId := by
  cases h with
  | process_step _ h_req _ _ _ => rw [h_req]
  | request_step h_inner _ _ _ _ _ _ =>
    exact request_causedByParentToolCallId_preserved h_inner
  | slot_acquire _ _ h_req _ _ _ _ => simp [h_req]
  | request_interrupt _ h_req _ _ _ _ => simp [h_req]
  | clock_advance _ _ h_req _ _ _ _ => simp [h_req]
  | persistence_step _ _ _ h_req _ _ _ _ => rw [h_req]
  | call_step _ h_req _ _ _ => rw [h_req]
  | tool_spawn _ _ _ h_req _ _ _ _ _ _ => rw [h_req]
  | tool_step _ _ _ h_req _ _ _ _ _ => rw [h_req]

/-- Per-step preservation of INV-DEPTH: any single bridge transition preserves
    the depth bound on both parent and child. The trace-level theorem below
    threads this through `Trace.step`. -/
private theorem inv_depth_step
    {s₁ s₂ : BridgedState}
    (h_init : s₁.parent.request.subagentDepth ≤ maxSubagentDepth ∧
              s₁.child.request.subagentDepth ≤ maxSubagentDepth)
    (h_step : Transition s₁ s₂) :
    s₂.parent.request.subagentDepth ≤ maxSubagentDepth ∧
    s₂.child.request.subagentDepth ≤ maxSubagentDepth := by
  cases h_step with
  | parent_step h_inner h_child_eq _ _ _ =>
    refine ⟨?_, ?_⟩
    · rw [composed_subagentDepth_preserved h_inner]; exact h_init.1
    · rw [h_child_eq]; exact h_init.2
  | child_step h_inner h_parent_eq _ _ _ =>
    refine ⟨?_, ?_⟩
    · rw [h_parent_eq]; exact h_init.1
    · rw [composed_subagentDepth_preserved h_inner]; exact h_init.2
  | bridge_spawn _ h_depth _ _ _ _ h_post_child _ h_request_eq _ _ =>
    refine ⟨?_, ?_⟩
    · rw [h_request_eq]; exact h_init.1
    · rw [h_post_child.2.2.2.1]; exact h_depth
  | bridge_complete _ _ _ _ _ _ _ _ _ _ h_request_eq h_child_eq _ _ =>
    refine ⟨?_, ?_⟩
    · rw [h_request_eq]; exact h_init.1
    · rw [h_child_eq]; exact h_init.2
  | bridge_failure _ _ _ _ _ _ _ _ _ h_request_eq h_child_eq _ _ =>
    refine ⟨?_, ?_⟩
    · rw [h_request_eq]; exact h_init.1
    · rw [h_child_eq]; exact h_init.2
  | bridge_cancel_cascade _ _ _ h_parent_eq _ _ _ _ h_child_depth_eq _ =>
    refine ⟨?_, ?_⟩
    · rw [h_parent_eq]; exact h_init.1
    · rw [h_child_depth_eq]; exact h_init.2

/-- INV-DEPTH: subagent depth on both sides of the bridge stays ≤ maxSubagentDepth
    across any reachable trace. -/
theorem inv_depth
    (pre post : BridgedState)
    (h_init  : pre.parent.request.subagentDepth ≤ maxSubagentDepth ∧
               pre.child.request.subagentDepth ≤ maxSubagentDepth)
    (h_trace : Trace pre post) :
    post.parent.request.subagentDepth ≤ maxSubagentDepth ∧
    post.child.request.subagentDepth ≤ maxSubagentDepth := by
  induction h_trace with
  | refl => exact h_init
  | step h_step _ ih => exact ih (inv_depth_step h_init h_step)

/-- Per-step preservation of INV-LINK: any single bridge transition preserves
    parent-child link symmetry. -/
private theorem inv_link_step
    {s₁ s₂ : BridgedState}
    (h_init : s₁.linked)
    (h_step : Transition s₁ s₂) :
    s₂.linked := by
  cases h_step with
  | parent_step _ _ _ _ h_link_post => exact h_link_post
  | child_step _ _ _ _ h_link_post  => exact h_link_post
  | @bridge_spawn newTool _ _ h_newTool_callId _ h_newTool_child h_tools_append
                    h_post_child _ _ h_parent_id_eq _ =>
    refine ⟨?_, ?_, ?_⟩
    · -- parentLink: newTool ∈ post.parent.tools (via h_tools_append's append),
      -- with callId = post.bridgeCallId and childRequestId = some post.child.requestId.
      refine ⟨newTool, ?_, h_newTool_callId, h_newTool_child⟩
      rw [h_tools_append]
      exact List.mem_append.mpr (Or.inr (List.mem_singleton.mpr rfl))
    · -- childLink.causedByParentRequestId: post.child.request.cBPRId =
      -- some pre.parent.requestId, and pre.parent.requestId =
      -- post.parent.requestId via h_parent_id_eq.
      rw [h_post_child.2.1, h_parent_id_eq]
    · exact h_post_child.2.2.1
  | @bridge_complete idx _ tPost _ h_idx_pre _ _ _ _
                       h_post_callId _ h_post_child h_tools_set _ h_child_eq
                       h_bridgeId_eq h_parent_id_eq =>
    obtain ⟨_h_pLink, h_cLink⟩ := h_init
    refine ⟨?_, ?_, ?_⟩
    · -- parentLink: tPost ∈ post.parent.tools (via .set), and tPost has the
      -- bridge callId and childRequestId pointing at post.child.
      have h_lt : idx < s₁.parent.tools.length :=
        (List.getElem?_eq_some_iff.mp h_idx_pre).1
      have h_in : tPost ∈ s₂.parent.tools := by
        rw [h_tools_set]
        exact List.mem_set s₁.parent.tools idx h_lt tPost
      refine ⟨tPost, h_in, ?_, ?_⟩
      · rw [h_post_callId, h_bridgeId_eq]
      · rw [h_post_child, h_child_eq]
    · -- childLink.causedByParentRequestId via h_child_eq + h_parent_id_eq.
      rw [h_child_eq, h_parent_id_eq]; exact h_cLink.1
    · rw [h_child_eq, h_bridgeId_eq]; exact h_cLink.2
  | @bridge_failure idx _ tPost _ h_idx_pre _ _ _
                      h_post_callId _ h_post_child h_tools_set _ h_child_eq
                      h_bridgeId_eq h_parent_id_eq =>
    obtain ⟨_h_pLink, h_cLink⟩ := h_init
    refine ⟨?_, ?_, ?_⟩
    · have h_lt : idx < s₁.parent.tools.length :=
        (List.getElem?_eq_some_iff.mp h_idx_pre).1
      have h_in : tPost ∈ s₂.parent.tools := by
        rw [h_tools_set]
        exact List.mem_set s₁.parent.tools idx h_lt tPost
      refine ⟨tPost, h_in, ?_, ?_⟩
      · rw [h_post_callId, h_bridgeId_eq]
      · rw [h_post_child, h_child_eq]
    · rw [h_child_eq, h_parent_id_eq]; exact h_cLink.1
    · rw [h_child_eq, h_bridgeId_eq]; exact h_cLink.2
  | bridge_cancel_cascade _ _ _ h_parent_eq h_bridgeId_eq h_child_id_eq h_child_cBPR_eq h_child_cBPT_eq _ _ =>
    obtain ⟨h_pLink, h_cLink⟩ := h_init
    refine ⟨?_, ?_, ?_⟩
    · -- parentLink: post.parent = pre.parent, so unfold parentLink and
      -- rewrite back to pre.parent. Also rewrite post.bridgeCallId and
      -- post.child.requestId.
      show ∃ t ∈ s₂.parent.tools,
        t.callId = s₂.bridgeCallId ∧
        t.childRequestId = some s₂.child.requestId
      rw [h_parent_eq, h_bridgeId_eq, h_child_id_eq]; exact h_pLink
    · -- childLink.causedByParentRequestId: h_child_cBPR_eq says it's preserved
      -- from pre. h_parent_eq gives pre.parent.requestId = post.parent.requestId.
      show s₂.child.request.causedByParentRequestId = some s₂.parent.requestId
      rw [h_child_cBPR_eq, h_parent_eq]; exact h_cLink.1
    · show s₂.child.request.causedByParentToolCallId = some s₂.bridgeCallId
      rw [h_child_cBPT_eq, h_bridgeId_eq]; exact h_cLink.2

/-- INV-LINK: parent and child links stay symmetric across any reachable trace
    once initialized by `bridge_spawn`. -/
theorem inv_link
    (pre post : BridgedState)
    (h_init  : pre.linked)
    (h_trace : Trace pre post) :
    post.linked := by
  induction h_trace with
  | refl => exact h_init
  | step h_step _ ih => exact ih (inv_link_step h_init h_step)

end BridgedState
end Subagent
