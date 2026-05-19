import Proofs.Background.Transition

/-! Child terminal-state projection onto the parent bridge tool. -/

namespace Subagent
namespace BridgedState

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
                       t.persistence = .committed ∧
                       t.childRequestId = some pre.child.requestId)
    (h_second_done : pre.terminalOf = .completed) :
    ∃ post, Trace pre post ∧
            ∃ t ∈ post.parent.tools,
              t.callId = pre.bridgeCallId ∧ t.state = .completed := by
  -- Pull out the running bridge tool and its index in pre.parent.tools.
  obtain ⟨tPre, h_in, h_id, h_run_state, h_committed, h_child_id⟩ := h_running
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
  -- The post bridge tool exists in post.parent.tools (it's the .set element).
  have h_tPost_in : tPost ∈ postParent.tools :=
    List.mem_set pre.parent.tools idx h_lt tPost
  -- Provide the exhibit for the post existential.
  refine ⟨post, ?_, tPost, ?_, ?_, rfl⟩
  · -- One-step trace via bridge_complete.
    refine Trace.step ?_ Trace.refl
    refine Transition.bridge_complete
      (idx := idx) (tPre := tPre) (tPost := tPost)
      h_second_done
      h_idx
      h_id
      h_run_state
      h_committed
      h_child_id
      ?_   -- h_post_callId
      ?_   -- h_post_state
      ?_   -- h_post_child
      rfl  -- h_tools_set       (post.parent.tools = pre.parent.tools.set idx tPost)
      rfl  -- h_request_eq      (post.parent.request = pre.parent.request)
      rfl  -- h_child_eq        (post.child = pre.child)
      rfl  -- h_bridgeId_eq     (post.bridgeCallId = pre.bridgeCallId)
      rfl  -- h_parent_id_eq    (post.parent.requestId = pre.parent.requestId)
    · -- h_post_callId: tPost.callId = pre.bridgeCallId.
      show tPre.callId = pre.bridgeCallId
      exact h_id
    · -- h_post_state: tPost.state = .completed.
      rfl
    · -- h_post_child: tPost.childRequestId = some pre.child.requestId.
      show tPre.childRequestId = some pre.child.requestId
      exact h_child_id
  · -- Bridge tool ∈ post.parent.tools.
    exact h_tPost_in
  · -- tPost.callId = pre.bridgeCallId.
    show tPre.callId = pre.bridgeCallId
    exact h_id

/-! ### B2: child non-completed terminal projects to parent ToolCall failure -/

/-- B2: A child Request reaching a non-`.completed` terminal projects to
    the parent ToolCall reaching `.failed` (for child `.failed`/`.dead`/
    `.superseded`) or `.cancelled` (for child `.interrupted`).

    Same hypothesis-bundling pattern as B1: the running-tool witness carries
    the bridge callId, the `.running` state, and the child link, so we step
    the same tool we know is bridge-linked. No persistence guard here —
    `bridge_failure` does not require the bridge tool to be `.committed`. -/
theorem bridged_child_failure_projects
    (pre : BridgedState)
    (h_running    : ∃ t ∈ pre.parent.tools,
                      t.callId = pre.bridgeCallId ∧
                      t.state = .running ∧
                      t.childRequestId = some pre.child.requestId)
    (h_second_term : pre.terminalOf.isFailure) :
    ∃ post, Trace pre post ∧
            ∃ t ∈ post.parent.tools,
              t.callId = pre.bridgeCallId ∧
              (t.state = .failed ∨ t.state = .cancelled) := by
  -- Pull out the running bridge tool and its index in pre.parent.tools.
  obtain ⟨tPre, h_in, h_id, h_run_state, h_child_id⟩ := h_running
  obtain ⟨idx, h_idx⟩ := List.mem_iff_getElem?.mp h_in
  -- Project the second-leg terminal to a parent-tool terminal:
  --   .interrupted → .cancelled  (child interrupted, parent cancels in sympathy)
  --   everything else → .failed
  let projectedState : ToolExecution.ToolCallState := pre.terminalOf.projectedToolState
  let tPost : ToolExecution.ToolCallContext := { tPre with state := projectedState }
  let postParent : ComposedState :=
    { pre.parent with tools := pre.parent.tools.set idx tPost }
  let post : BridgedState := { pre with parent := postParent }
  -- Index bound for `List.mem_set`.
  have h_lt : idx < pre.parent.tools.length :=
    (List.getElem?_eq_some_iff.mp h_idx).1
  -- The post bridge tool ∈ post.parent.tools (it's the .set element).
  have h_tPost_in : tPost ∈ postParent.tools :=
    List.mem_set pre.parent.tools idx h_lt tPost
  -- Show projectedState is .failed ∨ .cancelled (used both inside the
  -- constructor's h_post_tool and in the final goal).
  have h_proj : projectedState = .failed ∨ projectedState = .cancelled := by
    exact ChildTerminal.projected_failure_state pre.terminalOf h_second_term
  refine ⟨post, ?_, tPost, h_tPost_in, ?_, ?_⟩
  · -- One-step trace via bridge_failure.
    refine Trace.step ?_ Trace.refl
    refine Transition.bridge_failure
      (idx := idx) (tPre := tPre) (tPost := tPost)
      h_second_term
      h_idx
      h_id
      h_run_state
      h_child_id
      ?_   -- h_post_callId
      ?_   -- h_post_state
      ?_   -- h_post_child
      rfl  -- h_tools_set       (post.parent.tools = pre.parent.tools.set idx tPost)
      rfl  -- h_request_eq      (post.parent.request = pre.parent.request)
      rfl  -- h_child_eq        (post.child = pre.child)
      rfl  -- h_bridgeId_eq     (post.bridgeCallId = pre.bridgeCallId)
      rfl  -- h_parent_id_eq    (post.parent.requestId = pre.parent.requestId)
    · -- h_post_callId: tPost.callId = pre.bridgeCallId.
      show tPre.callId = pre.bridgeCallId
      exact h_id
    · -- h_post_state: tPost.state ∈ {.failed, .cancelled}.
      show projectedState = .failed ∨ projectedState = .cancelled
      exact h_proj
    · -- h_post_child: tPost.childRequestId = some pre.child.requestId.
      show tPre.childRequestId = some pre.child.requestId
      exact h_child_id
  · -- tPost.callId = pre.bridgeCallId.
    show tPre.callId = pre.bridgeCallId
    exact h_id
  · -- tPost.state ∈ {.failed, .cancelled}.
    show projectedState = .failed ∨ projectedState = .cancelled
    exact h_proj

end BridgedState
end Subagent
