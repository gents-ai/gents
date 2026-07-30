import Proofs.CrossMachineComposed.State

namespace ComposedState

def UniqueCallIds (s : ComposedState) : Prop :=
  ∀ (i j : Nat) (h_i : i < s.tools.length) (h_j : j < s.tools.length),
    s.tools[i].callId = s.tools[j].callId → i = j

theorem initial_uniqueCallIds : initial.UniqueCallIds := by
  intro _ _ h_i _ _
  simp [initial] at h_i

theorem UniqueCallIds.eq_of_callId_eq
    {s : ComposedState} (h_uniq : s.UniqueCallIds)
    {t₁ t₂ : ToolExecution.ToolCallContext}
    (h_in₁ : t₁ ∈ s.tools) (h_in₂ : t₂ ∈ s.tools)
    (h_eq  : t₁.callId = t₂.callId) :
    t₁ = t₂ := by
  obtain ⟨i, h_i⟩ := List.mem_iff_getElem?.mp h_in₁
  obtain ⟨j, h_j⟩ := List.mem_iff_getElem?.mp h_in₂
  have h_i_lt : i < s.tools.length := (List.getElem?_eq_some_iff.mp h_i).1
  have h_j_lt : j < s.tools.length := (List.getElem?_eq_some_iff.mp h_j).1
  have h_t1_eq : s.tools[i] = t₁ := by
    have := (List.getElem?_eq_some_iff.mp h_i).2
    simpa using this
  have h_t2_eq : s.tools[j] = t₂ := by
    have := (List.getElem?_eq_some_iff.mp h_j).2
    simpa using this
  have h_idx_eq : i = j := by
    apply h_uniq i j h_i_lt h_j_lt
    rw [h_t1_eq, h_t2_eq]; exact h_eq
  subst h_idx_eq
  rw [← h_t1_eq, h_t2_eq]

private theorem length_set_eq {α : Type _} (l : List α) (i : Nat) (a : α) :
    (l.set i a).length = l.length := by
  exact List.length_set l i a

private theorem uniqueCallIds_of_tools_eq
    {pre post : ComposedState}
    (h_inv : pre.UniqueCallIds)
    (h_tools : post.tools = pre.tools) :
    post.UniqueCallIds := by
  intro i j h_i h_j h_eq
  have h_i' : i < pre.tools.length := by
    simpa [h_tools] using h_i
  have h_j' : j < pre.tools.length := by
    simpa [h_tools] using h_j
  apply h_inv i j h_i' h_j'
  simpa [h_tools] using h_eq

