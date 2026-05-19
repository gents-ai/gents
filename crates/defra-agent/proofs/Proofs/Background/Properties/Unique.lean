import Proofs.Background.Transition

/-! Unique tool-call id preservation for bridged subagent traces. -/

namespace Subagent
namespace BridgedState

/-! ### INV-UNIQUE: BridgedState lift

`ComposedState.UniqueCallIds` is preserved by every `ComposedState.Transition`
(see `ComposedState.uniqueCallIds_preserved`). The BridgedState lift below
states that both sides of the bridge satisfy `UniqueCallIds` across any
single `BridgedState.Transition`, then the trace-level theorem threads it
through `Trace.step`. -/

/-- Helper: replacing element at `idx` with a tool that has the same callId
    preserves `UniqueCallIds`. The `set`-style description in
    `bridge_complete` / `bridge_failure` lets us reuse this proof shape. -/
private theorem uniqueCallIds_set_callId_preserved
    {s sPost : ComposedState} {idx : Nat}
    {tPre tPost : ToolExecution.ToolCallContext}
    (h_uniq         : s.UniqueCallIds)
    (h_idx          : s.tools[idx]? = some tPre)
    (h_callId_eq    : tPost.callId = tPre.callId)
    (h_tools_set    : sPost.tools = s.tools.set idx tPost) :
    sPost.UniqueCallIds := by
  intro i j h_i h_j h_eq
  have h_len : sPost.tools.length = s.tools.length := by
    rw [h_tools_set]; exact List.length_set _ _ _
  have h_i' : i < s.tools.length := by rw [h_len] at h_i; exact h_i
  have h_j' : j < s.tools.length := by rw [h_len] at h_j; exact h_j
  have h_idx_lt : idx < s.tools.length :=
    (List.getElem?_eq_some_iff.mp h_idx).1
  have h_pre_idx_eq : s.tools[idx] = tPre := by
    have := (List.getElem?_eq_some_iff.mp h_idx).2
    simpa using this
  -- For each k, post.tools[k].callId = pre.tools[k].callId.
  have h_callId_at : ∀ (k : Nat) (h_k : k < s.tools.length),
      (sPost.tools[k]'(by rw [h_len]; exact h_k)).callId = s.tools[k].callId := by
    intro k h_k
    by_cases h_eq_idx : k = idx
    · subst h_eq_idx
      have h_get : (sPost.tools[k]'(by rw [h_len]; exact h_k)) = tPost := by
        have h_k_set : (s.tools.set k tPost)[k]'(by rw [List.length_set]; exact h_k)
                        = tPost :=
          List.getElem_set_self (l := s.tools) (i := k) (a := tPost)
            (h := by rw [List.length_set]; exact h_k)
        have hk1 : sPost.tools[k]'(by rw [h_len]; exact h_k)
                    = (s.tools.set k tPost)[k]'(by rw [List.length_set]; exact h_k) := by
          congr 1 <;> rw [h_tools_set]
        rw [hk1]; exact h_k_set
      rw [h_get, h_callId_eq, ← h_pre_idx_eq]
    · have h_k_set : (s.tools.set idx tPost)[k]'(by rw [List.length_set]; exact h_k)
                      = s.tools[k] :=
        List.getElem_set_ne (l := s.tools) (i := idx) (j := k) (a := tPost)
          (h := fun h => h_eq_idx h.symm) (hj := by rw [List.length_set]; exact h_k)
      have hk1 : sPost.tools[k]'(by rw [h_len]; exact h_k)
                  = (s.tools.set idx tPost)[k]'(by rw [List.length_set]; exact h_k) := by
        congr 1 <;> rw [h_tools_set]
      rw [hk1, h_k_set]
  have h_eq' : s.tools[i].callId = s.tools[j].callId := by
    rw [← h_callId_at i h_i', ← h_callId_at j h_j']; exact h_eq
  exact h_uniq i j h_i' h_j' h_eq'

/-- Helper: appending a tool with a callId fresh w.r.t. `pre.tools` preserves
    `UniqueCallIds`. Used by the `bridge_spawn` arm. -/
private theorem uniqueCallIds_append_fresh_preserved
    {s sPost : ComposedState} {newTool : ToolExecution.ToolCallContext}
    (h_uniq         : s.UniqueCallIds)
    (h_fresh        : ∀ t ∈ s.tools, t.callId ≠ newTool.callId)
    (h_tools_append : sPost.tools = s.tools ++ [newTool]) :
    sPost.UniqueCallIds := by
  intro i j h_i h_j h_eq
  have h_len : sPost.tools.length = s.tools.length + 1 := by
    rw [h_tools_append, List.length_append, List.length_singleton]
  -- For each k, sPost.tools[k] = (s.tools ++ [newTool])[k]; case-split based
  -- on whether k < s.tools.length.
  have h_get_lt : ∀ (k : Nat) (h_lt : k < s.tools.length)
                    (h_k : k < sPost.tools.length),
      (sPost.tools[k]'h_k) = s.tools[k]'h_lt := by
    intro k h_lt h_k
    have hk1 : sPost.tools[k]'h_k
                = (s.tools ++ [newTool])[k]'(by rw [← h_tools_append]; exact h_k) := by
      congr 1 <;> rw [h_tools_append]
    rw [hk1]
    exact List.getElem_append_left h_lt
  have h_get_eq : ∀ (k : Nat) (h_k : k < sPost.tools.length),
      ¬ k < s.tools.length → (sPost.tools[k]'h_k) = newTool := by
    intro k h_k h_not_lt
    have h_k_total : k < s.tools.length + 1 := by rw [← h_len]; exact h_k
    have h_k_eq : k = s.tools.length := by omega
    have hk1 : sPost.tools[k]'h_k
                = (s.tools ++ [newTool])[k]'(by rw [← h_tools_append]; exact h_k) := by
      congr 1 <;> rw [h_tools_append]
    rw [hk1]
    have h_ge : s.tools.length ≤ k := by rw [h_k_eq]
    rw [List.getElem_append_right h_ge]
    simp [h_k_eq]
  -- Case-split on whether i < s.tools.length and j < s.tools.length.
  by_cases h_i_lt : i < s.tools.length
  · by_cases h_j_lt : j < s.tools.length
    · -- Both indices in pre.tools; uniqueness from h_uniq.
      apply h_uniq i j h_i_lt h_j_lt
      rw [← h_get_lt i h_i_lt h_i, ← h_get_lt j h_j_lt h_j]
      exact h_eq
    · -- j = length (newTool); i < length.  callIds equal contradicts freshness.
      exfalso
      have hi := h_get_lt i h_i_lt h_i
      have hj := h_get_eq j h_j h_j_lt
      have h_in_i : s.tools[i] ∈ s.tools := List.getElem_mem h_i_lt
      apply h_fresh _ h_in_i
      rw [hi, hj] at h_eq
      exact h_eq
  · by_cases h_j_lt : j < s.tools.length
    · -- i = length (newTool); j < length. Symmetric contradiction.
      exfalso
      have hi := h_get_eq i h_i h_i_lt
      have hj := h_get_lt j h_j_lt h_j
      have h_in_j : s.tools[j] ∈ s.tools := List.getElem_mem h_j_lt
      apply h_fresh _ h_in_j
      rw [hi, hj] at h_eq
      exact h_eq.symm
    · -- Both indices ≥ length; both = length; i = j.
      have h_i_total : i < s.tools.length + 1 := by rw [← h_len]; exact h_i
      have h_j_total : j < s.tools.length + 1 := by rw [← h_len]; exact h_j
      have h_i_eq : i = s.tools.length := by omega
      have h_j_eq : j = s.tools.length := by omega
      rw [h_i_eq, h_j_eq]

/-- Per-step preservation of INV-UNIQUE on both sides of the bridge. -/
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
    · -- Uniqueness on post.parent.tools = pre.parent.tools ++ [newTool].
      -- newTool.callId = post.bridgeCallId, and h_callId_fresh says no tool in
      -- pre.parent.tools has callId = post.bridgeCallId.
      apply uniqueCallIds_append_fresh_preserved (s := s₁.parent)
        h_parent_uniq ?_ h_tools_append
      intro t h_in
      rw [h_newTool_callId]
      exact h_callId_fresh t h_in
    · -- post.child.tools = [] is trivially unique.
      intro i j h_i h_j _
      rw [h_post_child_tools] at h_i
      cases h_i
  | @bridge_complete idx tPre tPost _ h_idx_pre h_pre_callId _ _ _
                       h_post_callId _ _ h_tools_set _ h_child_eq _ _ =>
    refine ⟨?_, ?_⟩
    · -- post.parent.tools = pre.parent.tools.set idx tPost; tPost.callId =
      -- tPre.callId via h_post_callId, h_pre_callId.
      apply uniqueCallIds_set_callId_preserved (s := s₁.parent)
        h_parent_uniq h_idx_pre ?_ h_tools_set
      rw [h_post_callId, ← h_pre_callId]
    · rw [h_child_eq]; exact h_child_uniq
  | @bridge_failure idx tPre tPost _ h_idx_pre h_pre_callId _ _
                      h_post_callId _ _ h_tools_set _ h_child_eq _ _ =>
    refine ⟨?_, ?_⟩
    · apply uniqueCallIds_set_callId_preserved (s := s₁.parent)
        h_parent_uniq h_idx_pre ?_ h_tools_set
      rw [h_post_callId, ← h_pre_callId]
    · rw [h_child_eq]; exact h_child_uniq
  | bridge_cancel_cascade _ _ _ h_parent_eq _ _ _ _ _ h_child_tools_eq =>
    refine ⟨?_, ?_⟩
    · -- post.parent = pre.parent → tools unchanged.
      intro i j h_i h_j h_eq
      have h_tools : s₂.parent.tools = s₁.parent.tools := by rw [h_parent_eq]
      have h_i' : i < s₁.parent.tools.length := by rw [h_tools] at h_i; exact h_i
      have h_j' : j < s₁.parent.tools.length := by rw [h_tools] at h_j; exact h_j
      apply h_parent_uniq i j h_i' h_j'
      have hi : s₁.parent.tools[i] = s₂.parent.tools[i] := by congr 1 <;> rw [h_tools]
      have hj : s₁.parent.tools[j] = s₂.parent.tools[j] := by congr 1 <;> rw [h_tools]
      rw [hi, hj]; exact h_eq
    · -- post.child.tools = pre.child.tools via h_child_tools_eq.
      intro i j h_i h_j h_eq
      have h_i' : i < s₁.child.tools.length := by rw [h_child_tools_eq] at h_i; exact h_i
      have h_j' : j < s₁.child.tools.length := by rw [h_child_tools_eq] at h_j; exact h_j
      apply h_child_uniq i j h_i' h_j'
      have hi : s₁.child.tools[i] = s₂.child.tools[i] := by
        congr 1 <;> rw [h_child_tools_eq]
      have hj : s₁.child.tools[j] = s₂.child.tools[j] := by
        congr 1 <;> rw [h_child_tools_eq]
      rw [hi, hj]; exact h_eq

/-- INV-UNIQUE (BridgedState lift): both sides of the bridge satisfy
    `ComposedState.UniqueCallIds` across any reachable bridge trace. -/
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
