import Proofs.CrossMachineComposed.State

namespace ComposedState

def invFG (s : ComposedState) : Prop :=
  (s.tools.filter (fun t => decide (t.awaitMode = .foreground) ∧
                              ¬ isTerminal t.state)).length ≤ 1

private lemma length_filter_set_le {α : Type _} (p : α → Bool) :
    ∀ (l : List α) (i : Nat) (a b : α),
      l[i]? = some a →
      (p b = true → p a = true) →
      ((l.set i b).filter p).length ≤ (l.filter p).length := by
  intro l
  induction l with
  | nil => intro i a b h _; simp at h
  | cons x xs ih =>
    intro i a b h_idx h_imp
    cases i with
    | zero =>
      have hxa : x = a := by simpa using h_idx
      subst hxa
      simp only [List.set_cons_zero, List.filter_cons]
      by_cases hb : p b = true
      ·
        have ha : p x = true := h_imp hb
        simp [hb, ha]
      ·
        by_cases ha : p x = true
        · rw [if_neg hb, if_pos ha, List.length_cons]; omega
        · rw [if_neg hb, if_neg ha]
    | succ j =>
      simp only [List.set_cons_succ, List.filter_cons]
      have h_tail : xs[j]? = some a := by simpa using h_idx
      have ih' : ((xs.set j b).filter p).length ≤ (xs.filter p).length :=
        ih j a b h_tail h_imp
      by_cases hx : p x = true
      · rw [if_pos hx, if_pos hx, List.length_cons, List.length_cons]; omega
      · rw [if_neg hx, if_neg hx]; exact ih'

private lemma length_filter_set_le_succ {α : Type _} (p : α → Bool) :
    ∀ (l : List α) (i : Nat) (b : α),
      ((l.set i b).filter p).length ≤ (l.filter p).length + 1 := by
  intro l
  induction l with
  | nil => intro i b; simp
  | cons x xs ih =>
    intro i b
    cases i with
    | zero =>
      simp only [List.set_cons_zero, List.filter_cons]
      by_cases hb : p b = true
      · by_cases hx : p x = true
        · rw [if_pos hb, if_pos hx]; simp [List.length_cons]
        · rw [if_pos hb, if_neg hx, List.length_cons]
      · by_cases hx : p x = true
        · rw [if_neg hb, if_pos hx, List.length_cons]; omega
        · rw [if_neg hb, if_neg hx]; omega
    | succ j =>
      simp only [List.set_cons_succ, List.filter_cons]
      have ih' : ((xs.set j b).filter p).length ≤ (xs.filter p).length + 1 :=
        ih j b
      by_cases hx : p x = true
      · rw [if_pos hx, if_pos hx]; simp [List.length_cons]; omega
      · rw [if_neg hx, if_neg hx]; exact ih'

