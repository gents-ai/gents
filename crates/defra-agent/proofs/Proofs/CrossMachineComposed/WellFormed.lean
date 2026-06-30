import Proofs.CrossMachineComposed.Foreground
import Proofs.CrossMachineComposed.UniqueCallIds

namespace ComposedState

/-!
## Global composed-state well-formedness

The C-theorems should not take structural coherence facts as ad-hoc per-tool
hypotheses. `WellFormed` bundles the list-level invariants that are established
at `initial` and preserved by every composed transition.
-/

/-- A coherent tool remains coherent when the composed request and request id
    are unchanged. -/
private theorem coherent_of_request_eq
    {pre post : ComposedState} {tool : ToolExecution.ToolCallContext}
    (h_coherent : Coherent pre tool)
    (h_request : post.request = pre.request)
    (h_requestId : post.requestId = pre.requestId) :
    Coherent post tool := by
  obtain ⟨h_linked, h_deadline, h_time⟩ := h_coherent
  exact ⟨by simpa [h_requestId] using h_linked,
         by simpa [h_request] using h_deadline,
         by simpa [h_request] using h_time⟩

/-- A coherent tool remains coherent when only request fields irrelevant to
    coherence changed. -/
private theorem coherent_of_request_clock_eq
    {pre post : ComposedState} {tool : ToolExecution.ToolCallContext}
    (h_coherent : Coherent pre tool)
    (h_requestId : post.requestId = pre.requestId)
    (h_deadline : post.request.deadline = pre.request.deadline)
    (h_time : post.request.currentTime = pre.request.currentTime) :
    Coherent post tool := by
  obtain ⟨h_linked, h_tool_deadline, h_tool_time⟩ := h_coherent
  exact ⟨by simpa [h_requestId] using h_linked,
         by simpa [h_deadline] using h_tool_deadline,
         by simpa [h_time] using h_tool_time⟩

/-- Membership in `l.set i a` means either the member is the replacement or it
    was already present in the original list. -/
private lemma mem_set_eq_or_mem {α : Type _}
    (l : List α) (i : Nat) (a b : α)
    (h_mem : b ∈ l.set i a) :
    b = a ∨ b ∈ l := by
  induction l generalizing i with
  | nil =>
    cases i <;> simp at h_mem
  | cons x xs ih =>
    cases i with
    | zero =>
      simp at h_mem ⊢
      cases h_mem with
      | inl h_eq => exact Or.inl h_eq
      | inr h_tail => exact Or.inr (Or.inr h_tail)
    | succ i =>
      simp at h_mem ⊢
      cases h_mem with
      | inl h_head => exact Or.inr (Or.inl h_head)
      | inr h_tail =>
        cases ih i h_tail with
        | inl h_eq => exact Or.inl h_eq
        | inr h_old => exact Or.inr (Or.inr h_old)

/-- Linkage is the first component of list-level coherence. -/
theorem allToolsLinked_of_allToolsCoherent
    {s : ComposedState}
    (h_coherent : s.AllToolsCoherent) :
    s.AllToolsLinked := by
  intro t h_in
  exact (h_coherent t h_in).1

/-- The empty initial tool list is list-coherent. -/
theorem initial_allToolsCoherent : initial.AllToolsCoherent := by
  intro _ h_in
  simp [initial] at h_in

/-- The empty initial tool list is linked to the initial request. -/
theorem initial_allToolsLinked : initial.AllToolsLinked :=
  allToolsLinked_of_allToolsCoherent initial_allToolsCoherent

/-- The initial state has no pre-processing tools. -/
theorem initial_noToolsBeforeProcessing : initial.NoToolsBeforeProcessing := by
  intro _ h_in
  simp [initial] at h_in

/-- The empty initial tool list satisfies the foreground-live uniqueness
    invariant. -/
theorem initial_invFG : initial.invFG := by
  unfold invFG
  simp [initial]

