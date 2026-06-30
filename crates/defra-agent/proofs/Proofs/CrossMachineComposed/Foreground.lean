import Proofs.CrossMachineComposed.State

namespace ComposedState

/-!
## INV-FG: foreground-blocking structural invariant

At most one foreground non-terminal tool may be live at any time per
`ComposedState`. Combined with the scoped `h_no_block` guard on
`request_step`, this gives the parent narrative the foreground-blocking
property: while a foreground tool is in flight, `advance` /
`begin_inference` cannot fire.

INV-FG is a structural witness; no C-theorem currently consumes it. It is
preserved across every composed transition. The request/process/persistence/call
arms are trivial (they don't touch `tools`); `tool_spawn` uses its foreground
admission guard, and `tool_step` requires case-analysis on the inner
`ToolCallContext.Transition`.
-/

/-- INV-FG: at most one foreground non-terminal tool per composed state. -/
def invFG (s : ComposedState) : Prop :=
  (s.tools.filter (fun t => decide (t.awaitMode = .foreground) ∧
                              ¬ isTerminal t.state)).length ≤ 1

/-- Helper: setting at index `i` to a value `b` whose filter classification is
    implied by `a`'s never grows the filtered length. Concretely, if the new
    element passes the predicate, the old one did too — so the filter can only
    keep the same or fewer elements after `set`. -/
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
      -- l[0] = x, so a = x. set 0 = b :: xs.
      have hxa : x = a := by simpa using h_idx
      subst hxa
      simp only [List.set_cons_zero, List.filter_cons]
      by_cases hb : p b = true
      · -- p b true: both branches keep one element
        have ha : p x = true := h_imp hb
        simp [hb, ha]
      · -- p b false: post drops the element
        by_cases ha : p x = true
        · rw [if_neg hb, if_pos ha, List.length_cons]; omega
        · rw [if_neg hb, if_neg ha]
    | succ j =>
      -- Recurse on tail.
      simp only [List.set_cons_succ, List.filter_cons]
      have h_tail : xs[j]? = some a := by simpa using h_idx
      have ih' : ((xs.set j b).filter p).length ≤ (xs.filter p).length :=
        ih j a b h_tail h_imp
      by_cases hx : p x = true
      · rw [if_pos hx, if_pos hx, List.length_cons, List.length_cons]; omega
      · rw [if_neg hx, if_neg hx]; exact ih'

/-- Helper: setting at index `i` to a value `b` increases the filtered length
    by at most one — the old element either passed the filter (count
    unchanged or decreases) or didn't (count grows by ≤ 1). Together with
    the foreground-flip guard's "pre count = 0" precondition, this bounds
    `post.invFG` for the foreground constructor. -/
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

/-- INV-FG is preserved by any composed-state transition. -/
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
  | persistence_step _ _ _ _ _ _ h_tools _ =>
    unfold invFG; rw [h_tools]; exact h_inv
  | call_step _ _ _ h_tools _ =>
    unfold invFG; rw [h_tools]; exact h_inv
  | @tool_spawn newTool _ _ h_tools _ _ _ _ _ _ h_fg_guard =>
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
  | @tool_step idx toolPre toolPost h_idx h_t_step h_tools _ _ _ _ _ _ h_fg_guard =>
    -- A single tool transitions. Case-split on the inner ToolCallContext.Transition.
    -- For all 11 non-`foreground` constructors: `toolPost.awaitMode = toolPre.awaitMode`
    -- AND if toolPost passes the filter (foreground + non-terminal) then so does
    -- toolPre. Hence by `length_filter_set_le`, post count ≤ pre count ≤ 1.
    -- For `foreground`: the guard `h_fg_guard` fires, forcing the pre-state to
    -- have no foreground non-terminal tool, i.e. `pre.filter ... = []`. By
    -- `length_filter_set_le_succ`, post count ≤ 0 + 1 = 1.
    unfold invFG
    rw [h_tools]
    set p : ToolExecution.ToolCallContext → Bool :=
      fun t => decide (t.awaitMode = .foreground) ∧ ¬ isTerminal t.state with hp
    -- Helper: every non-foreground inner constructor has the property that
    -- p toolPost → p toolPre, so `length_filter_set_le` closes the case.
    -- The `foreground` constructor is the lone exception, handled by the guard.
    cases h_t_step with
    | dispatch h_state h_post =>
      -- toolPost = { toolPre with state := .running, ... }. awaitMode preserved.
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp [hp, h_post] at h_post_p ⊢
      refine ⟨h_post_p.1, ?_⟩
      intro h_term
      rw [h_state] at h_term
      rcases h_term with h' | h' | h' | h' <;> cases h'
    | spawnFailed failure h_state h_post =>
      -- toolPost.state = .failed (terminal); p toolPost = false.
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
    | background h_state h_mode h_post =>
      -- toolPost.awaitMode = .background; p toolPost = false (foreground required).
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      -- After simp, h_post_p will simplify to False (awaitMode = .background ≠ .foreground)
      -- so this branch becomes unreachable; absurd via False.elim.
      exfalso
      simp [hp, h_post] at h_post_p
    | foreground h_state h_mode h_post =>
      -- The lone case where post passes the filter but pre doesn't. Use the
      -- foreground-flip guard `h_fg_guard` to conclude pre's filter is empty.
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
      -- toolPost = { toolPre with cancelPolicy := .detach }; awaitMode and state preserved.
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
