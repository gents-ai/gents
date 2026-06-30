import Proofs.Background.Transition

/-! Cascade and detach cancellation properties for bridged subagents. -/

namespace Subagent
namespace BridgedState

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
  let mid : BridgedState := { pre with child := midChild, secondLeg := .subagent midChild }
  -- Step 2: child_step lifts interrupt_processing.
  let postChildReq : RequestContext :=
    { midChildReq with state := .interrupted, admission := .released }
  let postChild : ComposedState :=
    { midChild with request := postChildReq }
  let post : BridgedState := { mid with child := postChild, secondLeg := .subagent postChild }
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
        rfl                            -- h_child_tools_eq
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

/-! ### INV-UNIQUE: BridgedState-level lift

The BridgedState-level `bridgedUniqueCallIds_preserved` theorem (above)
proves that `ComposedState.UniqueCallIds` is preserved on both parent
and child across any reachable bridge `Trace`. Per-step preservation
relies on the set-style descriptions of `post.parent.tools` carried by
`bridge_complete`, `bridge_failure`, and `bridge_spawn` (the latter via
append + a freshness precondition on the new tool's callId).
-/

/-! ### B3': detach does not cascade -/

/-- B3': Detach correctness (negative form). A detach-mode bridge tool's
    cancellation does NOT cascade to the child. Specifically: under any
    single-step transition `pre → post`, if the parent's bridge tool has
    `cancelPolicy = .detach`, the child's `interruptRequestedAt` flag is
    preserved.

    The `h_no_other` hypothesis says the pre-state has no interrupt set,
    blocking child-side `interrupt_processing` / `interrupt_claimed` /
    `interrupt_before_claim` arms (their `pre.interruptRequestedAt.isSome`
    guards fail). The `h_uniq` hypothesis (`UniqueCallIds` — callIds are
    distinct within `pre.parent.tools`) discharges the `bridge_cancel_cascade`
    arm by deriving same-tool from same-callId, then contradicting
    `cancelPolicy = .cascade` against `cancelPolicy = .detach`.

    `h_uniq` is a structural invariant of any reachable composed state
    (proved via `ComposedState.uniqueCallIds_preserved`), so this theorem is
    a property of any reachable state, not a conditional one. -/
theorem detach_does_not_cancel_child
    (pre post : BridgedState)
    (h_detach    : ∃ t ∈ pre.parent.tools,
                     t.callId = pre.bridgeCallId ∧ t.cancelPolicy = .detach)
    (h_step      : Transition pre post)
    (h_no_other  : ¬ pre.child.request.interruptRequestedAt.isSome)
    (h_uniq      : pre.parent.UniqueCallIds) :
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
    | tool_spawn _ _ _ h_req _ _ _ _ _ _ =>
      rw [h_req]
    | tool_step _ _ _ h_req _ _ _ _ _ =>
      rw [h_req]
  | bridge_spawn h_parent_proc _ _ _ _ _ h_post_child _ h_request_eq _ _ =>
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
  | bridge_complete _ _ _ _ _ _ _ _ _ _ _ h_child_eq _ _ =>
    rw [h_child_eq]
  | bridge_failure _ _ _ _ _ _ _ _ _ _ h_child_eq _ _ =>
    rw [h_child_eq]
  | bridge_cancel_cascade _ h_cascade _ _ _ _ _ _ _ _ =>
    -- Bridge tool with cascade policy contradicts h_detach (the same callId
    -- carries cancelPolicy = .detach). Use h_uniq (UniqueCallIds) to identify
    -- the two tools as equal: both carry callId = pre.bridgeCallId.
    obtain ⟨tDet, h_in_d, h_id_d, h_pol_d⟩ := h_detach
    obtain ⟨tCas, h_in_c, h_id_c, h_pol_c⟩ := h_cascade
    have h_callIds : tDet.callId = tCas.callId := by rw [h_id_d, h_id_c]
    have h_same_tool : tDet = tCas :=
      ComposedState.UniqueCallIds.eq_of_callId_eq h_uniq h_in_d h_in_c h_callIds
    -- Now h_pol_d says tDet.cancelPolicy = .detach and h_pol_c says
    -- tCas.cancelPolicy = .cascade. With tDet = tCas, we derive .detach = .cascade.
    rw [h_same_tool] at h_pol_d
    rw [h_pol_c] at h_pol_d
    -- h_pol_d : CancelPolicy.cascade = CancelPolicy.detach. Contradiction.
    cases h_pol_d

end BridgedState
end Subagent