private theorem uniqueCallIds_append_fresh_preserved
    {s sPost : ComposedState} {newTool : ToolExecution.ToolCallContext}
    (h_uniq         : s.UniqueCallIds)
    (h_fresh        : ∀ t ∈ s.tools, t.callId ≠ newTool.callId)
    (h_tools_append : sPost.tools = s.tools ++ [newTool]) :
    sPost.UniqueCallIds := by
  intro i j h_i h_j h_eq
  have h_len : sPost.tools.length = s.tools.length + 1 := by
    rw [h_tools_append, List.length_append, List.length_singleton]
  have h_get_lt : ∀ (k : Nat) (h_lt : k < s.tools.length)
                    (h_k : k < sPost.tools.length),
      (sPost.tools[k]'h_k) = s.tools[k]'h_lt := by
    intro k h_lt h_k
    have hk1 : sPost.tools[k]'h_k
                = (s.tools ++ [newTool])[k]'(by rw [← h_tools_append]; exact h_k) := by
      simp [h_tools_append]
    rw [hk1]
    exact List.getElem_append_left h_lt
  have h_get_eq : ∀ (k : Nat) (h_k : k < sPost.tools.length),
      ¬ k < s.tools.length → (sPost.tools[k]'h_k) = newTool := by
    intro k h_k h_not_lt
    have h_k_total : k < s.tools.length + 1 := by rw [← h_len]; exact h_k
    have h_k_eq : k = s.tools.length := by omega
    have hk1 : sPost.tools[k]'h_k
                = (s.tools ++ [newTool])[k]'(by rw [← h_tools_append]; exact h_k) := by
      simp [h_tools_append]
    rw [hk1]
    have h_ge : s.tools.length ≤ k := by rw [h_k_eq]
    rw [List.getElem_append_right h_ge]
    simp [h_k_eq]
  by_cases h_i_lt : i < s.tools.length
  · by_cases h_j_lt : j < s.tools.length
    · apply h_uniq i j h_i_lt h_j_lt
      rw [← h_get_lt i h_i_lt h_i, ← h_get_lt j h_j_lt h_j]
      exact h_eq
    · exfalso
      have hi := h_get_lt i h_i_lt h_i
      have hj := h_get_eq j h_j h_j_lt
      have h_in_i : s.tools[i] ∈ s.tools := List.getElem_mem h_i_lt
      apply h_fresh _ h_in_i
      rw [hi, hj] at h_eq
      exact h_eq
  · by_cases h_j_lt : j < s.tools.length
    · exfalso
      have hi := h_get_eq i h_i h_i_lt
      have hj := h_get_lt j h_j_lt h_j
      have h_in_j : s.tools[j] ∈ s.tools := List.getElem_mem h_j_lt
      apply h_fresh _ h_in_j
      rw [hi, hj] at h_eq
      exact h_eq.symm
    · have h_i_total : i < s.tools.length + 1 := by rw [← h_len]; exact h_i
      have h_j_total : j < s.tools.length + 1 := by rw [← h_len]; exact h_j
      have h_i_eq : i = s.tools.length := by omega
      have h_j_eq : j = s.tools.length := by omega
      rw [h_i_eq, h_j_eq]

private theorem uniqueCallIds_map_currentTime_preserved
    {pre post : ComposedState} (t : Time)
    (h_inv : pre.UniqueCallIds)
    (h_tools : post.tools = pre.tools.map (fun tool => { tool with currentTime := t })) :
    post.UniqueCallIds := by
  intro i j h_i h_j h_eq
  have h_i' : i < pre.tools.length := by
    simpa [h_tools] using h_i
  have h_j' : j < pre.tools.length := by
    simpa [h_tools] using h_j
  apply h_inv i j h_i' h_j'
  simpa [h_tools] using h_eq

theorem uniqueCallIds_preserved
    {pre post : ComposedState}
    (h_inv  : pre.UniqueCallIds)
    (h_step : Transition pre post) :
    post.UniqueCallIds := by
  cases h_step with
  | process_step _ _ _ h_tools _ =>
    exact uniqueCallIds_of_tools_eq h_inv h_tools
  | request_step _ _ _ h_tools _ _ _ =>
    exact uniqueCallIds_of_tools_eq h_inv h_tools
  | slot_acquire _ _ _ _ _ h_tools _ =>
    exact uniqueCallIds_of_tools_eq h_inv h_tools
  | request_interrupt _ _ _ _ h_tools _ =>
    exact uniqueCallIds_of_tools_eq h_inv h_tools
  | clock_advance t _ _ _ _ h_tools _ =>
    exact uniqueCallIds_map_currentTime_preserved t h_inv h_tools
  | persistence_step _ _ _ _ _ _ h_tools _ =>
    exact uniqueCallIds_of_tools_eq h_inv h_tools
  | call_step _ _ _ h_tools _ =>
    exact uniqueCallIds_of_tools_eq h_inv h_tools
  | @tool_spawn newTool _ _ h_tools _ _ _ _ _ _ h_fresh _ =>
    exact uniqueCallIds_append_fresh_preserved h_inv h_fresh h_tools
  | @tool_step idx toolPre toolPost h_idx h_t_step h_tools _ _ _ _ _ _ _ =>
    have h_callId_eq : toolPost.callId = toolPre.callId :=
      ToolExecution.ToolCallContext.transition_preserves_callId h_t_step
    have h_len : post.tools.length = pre.tools.length := by
      rw [h_tools]; exact List.length_set _ _ _
    have h_idx_lt : idx < pre.tools.length :=
      (List.getElem?_eq_some_iff.mp h_idx).1
    have h_pre_idx_eq : pre.tools[idx] = toolPre := by
      have := (List.getElem?_eq_some_iff.mp h_idx).2
      simpa using this
    intro i j h_i h_j h_eq
    have h_i' : i < pre.tools.length := by rw [h_len] at h_i; exact h_i
    have h_j' : j < pre.tools.length := by rw [h_len] at h_j; exact h_j
    have h_callId_at : ∀ (k : Nat) (h_k : k < pre.tools.length),
        (post.tools[k]'(by rw [h_len]; exact h_k)).callId = pre.tools[k].callId := by
      intro k h_k
      by_cases h_eq_idx : k = idx
      · subst h_eq_idx
        have : (post.tools[k]'(by rw [h_len]; exact h_k)) = toolPost := by
          have h_k_set : (pre.tools.set k toolPost)[k]'(by rw [List.length_set]; exact h_k)
                          = toolPost :=
            List.getElem_set_self (l := pre.tools) (i := k) (a := toolPost)
              (h := by rw [List.length_set]; exact h_k)
          have hk1 : post.tools[k]'(by rw [h_len]; exact h_k)
                      = (pre.tools.set k toolPost)[k]'(by rw [List.length_set]; exact h_k) := by
            simp [h_tools]
          rw [hk1]; exact h_k_set
        rw [this, h_callId_eq, ← h_pre_idx_eq]
      ·
        have h_k_set : (pre.tools.set idx toolPost)[k]'(by rw [List.length_set]; exact h_k)
                        = pre.tools[k] :=
          List.getElem_set_ne (l := pre.tools) (i := idx) (j := k) (a := toolPost)
            (h := fun h => h_eq_idx h.symm) (hj := by rw [List.length_set]; exact h_k)
        have hk1 : post.tools[k]'(by rw [h_len]; exact h_k)
                    = (pre.tools.set idx toolPost)[k]'(by rw [List.length_set]; exact h_k) := by
          simp [h_tools]
        rw [hk1, h_k_set]
    have h_eq' : pre.tools[i].callId = pre.tools[j].callId := by
      rw [← h_callId_at i h_i', ← h_callId_at j h_j']; exact h_eq
    exact h_inv i j h_i' h_j' h_eq'

end ComposedState