/-- List-level coherence is preserved by every composed transition, provided
    the pre-state also rules out malformed pending/claimed states with tools. -/
theorem allToolsCoherent_preserved
    {pre post : ComposedState}
    (h_coherent : pre.AllToolsCoherent)
    (h_no_early_tools : pre.NoToolsBeforeProcessing)
    (h_step : Transition pre post) :
    post.AllToolsCoherent := by
  intro tool h_in_post
  cases h_step with
  | process_step _ h_request _ h_tools h_requestId =>
    have h_in_pre : tool ∈ pre.tools := by
      rw [h_tools] at h_in_post
      exact h_in_post
    exact coherent_of_request_eq (h_coherent tool h_in_pre) h_request h_requestId
  | request_step h_request_step _ _ h_tools h_requestId _ _ =>
    have h_in_pre : tool ∈ pre.tools := by
      rw [h_tools] at h_in_post
      exact h_in_post
    cases h_request_step with
    | claim h_state _ _ _ =>
      exact False.elim ((h_no_early_tools tool h_in_pre).1 h_state)
    | dedup_lose h_state _ _ =>
      exact False.elim ((h_no_early_tools tool h_in_pre).1 h_state)
    | begin_inference h_state _ _ =>
      exact False.elim ((h_no_early_tools tool h_in_pre).2 h_state)
    | advance _ _ h_post =>
      exact coherent_of_request_clock_eq (h_coherent tool h_in_pre) h_requestId
        (by simp [h_post]) (by simp [h_post])
    | finish _ _ h_post =>
      exact coherent_of_request_clock_eq (h_coherent tool h_in_pre) h_requestId
        (by simp [h_post]) (by simp [h_post])
    | fail _ _ h_post =>
      exact coherent_of_request_clock_eq (h_coherent tool h_in_pre) h_requestId
        (by simp [h_post]) (by simp [h_post])
    | fail_before_stream h_state _ _ =>
      exact False.elim ((h_no_early_tools tool h_in_pre).2 h_state)
    | expire h_state _ _ _ _ =>
      exact False.elim ((h_no_early_tools tool h_in_pre).1 h_state)
    | interrupt_before_claim h_state _ _ _ =>
      exact False.elim ((h_no_early_tools tool h_in_pre).1 h_state)
    | interrupt_claimed h_state _ _ _ =>
      exact False.elim ((h_no_early_tools tool h_in_pre).2 h_state)
    | interrupt_processing _ _ _ h_post =>
      exact coherent_of_request_clock_eq (h_coherent tool h_in_pre) h_requestId
        (by simp [h_post]) (by simp [h_post])
  | persistence_step _ _ _ h_request _ _ h_tools h_requestId =>
    have h_in_pre : tool ∈ pre.tools := by
      rw [h_tools] at h_in_post
      exact h_in_post
    exact coherent_of_request_clock_eq (h_coherent tool h_in_pre) h_requestId
      (by simp [h_request]) (by simp [h_request])
  | call_step _ h_request _ h_tools h_requestId =>
    have h_in_pre : tool ∈ pre.tools := by
      rw [h_tools] at h_in_post
      exact h_in_post
    exact coherent_of_request_eq (h_coherent tool h_in_pre) h_request h_requestId
  | @tool_step idx toolPre toolPost h_idx _ h_tools h_request _ _ h_requestId
      _ h_post_coherent _ =>
    have h_in_set : tool ∈ pre.tools.set idx toolPost := by
      rw [h_tools] at h_in_post
      exact h_in_post
    cases mem_set_eq_or_mem pre.tools idx toolPost tool h_in_set with
    | inl h_eq =>
      subst h_eq
      exact h_post_coherent
    | inr h_in_pre =>
      exact coherent_of_request_eq (h_coherent tool h_in_pre) h_request h_requestId

