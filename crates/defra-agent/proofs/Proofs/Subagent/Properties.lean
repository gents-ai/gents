import Proofs.Subagent.Transition

/-!
# Subagent Properties

B1–B6 plus the structural invariants INV-DEPTH and INV-LINK.

This file accumulates: the depth-bound and link-symmetry invariants here,
then B1–B6 are added in subsequent tasks (16-19).
-/

namespace Subagent
namespace BridgedState

/-! ### Helper lemmas: lineage-field preservation across composed transitions

The bridged invariants reduce to: `subagentDepth`, `causedByParentRequestId`,
and `causedByParentToolCallId` are preserved across any inner
`ComposedState.Transition`. None of the request- or persistence-layer
constructors touch these fields (they are submitter/spawn-set; the runtime
treats them read-only after `bridge_spawn`). -/

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
  | persistence_step _ _ _ h_req _ _ _ _ => rw [h_req]
  | call_step _ h_req _ _ _ => rw [h_req]
  | tool_step _ _ _ h_req _ _ _ _ => rw [h_req]

/-- Any ComposedState transition preserves the request's `causedByParentRequestId`. -/
private theorem composed_causedByParentRequestId_preserved
    {pre post : ComposedState}
    (h : ComposedState.Transition pre post) :
    post.request.causedByParentRequestId = pre.request.causedByParentRequestId := by
  cases h with
  | process_step _ h_req _ _ _ => rw [h_req]
  | request_step h_inner _ _ _ _ _ _ =>
    exact request_causedByParentRequestId_preserved h_inner
  | persistence_step _ _ _ h_req _ _ _ _ => rw [h_req]
  | call_step _ h_req _ _ _ => rw [h_req]
  | tool_step _ _ _ h_req _ _ _ _ => rw [h_req]

/-- Any ComposedState transition preserves the request's `causedByParentToolCallId`. -/
private theorem composed_causedByParentToolCallId_preserved
    {pre post : ComposedState}
    (h : ComposedState.Transition pre post) :
    post.request.causedByParentToolCallId = pre.request.causedByParentToolCallId := by
  cases h with
  | process_step _ h_req _ _ _ => rw [h_req]
  | request_step h_inner _ _ _ _ _ _ =>
    exact request_causedByParentToolCallId_preserved h_inner
  | persistence_step _ _ _ h_req _ _ _ _ => rw [h_req]
  | call_step _ h_req _ _ _ => rw [h_req]
  | tool_step _ _ _ h_req _ _ _ _ => rw [h_req]

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
  | bridge_spawn _ h_depth _ h_post_child h_request_eq _ =>
    refine ⟨?_, ?_⟩
    · rw [h_request_eq]; exact h_init.1
    · rw [h_post_child.2.2.2]; exact h_depth
  | bridge_complete _ _ _ _ _ h_request_eq h_child_eq _ _ =>
    refine ⟨?_, ?_⟩
    · rw [h_request_eq]; exact h_init.1
    · rw [h_child_eq]; exact h_init.2
  | bridge_failure _ _ _ _ h_request_eq h_child_eq _ _ =>
    refine ⟨?_, ?_⟩
    · rw [h_request_eq]; exact h_init.1
    · rw [h_child_eq]; exact h_init.2
  | bridge_cancel_cascade _ _ _ h_parent_eq _ _ _ _ h_child_depth_eq =>
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
  | bridge_spawn _ _ h_post_parent_tool h_post_child _ h_parent_id_eq =>
    refine ⟨?_, ?_, ?_⟩
    · -- parentLink: the new bridge tool with t.callId = post.bridgeCallId ∧
      -- t.childRequestId = some post.child.requestId is exactly what we need.
      obtain ⟨t, h_in, h_id, _h_state, h_child⟩ := h_post_parent_tool
      exact ⟨t, h_in, h_id, h_child⟩
    · -- childLink.causedByParentRequestId: post.child.request.cBPRId =
      -- some pre.parent.requestId, and pre.parent.requestId =
      -- post.parent.requestId via h_parent_id_eq.
      rw [h_post_child.2.1, h_parent_id_eq]
    · exact h_post_child.2.2.1
  | bridge_complete _ _ _ h_post_tool h_others_eq h_request_eq h_child_eq h_bridgeId_eq h_parent_id_eq =>
    obtain ⟨h_pLink, h_cLink⟩ := h_init
    refine ⟨?_, ?_, ?_⟩
    · -- parentLink: h_post_tool now provides t.childRequestId =
      -- some pre.child.requestId. Combined with h_child_eq we have
      -- pre.child.requestId = post.child.requestId.
      obtain ⟨t, h_in, h_id, _h_state, h_child⟩ := h_post_tool
      refine ⟨t, h_in, ?_, ?_⟩
      · rw [h_id, h_bridgeId_eq]
      · rw [h_child, h_child_eq]
    · -- childLink.causedByParentRequestId via h_child_eq + h_parent_id_eq.
      rw [h_child_eq, h_parent_id_eq]; exact h_cLink.1
    · rw [h_child_eq, h_bridgeId_eq]; exact h_cLink.2
  | bridge_failure _ _ h_post_tool h_others_eq h_request_eq h_child_eq h_bridgeId_eq h_parent_id_eq =>
    obtain ⟨h_pLink, h_cLink⟩ := h_init
    refine ⟨?_, ?_, ?_⟩
    · obtain ⟨t, h_in, h_id, _h_state, h_child⟩ := h_post_tool
      refine ⟨t, h_in, ?_, ?_⟩
      · rw [h_id, h_bridgeId_eq]
      · rw [h_child, h_child_eq]
    · rw [h_child_eq, h_parent_id_eq]; exact h_cLink.1
    · rw [h_child_eq, h_bridgeId_eq]; exact h_cLink.2
  | bridge_cancel_cascade _ _ _ h_parent_eq h_bridgeId_eq h_child_id_eq h_child_cBPR_eq h_child_cBPT_eq _ =>
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