theorem invFG_preserved
    {pre post : ComposedState}
    (h_inv  : pre.invFG)
    (h_step : Transition pre post) :
    post.invFG := by
  cases h_step with
  | process_step _ _ _ h_tools _ =>
    unfold invFG; rw [h_tools]; exact h_inv
  | request_step _ _ _ h_tools _ _ _ =>
    unfold invFG; rw [h_tools]; exact h_inv
  | slot_acquire _ _ _ _ _ h_tools _ =>
    unfold invFG; rw [h_tools]; exact h_inv
  | request_interrupt _ _ _ _ h_tools _ =>
    unfold invFG; rw [h_tools]; exact h_inv
  | clock_advance t _ _ _ _ h_tools _ =>
    unfold invFG
    rw [h_tools]
    set p : ToolExecution.ToolCallContext → Bool :=
      fun t => decide (t.awaitMode = .foreground) ∧ ¬ isTerminal t.state with hp
    have h_filter_len :
        ((pre.tools.map (fun tool => { tool with currentTime := t })).filter p).length =
          (pre.tools.filter p).length := by
      induction pre.tools with
      | nil => simp
      | cons tool rest ih =>
        have h_tool :
            p { tool with currentTime := t } = p tool := by
          simp [p]
        simp only [List.map_cons, List.filter_cons]
        rw [h_tool]
        by_cases h_keep : p tool = true
        · rw [if_pos h_keep, if_pos h_keep, List.length_cons, List.length_cons, ih]
        · rw [if_neg h_keep, if_neg h_keep, ih]
    rw [h_filter_len]
    exact h_inv
  | persistence_step _ _ _ _ _ _ h_tools _ =>
    unfold invFG; rw [h_tools]; exact h_inv
  | call_step _ _ _ h_tools _ =>
    unfold invFG; rw [h_tools]; exact h_inv
  | @tool_spawn newTool _ _ h_tools _ _ _ _ _ _ _ h_fg_guard =>
    unfold invFG
    rw [h_tools]
    set p : ToolExecution.ToolCallContext → Bool :=
      fun t => decide (t.awaitMode = .foreground) ∧ ¬ isTerminal t.state with hp
    by_cases h_new_p : p newTool = true
    · have h_new_fg : newTool.awaitMode = .foreground := by
        have h_new_props : newTool.awaitMode = .foreground ∧
            ¬ isTerminal newTool.state := by
          simpa [hp] using h_new_p
        exact h_new_props.1
      have h_no_other : ¬ ∃ t ∈ pre.tools, t.awaitMode = .foreground ∧
                            ¬ isTerminal t.state :=
        h_fg_guard h_new_fg
      have h_filter_nil : pre.tools.filter p = [] := by
        rw [List.filter_eq_nil_iff]
        intro t h_in h_pt
        apply h_no_other
        refine ⟨t, h_in, ?_, ?_⟩
        · simp [hp] at h_pt; exact h_pt.1
        · simp [hp] at h_pt; exact h_pt.2
      rw [List.filter_append, h_filter_nil]
      simp [h_new_p]
    · rw [List.filter_append]
      simp [h_new_p]
      exact h_inv
  | @tool_step idx toolPre toolPost h_idx h_t_step h_tools _ _ _ _ _ _ _ h_fg_guard =>
    unfold invFG
    rw [h_tools]
    set p : ToolExecution.ToolCallContext → Bool :=
      fun t => decide (t.awaitMode = .foreground) ∧ ¬ isTerminal t.state with hp
    cases h_t_step with
    | dispatch h_state h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp [hp, h_post] at h_post_p ⊢
      refine ⟨h_post_p.1, ?_⟩
      intro h_term
      rw [h_state] at h_term
      rcases h_term with h' | h' | h' | h' <;> cases h'
    | spawnFailed failure h_state h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp [hp, h_post] at h_post_p
      exact absurd h_post_p.2 (fun h => h (Or.inr (Or.inl rfl)))
    | complete h_state _ _ h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp [hp, h_post] at h_post_p
      exact absurd h_post_p.2 (fun h => h (Or.inl rfl))
    | fail failure h_state h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp [hp, h_post] at h_post_p
      exact absurd h_post_p.2 (fun h => h (Or.inr (Or.inl rfl)))
    | timeout h_state _ h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp [hp, h_post] at h_post_p
      exact absurd h_post_p.2 (fun h => h (Or.inr (Or.inr (Or.inl rfl))))
    | cancelBeforeDispatch _ h_state h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp [hp, h_post] at h_post_p
      exact absurd h_post_p.2 (fun h => h (Or.inr (Or.inr (Or.inr rfl))))
    | cancelDuringRun _ h_state h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp [hp, h_post] at h_post_p
      exact absurd h_post_p.2 (fun h => h (Or.inr (Or.inr (Or.inr rfl))))
    | holdForApproval h_state h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp [hp, h_post] at h_post_p ⊢
      refine ⟨h_post_p.1, ?_⟩
      intro h_term
      rw [h_state] at h_term
      rcases h_term with h' | h' | h' | h' <;> cases h'
    | recordApproval _ h_state h_none h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp only [hp, h_post] at h_post_p ⊢
      exact h_post_p
    | approve h_state h_evidence h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp [hp, h_post] at h_post_p ⊢
      refine ⟨h_post_p.1, ?_⟩
      intro h_term
      rw [h_state] at h_term
      rcases h_term with h' | h' | h' | h' <;> cases h'
    | deny h_state h_evidence h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp [hp, h_post] at h_post_p
      exact absurd h_post_p.2 (fun h => h (Or.inr (Or.inl rfl)))
    | cancelWhileHeld _ h_state h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp [hp, h_post] at h_post_p
      exact absurd h_post_p.2 (fun h => h (Or.inr (Or.inr (Or.inr rfl))))
    | timeoutWhileHeld h_state _ h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp [hp, h_post] at h_post_p
      exact absurd h_post_p.2 (fun h => h (Or.inr (Or.inr (Or.inl rfl))))
    | background h_state h_mode h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      exfalso
      simp [hp, h_post] at h_post_p
    | foreground h_state h_mode h_post =>
      have h_post_fg : toolPost.awaitMode = .foreground := by simp [h_post]
      have h_no_other : ¬ ∃ t ∈ pre.tools, t.awaitMode = .foreground ∧
                            ¬ isTerminal t.state :=
        h_fg_guard h_mode h_post_fg
      have h_filter_nil : pre.tools.filter p = [] := by
        rw [List.filter_eq_nil_iff]
        intro t h_in h_pt
        apply h_no_other
        refine ⟨t, h_in, ?_, ?_⟩
        · simp [hp] at h_pt; exact h_pt.1
        · simp [hp] at h_pt; exact h_pt.2
      have h_pre_zero : (pre.tools.filter p).length = 0 := by
        rw [h_filter_nil]; rfl
      have h_le := length_filter_set_le_succ p pre.tools idx toolPost
      omega
    | detach h_live h_pol h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp only [hp, h_post] at h_post_p ⊢
      exact h_post_p
    | timeAdvance t h_le h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp only [hp, h_post] at h_post_p ⊢
      exact h_post_p
    | persistenceStep policy next h_p_step h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp only [hp, h_post] at h_post_p ⊢
      exact h_post_p

end ComposedState
