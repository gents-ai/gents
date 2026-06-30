import Proofs.CrossMachineComposed.State

namespace ComposedState

/-!
## UniqueCallIds: structural invariant on the tool list

Every tool in `pre.tools` has a distinct `callId`. The runtime mints fresh
callIds at spawn time and never reuses them, so this is a structural fact
about any reachable state, not a conditional assumption.

Used by `Subagent.Properties.detach_does_not_cancel_child` (B3') to discharge
the cascade-vs-detach contradiction without taking callId-uniqueness as a
hypothesis.
-/

/-- UniqueCallIds: every tool in the list has a distinct callId. -/
def UniqueCallIds (s : ComposedState) : Prop :=
  ∀ (i j : Nat) (h_i : i < s.tools.length) (h_j : j < s.tools.length),
    s.tools[i].callId = s.tools[j].callId → i = j

/-- The empty initial tool list has unique call ids. -/
theorem initial_uniqueCallIds : initial.UniqueCallIds := by
  intro _ _ h_i _ _
  simp [initial] at h_i

/-- Pairwise corollary: from UniqueCallIds, any two `∈ s.tools` tools with
    the same callId are equal. The form consumed by B3' (cascade-vs-detach
    contradiction) and similar pairwise-uniqueness arguments. -/
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
  -- Substitute i := j to identify s.tools[i] with s.tools[j].
  subst h_idx_eq
  -- Now h_t1_eq, h_t2_eq both refer to s.tools[i], so t₁ = s.tools[i] = t₂.
  rw [← h_t1_eq, h_t2_eq]

/-- Helper: `set` preserves length. -/
private theorem length_set_eq {α : Type _} (l : List α) (i : Nat) (a : α) :
    (l.set i a).length = l.length := by
  exact List.length_set l i a

/-- UniqueCallIds is preserved by every composed transition. The four non-tool
    arms simply propagate `pre.tools = post.tools`. The `tool_step` arm uses
    the inner `transition_preserves_callId` to argue that the (single) tool
    swapped at `idx` keeps its original callId; uniqueness is therefore
    inherited from the pre-state. -/
theorem uniqueCallIds_preserved
    {pre post : ComposedState}
    (h_inv  : pre.UniqueCallIds)
    (h_step : Transition pre post) :
    post.UniqueCallIds := by
  cases h_step with
  | process_step _ _ _ h_tools _ =>
    intro i j h_i h_j h_eq
    have h_i' : i < pre.tools.length := by rw [h_tools] at h_i; exact h_i
    have h_j' : j < pre.tools.length := by rw [h_tools] at h_j; exact h_j
    apply h_inv i j h_i' h_j'
    have hi : pre.tools[i] = post.tools[i] := by congr 1 <;> rw [h_tools]
    have hj : pre.tools[j] = post.tools[j] := by congr 1 <;> rw [h_tools]
    rw [hi, hj]; exact h_eq
  | request_step _ _ _ h_tools _ _ _ =>
    intro i j h_i h_j h_eq
    have h_i' : i < pre.tools.length := by rw [h_tools] at h_i; exact h_i
    have h_j' : j < pre.tools.length := by rw [h_tools] at h_j; exact h_j
    apply h_inv i j h_i' h_j'
    have hi : pre.tools[i] = post.tools[i] := by congr 1 <;> rw [h_tools]
    have hj : pre.tools[j] = post.tools[j] := by congr 1 <;> rw [h_tools]
    rw [hi, hj]; exact h_eq
  | persistence_step _ _ _ _ _ _ h_tools _ =>
    intro i j h_i h_j h_eq
    have h_i' : i < pre.tools.length := by rw [h_tools] at h_i; exact h_i
    have h_j' : j < pre.tools.length := by rw [h_tools] at h_j; exact h_j
    apply h_inv i j h_i' h_j'
    have hi : pre.tools[i] = post.tools[i] := by congr 1 <;> rw [h_tools]
    have hj : pre.tools[j] = post.tools[j] := by congr 1 <;> rw [h_tools]
    rw [hi, hj]; exact h_eq
  | call_step _ _ _ h_tools _ =>
    intro i j h_i h_j h_eq
    have h_i' : i < pre.tools.length := by rw [h_tools] at h_i; exact h_i
    have h_j' : j < pre.tools.length := by rw [h_tools] at h_j; exact h_j
    apply h_inv i j h_i' h_j'
    have hi : pre.tools[i] = post.tools[i] := by congr 1 <;> rw [h_tools]
    have hj : pre.tools[j] = post.tools[j] := by congr 1 <;> rw [h_tools]
    rw [hi, hj]; exact h_eq
  | @tool_step idx toolPre toolPost h_idx h_t_step h_tools _ _ _ _ _ _ _ =>
    -- post.tools = pre.tools.set idx toolPost. Since
    -- transition_preserves_callId says toolPost.callId = toolPre.callId, the
    -- callId at every index is the same in pre and post. UniqueCallIds carries
    -- straight through.
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
    -- For each index k, post.tools[k].callId = pre.tools[k].callId.
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
            congr 1 <;> rw [h_tools]
          rw [hk1]; exact h_k_set
        rw [this, h_callId_eq, ← h_pre_idx_eq]
      · -- k ≠ idx: set leaves the element unchanged.
        have h_k_set : (pre.tools.set idx toolPost)[k]'(by rw [List.length_set]; exact h_k)
                        = pre.tools[k] :=
          List.getElem_set_ne (l := pre.tools) (i := idx) (j := k) (a := toolPost)
            (h := fun h => h_eq_idx h.symm) (hj := by rw [List.length_set]; exact h_k)
        have hk1 : post.tools[k]'(by rw [h_len]; exact h_k)
                    = (pre.tools.set idx toolPost)[k]'(by rw [List.length_set]; exact h_k) := by
          congr 1 <;> rw [h_tools]
        rw [hk1, h_k_set]
    have h_eq' : pre.tools[i].callId = pre.tools[j].callId := by
      rw [← h_callId_at i h_i', ← h_callId_at j h_j']; exact h_eq
    exact h_inv i j h_i' h_j' h_eq'


end ComposedState