/-! ### B1: child completion propagates to parent ToolCall completion -/

/-- B1: A child Request reaching `.completed` propagates to the parent
    ToolCall reaching `.completed` along a single `bridge_complete` step.

    The bridge tool hypothesis bundles three facts about the running bridge
    tool: it carries the bridge callId, it is currently `.running`, and its
    `childRequestId` matches `pre.child.requestId`. The last conjunct is
    morally part of `pre.linked`'s `parentLink`, but baking it directly into
    the running-tool witness avoids a callId-uniqueness side condition: we
    need to step the *same* tool for which we know the child link. -/
theorem bridged_child_completion_propagates
    (pre : BridgedState)
    (h_running     : ∃ t ∈ pre.parent.tools,
                       t.callId = pre.bridgeCallId ∧
                       t.state = .running ∧
                       t.childRequestId = some pre.child.requestId)
    (h_persisted   : ∃ t ∈ pre.parent.tools,
                       t.callId = pre.bridgeCallId ∧
                       t.persistence = .committed)
    (h_child_done  : pre.child.request.state = .completed) :
    ∃ post, Trace pre post ∧
            ∃ t ∈ post.parent.tools,
              t.callId = pre.bridgeCallId ∧ t.state = .completed := by
  -- Pull out the running bridge tool and its index in pre.parent.tools.
  obtain ⟨tPre, h_in, h_id, h_run_state, h_child_id⟩ := h_running
  obtain ⟨idx, h_idx⟩ := List.mem_iff_getElem?.mp h_in
  -- The post-state advances the running bridge tool to `.completed`,
  -- preserving every other field (including `childRequestId`).
  let tPost : ToolExecution.ToolCallContext := { tPre with state := .completed }
  let postParent : ComposedState :=
    { pre.parent with tools := pre.parent.tools.set idx tPost }
  let post : BridgedState := { pre with parent := postParent }
  -- We need the index bound for `List.mem_set`.
  have h_lt : idx < pre.parent.tools.length :=
    (List.getElem?_eq_some_iff.mp h_idx).1
  -- Recover `h_running` for the constructor (un-destructured form).
  have h_running' : ∃ t ∈ pre.parent.tools,
                       t.callId = pre.bridgeCallId ∧ t.state = .running :=
    ⟨tPre, h_in, h_id, h_run_state⟩
  -- The post bridge tool exists in post.parent.tools (it's the .set element).
  have h_tPost_in : tPost ∈ postParent.tools :=
    List.mem_set pre.parent.tools idx h_lt tPost
  -- Provide the exhibit for the post existential.
  refine ⟨post, ?_, tPost, ?_, ?_, rfl⟩
  · -- One-step trace via bridge_complete.
    refine Trace.step ?_ Trace.refl
    refine Transition.bridge_complete
      h_child_done
      h_running'
      h_persisted
      ?_   -- h_post_tool
      ?_   -- h_others_eq
      rfl  -- h_request_eq      (post.parent.request = pre.parent.request)
      rfl  -- h_child_eq        (post.child = pre.child)
      rfl  -- h_bridgeId_eq     (post.bridgeCallId = pre.bridgeCallId)
      rfl  -- h_parent_id_eq    (post.parent.requestId = pre.parent.requestId)
    · -- h_post_tool: tPost ∈ post.parent.tools, callId, state = .completed,
      --              childRequestId = some pre.child.requestId.
      refine ⟨tPost, h_tPost_in, ?_, rfl, ?_⟩
      · -- tPost.callId = pre.bridgeCallId  (tPost shares tPre's callId).
        show tPre.callId = pre.bridgeCallId
        exact h_id
      · -- tPost.childRequestId = some pre.child.requestId.
        show tPre.childRequestId = some pre.child.requestId
        exact h_child_id
    · -- h_others_eq: every non-bridge tool in pre is preserved in post.
      intro t h_t_in h_t_ne
      -- post.parent.tools = pre.parent.tools.set idx tPost. A non-bridge tool
      -- t ≠ tPre (because tPre.callId = pre.bridgeCallId and t.callId ≠
      -- pre.bridgeCallId). So t survives the .set unchanged.
      show t ∈ postParent.tools
      have h_set_subset :
          ∀ x ∈ pre.parent.tools, x ≠ tPre →
            x ∈ pre.parent.tools.set idx tPost := by
        intro x hx hx_ne
        -- x is in the original list; either its index = idx (then x = tPre,
        -- contradicting hx_ne) or its index ≠ idx (then .set preserves it).
        rcases List.mem_iff_getElem?.mp hx with ⟨j, hj⟩
        by_cases h_eq : j = idx
        · -- j = idx ⇒ x = tPre via h_idx, hj.
          subst h_eq
          have : some x = some tPre := by rw [← hj, h_idx]
          exact absurd (Option.some.inj this) hx_ne
        · -- j ≠ idx ⇒ x ∈ pre.parent.tools.set idx tPost.
          apply List.mem_iff_getElem?.mpr
          refine ⟨j, ?_⟩
          rw [List.getElem?_set_ne (by exact fun h => h_eq h.symm)]
          exact hj
      apply h_set_subset t h_t_in
      intro h_eq
      apply h_t_ne
      rw [h_eq]; exact h_id
  · -- Bridge tool ∈ post.parent.tools.
    exact h_tPost_in

end BridgedState
end Subagent