/-- List-level linkage is preserved because list-level coherence is preserved. -/
theorem allToolsLinked_preserved
    {pre post : ComposedState}
    (h_coherent : pre.AllToolsCoherent)
    (h_no_early_tools : pre.NoToolsBeforeProcessing)
    (h_step : Transition pre post) :
    post.AllToolsLinked :=
  allToolsLinked_of_allToolsCoherent
    (allToolsCoherent_preserved h_coherent h_no_early_tools h_step)

/-- The no-tools-before-processing invariant is preserved by every composed
    transition. -/
theorem noToolsBeforeProcessing_preserved
    {pre post : ComposedState}
    (h_no_early_tools : pre.NoToolsBeforeProcessing)
    (h_step : Transition pre post) :
    post.NoToolsBeforeProcessing := by
  intro tool h_in_post
  cases h_step with
  | process_step _ h_request _ h_tools _ =>
    have h_in_pre : tool ∈ pre.tools := by
      rw [h_tools] at h_in_post
      exact h_in_post
    obtain ⟨h_not_pending, h_not_claimed⟩ := h_no_early_tools tool h_in_pre
    refine ⟨?_, ?_⟩
    · intro h_pending
      exact h_not_pending (by simpa [h_request] using h_pending)
    · intro h_claimed
      exact h_not_claimed (by simpa [h_request] using h_claimed)
  | request_step h_request_step _ _ h_tools _ _ _ =>
    have h_in_pre : tool ∈ pre.tools := by
      rw [h_tools] at h_in_post
      exact h_in_post
    cases h_request_step with
    | claim h_state _ _ _ =>
      exact False.elim ((h_no_early_tools tool h_in_pre).1 h_state)
    | dedup_lose h_state _ _ =>
      exact False.elim ((h_no_early_tools tool h_in_pre).1 h_state)
    | begin_inference h_state _ _ =>
      exact False.elim ((h_no_early_tools tool h_in_pre).2 h_state)
    | advance h_state _ h_post =>
      refine ⟨?_, ?_⟩
      · intro h_pending
        have h_pre_pending : pre.request.state = .pending := by
          simpa [h_post] using h_pending
        rw [h_state] at h_pre_pending
        cases h_pre_pending
      · intro h_claimed
        have h_pre_claimed : pre.request.state = .claimed := by
          simpa [h_post] using h_claimed
        rw [h_state] at h_pre_claimed
        cases h_pre_claimed
    | finish _ _ h_post =>
      have h_state_post : post.request.state = .completed := by
        simp [h_post]
      refine ⟨?_, ?_⟩
      · intro h_pending
        rw [h_state_post] at h_pending
        cases h_pending
      · intro h_claimed
        rw [h_state_post] at h_claimed
        cases h_claimed
    | fail _ _ h_post =>
      have h_state_post : post.request.state = .failed := by
        simp [h_post]
      refine ⟨?_, ?_⟩
      · intro h_pending
        rw [h_state_post] at h_pending
        cases h_pending
      · intro h_claimed
        rw [h_state_post] at h_claimed
        cases h_claimed
    | fail_before_stream h_state _ _ =>
      exact False.elim ((h_no_early_tools tool h_in_pre).2 h_state)
    | expire h_state _ _ _ _ =>
      exact False.elim ((h_no_early_tools tool h_in_pre).1 h_state)
    | interrupt_before_claim h_state _ _ _ =>
      exact False.elim ((h_no_early_tools tool h_in_pre).1 h_state)
    | interrupt_claimed h_state _ _ _ =>
      exact False.elim ((h_no_early_tools tool h_in_pre).2 h_state)
    | interrupt_processing _ _ _ h_post =>
      have h_state_post : post.request.state = .interrupted := by
        simp [h_post]
      refine ⟨?_, ?_⟩
      · intro h_pending
        rw [h_state_post] at h_pending
        cases h_pending
      · intro h_claimed
        rw [h_state_post] at h_claimed
        cases h_claimed
  | persistence_step _ _ _ h_request _ _ h_tools _ =>
    have h_in_pre : tool ∈ pre.tools := by
      rw [h_tools] at h_in_post
      exact h_in_post
    obtain ⟨h_not_pending, h_not_claimed⟩ := h_no_early_tools tool h_in_pre
    refine ⟨?_, ?_⟩
    · intro h_pending
      exact h_not_pending (by simpa [h_request] using h_pending)
    · intro h_claimed
      exact h_not_claimed (by simpa [h_request] using h_claimed)
  | call_step _ h_request _ h_tools _ =>
    have h_in_pre : tool ∈ pre.tools := by
      rw [h_tools] at h_in_post
      exact h_in_post
    obtain ⟨h_not_pending, h_not_claimed⟩ := h_no_early_tools tool h_in_pre
    refine ⟨?_, ?_⟩
    · intro h_pending
      exact h_not_pending (by simpa [h_request] using h_pending)
    · intro h_claimed
      exact h_not_claimed (by simpa [h_request] using h_claimed)
  | @tool_step idx toolPre _ h_idx _ _ h_request _ _ _ _ _ _ =>
    have h_toolPre_in : toolPre ∈ pre.tools :=
      List.mem_iff_getElem?.mpr ⟨idx, h_idx⟩
    obtain ⟨h_not_pending, h_not_claimed⟩ :=
      h_no_early_tools toolPre h_toolPre_in
    refine ⟨?_, ?_⟩
    · intro h_pending
      exact h_not_pending (by simpa [h_request] using h_pending)
    · intro h_claimed
      exact h_not_claimed (by simpa [h_request] using h_claimed)

