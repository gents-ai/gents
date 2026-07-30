import Proofs.Background.Transition

namespace Subagent
namespace BridgedState

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
  obtain ⟨tPre, h_in, h_id, h_run_state, h_committed, h_child_id⟩ := h_running
  obtain ⟨idx, h_idx⟩ := List.mem_iff_getElem?.mp h_in
  let tPost : ToolExecution.ToolCallContext := { tPre with state := .completed }
  let postParent : ComposedState :=
    { pre.parent with tools := pre.parent.tools.set idx tPost }
  let post : BridgedState := { pre with parent := postParent }
  have h_lt : idx < pre.parent.tools.length :=
    (List.getElem?_eq_some_iff.mp h_idx).1
  have h_tPost_in : tPost ∈ postParent.tools :=
    List.mem_set pre.parent.tools idx h_lt tPost
  refine ⟨post, ?_, tPost, ?_, ?_, rfl⟩
  ·
    refine Trace.step ?_ Trace.refl
    refine Transition.bridge_complete
      (idx := idx) (tPre := tPre) (tPost := tPost)
      h_second_done
      h_idx
      h_id
      h_run_state
      h_committed
      h_child_id
      ?_
      ?_
      ?_
      rfl
      rfl
      rfl
      rfl
      rfl
    ·
      show tPre.callId = pre.bridgeCallId
      exact h_id
    ·
      rfl
    ·
      show tPre.childRequestId = some pre.child.requestId
      exact h_child_id
  ·
    exact h_tPost_in
  ·
    show tPre.callId = pre.bridgeCallId
    exact h_id

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
  obtain ⟨tPre, h_in, h_id, h_run_state, h_child_id⟩ := h_running
  obtain ⟨idx, h_idx⟩ := List.mem_iff_getElem?.mp h_in
  let projectedState : ToolExecution.ToolCallState := pre.terminalOf.projectedToolState
  let tPost : ToolExecution.ToolCallContext := { tPre with state := projectedState }
  let postParent : ComposedState :=
    { pre.parent with tools := pre.parent.tools.set idx tPost }
  let post : BridgedState := { pre with parent := postParent }
  have h_lt : idx < pre.parent.tools.length :=
    (List.getElem?_eq_some_iff.mp h_idx).1
  have h_tPost_in : tPost ∈ postParent.tools :=
    List.mem_set pre.parent.tools idx h_lt tPost
  have h_proj : projectedState = .failed ∨ projectedState = .cancelled := by
    exact ChildTerminal.projected_failure_state pre.terminalOf h_second_term
  refine ⟨post, ?_, tPost, h_tPost_in, ?_, ?_⟩
  ·
    refine Trace.step ?_ Trace.refl
    refine Transition.bridge_failure
      (idx := idx) (tPre := tPre) (tPost := tPost)
      h_second_term
      h_idx
      h_id
      h_run_state
      h_child_id
      ?_
      ?_
      ?_
      rfl
      rfl
      rfl
      rfl
      rfl
    ·
      show tPre.callId = pre.bridgeCallId
      exact h_id
    ·
      show projectedState = .failed ∨ projectedState = .cancelled
      exact h_proj
    ·
      show tPre.childRequestId = some pre.child.requestId
      exact h_child_id
  ·
    show tPre.callId = pre.bridgeCallId
    exact h_id
  ·
    show projectedState = .failed ∨ projectedState = .cancelled
    exact h_proj

end BridgedState
end Subagent
