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
    · rw [h_post_child.2.2.2.1]; exact h_depth
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
    (h_child_term : pre.child.request.state = .failed ∨
                    pre.child.request.state = .dead ∨
                    pre.child.request.state = .interrupted ∨
                    pre.child.request.state = .superseded) :
    ∃ post, Trace pre post ∧
            ∃ t ∈ post.parent.tools,
              t.callId = pre.bridgeCallId ∧
              (t.state = .failed ∨ t.state = .cancelled) := by
  -- Pull out the running bridge tool and its index in pre.parent.tools.
  obtain ⟨tPre, h_in, h_id, h_run_state, h_child_id⟩ := h_running
  obtain ⟨idx, h_idx⟩ := List.mem_iff_getElem?.mp h_in
  -- Project the child terminal to a parent-tool terminal:
  --   .interrupted → .cancelled  (child interrupted, parent cancels in sympathy)
  --   everything else → .failed
  -- Defined as a function on RequestState so that elaboration stays at the
  -- `Type` level (matching on `Or` would be a Prop→Type elimination).
  let projectFn : RequestState → ToolExecution.ToolCallState
    | .interrupted => .cancelled
    | _            => .failed
  let projectedState : ToolExecution.ToolCallState := projectFn pre.child.request.state
  let tPost : ToolExecution.ToolCallContext := { tPre with state := projectedState }
  let postParent : ComposedState :=
    { pre.parent with tools := pre.parent.tools.set idx tPost }
  let post : BridgedState := { pre with parent := postParent }
  -- Index bound for `List.mem_set`.
  have h_lt : idx < pre.parent.tools.length :=
    (List.getElem?_eq_some_iff.mp h_idx).1
  -- Recover the `bridge_failure` running-tool form (un-destructured, 4 conjuncts
  -- with the trailing childRequestId dropped — the constructor takes the
  -- 2-conjunct form just as B1 did with `h_running'`).
  have h_running' : ∃ t ∈ pre.parent.tools,
                       t.callId = pre.bridgeCallId ∧ t.state = .running :=
    ⟨tPre, h_in, h_id, h_run_state⟩
  -- The post bridge tool ∈ post.parent.tools (it's the .set element).
  have h_tPost_in : tPost ∈ postParent.tools :=
    List.mem_set pre.parent.tools idx h_lt tPost
  -- Show projectedState is .failed ∨ .cancelled (used both inside the
  -- constructor's h_post_tool and in the final goal). Case-split on the
  -- four-way child-terminal disjunction; for each, rewrite the request state
  -- and reduce `projectFn`.
  have h_proj : projectedState = .failed ∨ projectedState = .cancelled := by
    rcases h_child_term with h | h | h | h
    · left;  show projectFn pre.child.request.state = .failed
      rw [h]
    · left;  show projectFn pre.child.request.state = .failed
      rw [h]
    · right; show projectFn pre.child.request.state = .cancelled
      rw [h]
    · left;  show projectFn pre.child.request.state = .failed
      rw [h]
  refine ⟨post, ?_, tPost, h_tPost_in, ?_, ?_⟩
  · -- One-step trace via bridge_failure.
    refine Trace.step ?_ Trace.refl
    refine Transition.bridge_failure
      h_child_term
      h_running'
      ?_   -- h_post_tool
      ?_   -- h_others_eq
      rfl  -- h_request_eq      (post.parent.request = pre.parent.request)
      rfl  -- h_child_eq        (post.child = pre.child)
      rfl  -- h_bridgeId_eq     (post.bridgeCallId = pre.bridgeCallId)
      rfl  -- h_parent_id_eq    (post.parent.requestId = pre.parent.requestId)
    · -- h_post_tool: tPost ∈ post.parent.tools, callId, state ∈ {failed,cancelled},
      --              childRequestId = some pre.child.requestId.
      refine ⟨tPost, h_tPost_in, ?_, ?_, ?_⟩
      · -- tPost.callId = pre.bridgeCallId.
        show tPre.callId = pre.bridgeCallId
        exact h_id
      · -- tPost.state ∈ {.failed, .cancelled}.
        show projectedState = .failed ∨ projectedState = .cancelled
        exact h_proj
      · -- tPost.childRequestId = some pre.child.requestId.
        show tPre.childRequestId = some pre.child.requestId
        exact h_child_id
    · -- h_others_eq: every non-bridge tool in pre survives unchanged in post.
      intro t h_t_in h_t_ne
      show t ∈ postParent.tools
      have h_set_subset :
          ∀ x ∈ pre.parent.tools, x ≠ tPre →
            x ∈ pre.parent.tools.set idx tPost := by
        intro x hx hx_ne
        rcases List.mem_iff_getElem?.mp hx with ⟨j, hj⟩
        by_cases h_eq : j = idx
        · subst h_eq
          have : some x = some tPre := by rw [← hj, h_idx]
          exact absurd (Option.some.inj this) hx_ne
        · apply List.mem_iff_getElem?.mpr
          refine ⟨j, ?_⟩
          rw [List.getElem?_set_ne (by exact fun h => h_eq h.symm)]
          exact hj
      apply h_set_subset t h_t_in
      intro h_eq
      apply h_t_ne
      rw [h_eq]; exact h_id
  · -- tPost.callId = pre.bridgeCallId.
    show tPre.callId = pre.bridgeCallId
    exact h_id
  · -- tPost.state ∈ {.failed, .cancelled}.
    show projectedState = .failed ∨ projectedState = .cancelled
    exact h_proj

/-! ### B3: cascade cancellation correctness -/

/-- B3: Cascade cancellation correctness. Parent terminal under cascade ⇒
    child reaches `.interrupted` via two-step trace (`bridge_cancel_cascade`
    sets `interruptRequestedAt`; `child_step` lifts `interrupt_processing`).

    The child must be in `.processing` with admission `.executing` for
    `interrupt_processing` to fire. The `h_child_no_fg` hypothesis discharges
    the foreground-blocking guard on the inner `request_step`. The `h_linked`
    hypothesis is required at both `child_step` invocations so INV-LINK can
    be threaded; the link is preserved because all link-relevant fields
    (`requestId`, `causedByParentRequestId`, `causedByParentToolCallId`,
    `bridgeCallId`, parent state, `parent.tools`) are unchanged across both
    steps. -/
theorem cascade_cancels_child
    (pre : BridgedState)
    (h_parent_term : isTerminal pre.parent.request.state)
    (h_cascade     : ∃ t ∈ pre.parent.tools,
                       t.callId = pre.bridgeCallId ∧
                       t.cancelPolicy = .cascade ∧
                       ¬ isTerminal t.state)
    (h_child_proc      : pre.child.request.state = .processing)
    (h_child_admission : pre.child.request.admission = .executing)
    (h_child_no_fg     : ¬ ∃ t ∈ pre.child.tools, t.awaitMode = .foreground ∧
                                                    ¬ isTerminal t.state)
    (h_linked          : pre.linked) :
    ∃ post, Trace pre post ∧ post.child.request.state = .interrupted := by
  -- Pull out the cascade-mode tool witness; we use it in step 1.
  obtain ⟨tCascade, h_in, h_id, h_pol, _h_live⟩ := h_cascade
  -- Step 1: bridge_cancel_cascade sets interruptRequestedAt on child.
  let midChildReq : RequestContext :=
    { pre.child.request with
        interruptRequestedAt := some pre.child.request.currentTime }
  let midChild : ComposedState :=
    { pre.child with request := midChildReq }
  let mid : BridgedState := { pre with child := midChild }
  -- Step 2: child_step lifts interrupt_processing.
  let postChildReq : RequestContext :=
    { midChildReq with state := .interrupted, admission := .released }
  let postChild : ComposedState :=
    { midChild with request := postChildReq }
  let post : BridgedState := { mid with child := postChild }
  refine ⟨post, ?_, ?_⟩
  · -- Build the two-step trace, providing the intermediate state explicitly.
    refine @Trace.step pre mid post ?_ (@Trace.step mid post post ?_ Trace.refl)
    · -- Step 1: bridge_cancel_cascade.
      refine Transition.bridge_cancel_cascade
        (Or.inl h_parent_term)        -- h_parent_term : terminal disjunct
        ⟨tCascade, h_in, h_id, h_pol⟩  -- h_cascade_pol
        ?_                             -- h_interrupt_set
        rfl                            -- h_parent_eq
        rfl                            -- h_bridgeId_eq
        rfl                            -- h_child_id_eq
        ?_                             -- h_child_caused_req_eq
        ?_                             -- h_child_caused_tool_eq
        ?_                             -- h_child_depth_eq
      · -- mid.child.request.interruptRequestedAt.isSome.
        show midChildReq.interruptRequestedAt.isSome
        simp [midChildReq]
      · -- causedByParentRequestId preserved (only interruptRequestedAt changed).
        show midChildReq.causedByParentRequestId = pre.child.request.causedByParentRequestId
        rfl
      · show midChildReq.causedByParentToolCallId = pre.child.request.causedByParentToolCallId
        rfl
      · show midChildReq.subagentDepth = pre.child.request.subagentDepth
        rfl
    · -- Step 2: child_step lifting RequestContext.Transition.interrupt_processing.
      -- mid.linked first.
      have h_link_mid : mid.linked := by
        -- pre.linked has parentLink (about pre.parent) and childLink (about
        -- pre.child.request.causedByParentRequestId / ToolCallId). For mid:
        --   mid.parent = pre.parent, mid.bridgeCallId = pre.bridgeCallId,
        --   mid.child.request differs from pre.child.request only on
        --   interruptRequestedAt; the link-relevant fields are unchanged.
        -- So pre.linked carries straight through.
        obtain ⟨h_pLink, h_cReq, h_cTool⟩ := h_linked
        refine ⟨h_pLink, ?_, ?_⟩
        · show midChildReq.causedByParentRequestId = some pre.parent.requestId
          exact h_cReq
        · show midChildReq.causedByParentToolCallId = some pre.bridgeCallId
          exact h_cTool
      have h_link_post : post.linked := by
        -- post.parent = mid.parent, post.bridgeCallId = mid.bridgeCallId,
        -- post.child.request changes only state and admission; lineage and
        -- requestId are inherited from midChildReq, hence from pre.
        obtain ⟨h_pLink, h_cReq, h_cTool⟩ := h_link_mid
        refine ⟨h_pLink, ?_, ?_⟩
        · show postChildReq.causedByParentRequestId = some mid.parent.requestId
          exact h_cReq
        · show postChildReq.causedByParentToolCallId = some mid.bridgeCallId
          exact h_cTool
      -- Inner request transition on the child: interrupt_processing.
      have h_inner_req :
          RequestContext.Transition mid.child.request post.child.request := by
        show RequestContext.Transition midChildReq postChildReq
        refine RequestContext.Transition.interrupt_processing ?_ ?_ ?_ ?_
        · -- midChildReq.state = .processing (only interruptRequestedAt changed).
          show pre.child.request.state = .processing
          exact h_child_proc
        · -- midChildReq.admission = .executing.
          show pre.child.request.admission = .executing
          exact h_child_admission
        · -- midChildReq.interruptRequestedAt.isSome.
          show (some pre.child.request.currentTime).isSome
          rfl
        · -- post = { mid with state := .interrupted, admission := .released }.
          rfl
      -- Inner ComposedState transition on the child: request_step.
      have h_inner_composed :
          ComposedState.Transition mid.child post.child := by
        refine ComposedState.Transition.request_step
          h_inner_req
          rfl  -- post.process = pre.process (i.e., midChild.process)
          rfl  -- post.call = pre.call
          rfl  -- post.tools = pre.tools
          rfl  -- post.requestId = pre.requestId
          ?_   -- pending → acceptsWork (vacuous: state = .processing)
          ?_   -- INV-FG guard
        · -- pending gate: midChildReq.state = .processing, not .pending.
          intro h_pending
          exfalso
          have h_eq : midChildReq.state = pre.child.request.state := rfl
          rw [h_eq, h_child_proc] at h_pending
          cases h_pending
        · -- INV-FG guard: interrupt_processing doesn't increase progressSeq
          -- and isn't claimed → processing, so the antecedent is false.
          -- More precisely: postChildReq.progressSeq = midChildReq.progressSeq
          -- (only state and admission change), and midChildReq.state =
          -- .processing ≠ .claimed.
          intro h_advance
          exfalso
          rcases h_advance with h_progress | ⟨h_claimed, _⟩
          · -- progressSeq strictly increases? No: postChildReq.progressSeq
            -- equals midChildReq.progressSeq (record update doesn't touch it).
            have h_eq_seq : postChildReq.progressSeq = midChildReq.progressSeq := rfl
            rw [h_eq_seq] at h_progress
            exact Nat.lt_irrefl _ h_progress
          · -- mid.child.request.state = .claimed? No: it's .processing.
            have h_eq_state : midChildReq.state = pre.child.request.state := rfl
            rw [h_eq_state, h_child_proc] at h_claimed
            cases h_claimed
      -- Lift through child_step.
      refine Transition.child_step
        h_inner_composed
        rfl                  -- h_parent_eq: post.parent = mid.parent
        rfl                  -- h_bridgeId_eq
        h_link_mid           -- h_link_pre
        h_link_post          -- h_link_post
  · -- post.child.request.state = .interrupted.
    show postChildReq.state = .interrupted
    rfl

/-! ### B3': detach does not cascade -/

/-- B3': Detach correctness (negative form). A detach-mode bridge tool's
    cancellation does NOT cascade to the child. Specifically: under any
    single-step transition `pre → post`, if the parent's bridge tool has
    `cancelPolicy = .detach`, the child's `interruptRequestedAt` flag is
    preserved.

    The `h_no_other` hypothesis says the pre-state has no interrupt set,
    blocking child-side `interrupt_processing` / `interrupt_claimed` /
    `interrupt_before_claim` arms (their `pre.interruptRequestedAt.isSome`
    guards fail). The `h_unique` hypothesis (callId uniqueness within
    `pre.parent.tools`) discharges the `bridge_cancel_cascade` arm by
    contradicting `cancelPolicy = .cascade` against `cancelPolicy = .detach`
    on the same tool. -/
theorem detach_does_not_cancel_child
    (pre post : BridgedState)
    (h_detach    : ∃ t ∈ pre.parent.tools,
                     t.callId = pre.bridgeCallId ∧ t.cancelPolicy = .detach)
    (h_step      : Transition pre post)
    (h_no_other  : ¬ pre.child.request.interruptRequestedAt.isSome)
    (h_unique    : ∀ t₁ ∈ pre.parent.tools, ∀ t₂ ∈ pre.parent.tools,
                     t₁.callId = pre.bridgeCallId →
                     t₂.callId = pre.bridgeCallId →
                     t₁ = t₂) :
    post.child.request.interruptRequestedAt =
      pre.child.request.interruptRequestedAt := by
  cases h_step with
  | parent_step _ h_child_eq _ _ _ =>
    -- Parent-only step: child unchanged.
    rw [h_child_eq]
  | child_step h_inner _ _ _ _ =>
    -- A child-side ComposedState step. The only inner constructors that can
    -- touch `interruptRequestedAt` are within `request_step` (the request
    -- layer); other layers (process_step, call_step, tool_step, persistence_step)
    -- preserve `request` directly. Within `request_step`, every inner
    -- RequestContext.Transition either preserves `interruptRequestedAt`
    -- (most arms only update `state`/`admission`) or has a precondition on
    -- the *current* `interruptRequestedAt` flag — namely `interrupt_*` arms,
    -- which require `pre.interruptRequestedAt.isSome`, contradicting
    -- `h_no_other`. The `interrupt_*` arms set the post state to .interrupted
    -- but the timestamp itself is *preserved* by the record update.
    --
    -- Concretely: every constructor produces `post = { pre with ... }` where
    -- the `...` does not mention `interruptRequestedAt`, so post.iRA = pre.iRA.
    cases h_inner with
    | process_step _ h_req _ _ _ =>
      rw [h_req]
    | request_step h_req_inner _ _ _ _ _ _ =>
      -- Case-split on the inner RequestContext.Transition; every arm has a
      -- record-update post that preserves `interruptRequestedAt`.
      cases h_req_inner with
      | claim _ _ _ h_post =>
        rw [h_post]
      | dedup_lose _ _ h_post =>
        rw [h_post]
      | begin_inference _ _ h_post =>
        rw [h_post]
      | advance _ _ h_post =>
        rw [h_post]
      | finish _ _ h_post =>
        rw [h_post]
      | fail _ _ h_post =>
        rw [h_post]
      | fail_before_stream _ _ h_post =>
        rw [h_post]
      | expire _ _ _ _ h_post =>
        rw [h_post]
      | interrupt_before_claim _ _ _ h_post =>
        rw [h_post]
      | interrupt_claimed _ _ _ h_post =>
        rw [h_post]
      | interrupt_processing _ _ _ h_post =>
        rw [h_post]
    | persistence_step _ _ _ h_req _ _ _ _ =>
      rw [h_req]
    | call_step _ h_req _ _ _ =>
      rw [h_req]
    | tool_step _ _ _ h_req _ _ _ _ =>
      rw [h_req]
  | bridge_spawn h_parent_proc _ _ h_post_child h_request_eq _ =>
    -- bridge_spawn now guarantees post.child.request.interruptRequestedAt = none
    -- (the new conjunct in h_post_child). Under h_no_other, the pre-state also
    -- has interruptRequestedAt = none (proved by cases on the Option). Both
    -- sides are none, so they are equal.
    have h_post_none : post.child.request.interruptRequestedAt = none :=
      h_post_child.2.2.2.2
    have h_pre_none : pre.child.request.interruptRequestedAt = none := by
      cases h : pre.child.request.interruptRequestedAt with
      | none => rfl
      | some _ => simp [h] at h_no_other
    rw [h_post_none, h_pre_none]
  | bridge_complete _ _ _ _ _ _ h_child_eq _ _ =>
    rw [h_child_eq]
  | bridge_failure _ _ _ _ _ h_child_eq _ _ =>
    rw [h_child_eq]
  | bridge_cancel_cascade _ h_cascade _ _ _ _ _ _ _ =>
    -- Bridge tool with cascade policy contradicts h_detach (the same callId
    -- carries cancelPolicy = .detach). Use h_unique to identify the tools.
    obtain ⟨tDet, h_in_d, h_id_d, h_pol_d⟩ := h_detach
    obtain ⟨tCas, h_in_c, h_id_c, h_pol_c⟩ := h_cascade
    have h_same_tool : tDet = tCas :=
      h_unique tDet h_in_d tCas h_in_c h_id_d h_id_c
    -- Now h_pol_d says tDet.cancelPolicy = .detach and h_pol_c says
    -- tCas.cancelPolicy = .cascade. With tDet = tCas, we derive .detach = .cascade.
    rw [h_same_tool] at h_pol_d
    rw [h_pol_c] at h_pol_d
    -- h_pol_d : CancelPolicy.cascade = CancelPolicy.detach. Contradiction.
    cases h_pol_d

/-! ### B6: foreground blocks parent advance -/

/-- B6: A live foreground tool on the parent prevents the parent's
    `progressSeq` and `messageSeq` from advancing across any single bridge
    `Transition`. Restates the `no_blocking_foreground` guard at the
    BridgedState layer. -/
theorem foreground_blocks_parent_advance
    (pre post : BridgedState)
    (h_fg     : ∃ t ∈ pre.parent.tools,
                  t.awaitMode = .foreground ∧
                  ¬ isTerminal t.state)
    (h_step   : Transition pre post) :
    pre.parent.request.progressSeq = post.parent.request.progressSeq ∧
    pre.parent.request.messageSeq  = post.parent.request.messageSeq := by
  cases h_step with
  | parent_step h_inner h_child_eq h_bridge_eq _ _ =>
    -- Inner ComposedState.Transition. Case-split.
    cases h_inner with
    | request_step h_req _ _ h_tools _ _ h_no_block =>
      -- The h_no_block guard says: if reqPost.progressSeq > pre.parent.request.progressSeq
      -- OR (claimed → processing), THEN no live foreground tool. h_fg directly contradicts
      -- the consequent. So the antecedent must be false: progressSeq cannot increase
      -- and we cannot transition claimed → processing. Case-split on h_req.
      cases h_req with
      | claim _ _ _ h_post =>
        constructor <;> rw [h_post]
      | dedup_lose _ _ h_post =>
        constructor <;> rw [h_post]
      | begin_inference h_pre_claimed _ h_post =>
        -- begin_inference transitions claimed → processing; h_no_block fires
        -- via Or.inr ⟨pre.state = .claimed, post.state = .processing⟩, giving
        -- ¬ ∃ live foreground tool, contradicting h_fg.
        exfalso
        apply h_no_block
        · refine Or.inr ⟨h_pre_claimed, ?_⟩
          rw [h_post]
        · -- Reconstruct h_fg in pre.tools (which equals pre.parent.tools).
          exact h_fg
      | advance _ _ h_post =>
        -- advance increases progressSeq; h_no_block fires via Or.inl, contradicting h_fg.
        exfalso
        apply h_no_block
        · refine Or.inl ?_
          rw [h_post]
          exact Nat.lt_succ_self _
        · exact h_fg
      | finish _ _ h_post =>
        constructor <;> rw [h_post]
      | fail _ _ h_post =>
        constructor <;> rw [h_post]
      | fail_before_stream _ _ h_post =>
        constructor <;> rw [h_post]
      | expire _ _ _ _ h_post =>
        constructor <;> rw [h_post]
      | interrupt_before_claim _ _ _ h_post =>
        constructor <;> rw [h_post]
      | interrupt_claimed _ _ _ h_post =>
        constructor <;> rw [h_post]
      | interrupt_processing _ _ _ h_post =>
        constructor <;> rw [h_post]
    | tool_step _ _ _ h_req_eq _ _ _ _ =>
      constructor <;> rw [h_req_eq]
    | process_step _ h_req _ _ _ =>
      constructor <;> rw [h_req]
    | persistence_step _ _ _ h_req _ _ _ _ =>
      constructor <;> rw [h_req]
    | call_step _ h_req _ _ _ =>
      constructor <;> rw [h_req]
  | child_step _ h_parent_eq _ _ _ =>
    constructor <;> rw [h_parent_eq]
  | bridge_spawn _ _ _ _ h_request_eq _ =>
    constructor <;> rw [h_request_eq]
  | bridge_complete _ _ _ _ _ h_request_eq _ _ _ =>
    constructor <;> rw [h_request_eq]
  | bridge_failure _ _ _ _ h_request_eq _ _ _ =>
    constructor <;> rw [h_request_eq]
  | bridge_cancel_cascade _ _ _ h_parent_eq _ _ _ _ _ =>
    constructor <;> rw [h_parent_eq]

/-- B4: Subagent depth bound. Restated standalone for prominence; alias of `inv_depth`. -/
theorem subagent_depth_bounded
    (pre post : BridgedState)
    (h_init  : pre.parent.request.subagentDepth ≤ maxSubagentDepth ∧
               pre.child.request.subagentDepth ≤ maxSubagentDepth)
    (h_trace : Trace pre post) :
    post.parent.request.subagentDepth ≤ maxSubagentDepth ∧
    post.child.request.subagentDepth ≤ maxSubagentDepth :=
  inv_depth pre post h_init h_trace

/-- B5: Bridge link symmetry. Restated standalone for prominence; alias of `inv_link`. -/
theorem bridge_link_symmetric
    (pre post : BridgedState)
    (h_init  : pre.linked)
    (h_trace : Trace pre post) :
    post.linked :=
  inv_link pre post h_init h_trace

end BridgedState
end Subagent