/-- The global composed-state well-formedness invariant. -/
structure WellFormed (s : ComposedState) : Prop where
  allToolsCoherent : s.AllToolsCoherent
  allToolsLinked : s.AllToolsLinked
  uniqueCallIds : s.UniqueCallIds
  noToolsBeforeProcessing : s.NoToolsBeforeProcessing
  noDuplicateForegroundLive : s.invFG

/-- The initial composed state is globally well-formed. -/
theorem initial_wellFormed : initial.WellFormed where
  allToolsCoherent := initial_allToolsCoherent
  allToolsLinked := initial_allToolsLinked
  uniqueCallIds := initial_uniqueCallIds
  noToolsBeforeProcessing := initial_noToolsBeforeProcessing
  noDuplicateForegroundLive := initial_invFG

/-- Global composed-state well-formedness is preserved by every transition. -/
theorem wellFormed_preserved
    {pre post : ComposedState}
    (h_wf : pre.WellFormed)
    (h_step : Transition pre post) :
    post.WellFormed := by
  have h_post_coherent :
      post.AllToolsCoherent :=
    allToolsCoherent_preserved
      h_wf.allToolsCoherent h_wf.noToolsBeforeProcessing h_step
  exact
    { allToolsCoherent := h_post_coherent
      allToolsLinked := allToolsLinked_of_allToolsCoherent h_post_coherent
      uniqueCallIds := uniqueCallIds_preserved h_wf.uniqueCallIds h_step
      noToolsBeforeProcessing :=
        noToolsBeforeProcessing_preserved h_wf.noToolsBeforeProcessing h_step
      noDuplicateForegroundLive :=
        invFG_preserved h_wf.noDuplicateForegroundLive h_step }

/-- Global composed-state well-formedness is preserved along traces. -/
theorem wellFormed_trace
    {pre post : ComposedState}
    (h_wf : pre.WellFormed)
    (h_trace : Trace pre post) :
    post.WellFormed := by
  induction h_trace with
  | refl => exact h_wf
  | step h_step _ ih =>
    exact ih (wellFormed_preserved h_wf h_step)

/-- Any state reachable from `initial` by a composed trace is well-formed. -/
theorem wellFormed_from_initial
    {post : ComposedState}
    (h_trace : Trace initial post) :
    post.WellFormed :=
  wellFormed_trace initial_wellFormed h_trace

end ComposedState
