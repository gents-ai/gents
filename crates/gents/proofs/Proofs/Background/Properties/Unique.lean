import Proofs.Background.Transition

namespace Subagent
namespace BridgedState

private theorem bridgedUniqueCallIds_step
    {s₁ s₂ : BridgedState}
    (h_parent_uniq : s₁.parent.UniqueCallIds)
    (h_child_uniq  : s₁.child.UniqueCallIds)
    (h_step : Transition s₁ s₂) :
    s₂.parent.UniqueCallIds ∧ s₂.child.UniqueCallIds := by
  cases h_step with
  | parent_step h_inner h_child_eq _ _ _ =>
    refine ⟨ComposedState.uniqueCallIds_preserved h_parent_uniq h_inner, ?_⟩
    rw [h_child_eq]; exact h_child_uniq
  | child_step h_inner h_parent_eq _ _ _ =>
    refine ⟨?_, ComposedState.uniqueCallIds_preserved h_child_uniq h_inner⟩
    rw [h_parent_eq]; exact h_parent_uniq
  | @bridge_spawn newTool _ _ h_newTool_callId _ _ h_tools_append _ h_post_child_tools _ _ h_callId_fresh =>
    refine ⟨?_, ?_⟩
    ·
      apply ComposedState.uniqueCallIds_append_fresh_preserved (s := s₁.parent)
        h_parent_uniq ?_ h_tools_append
      intro t h_in
      rw [h_newTool_callId]
      exact h_callId_fresh t h_in
    ·
      intro i j h_i h_j _
      rw [h_post_child_tools] at h_i
      cases h_i
  | @bridge_complete idx tPre tPost _ h_idx_pre h_pre_callId _ _ _
                       h_post_callId _ _ h_tools_set _ h_child_eq _ _ =>
    refine ⟨?_, ?_⟩
    ·
      apply ComposedState.uniqueCallIds_set_callId_preserved (s := s₁.parent)
        h_parent_uniq h_idx_pre ?_ h_tools_set
      rw [h_post_callId, ← h_pre_callId]
    · rw [h_child_eq]; exact h_child_uniq
  | @bridge_failure idx tPre tPost _ h_idx_pre h_pre_callId _ _
                      h_post_callId _ _ h_tools_set _ h_child_eq _ _ =>
    refine ⟨?_, ?_⟩
    · apply ComposedState.uniqueCallIds_set_callId_preserved (s := s₁.parent)
        h_parent_uniq h_idx_pre ?_ h_tools_set
      rw [h_post_callId, ← h_pre_callId]
    · rw [h_child_eq]; exact h_child_uniq
  | bridge_cancel_cascade _ _ _ h_parent_eq _ _ _ _ _ h_child_tools_eq =>
    exact ⟨h_parent_eq ▸ h_parent_uniq,
      ComposedState.uniqueCallIds_of_tools_eq h_child_uniq h_child_tools_eq⟩

theorem bridgedUniqueCallIds_preserved
    (pre post : BridgedState)
    (h_parent_init : pre.parent.UniqueCallIds)
    (h_child_init  : pre.child.UniqueCallIds)
    (h_trace : Trace pre post) :
    post.parent.UniqueCallIds ∧ post.child.UniqueCallIds := by
  induction h_trace with
  | refl => exact ⟨h_parent_init, h_child_init⟩
  | step h_step _ ih =>
    obtain ⟨h_p, h_c⟩ := bridgedUniqueCallIds_step h_parent_init h_child_init h_step
    exact ih h_p h_c

end BridgedState
end Subagent
