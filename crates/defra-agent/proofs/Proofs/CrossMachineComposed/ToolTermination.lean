import Proofs.CrossMachineComposed.State

namespace ComposedState

/-!
## Interrupted requests and inference-call cancellation

The composed model includes a first-class `InferenceCall` state machine.
-/

/--
If a request is already interrupted, every live `InferenceCall` linked by
`request_id` has a valid composed trace to persisted `cancelled`.
-/
theorem interrupted_request_cancels_live_linked_call
    {pre : ComposedState}
    (h_interrupted : pre.request.state = .interrupted)
    (h_linked : pre.call.linkedTo pre.requestId)
    (h_live : pre.call.cancellable) :
    ∃ post : ComposedState,
      Trace pre post ∧
      post.request = pre.request ∧
      post.request.state = .interrupted ∧
      post.call.linkedTo pre.requestId ∧
      post.call.state = .cancelled ∧
      InferenceCall.Trace pre.call post.call := by
  let postCall := pre.call.cancel
  let post : ComposedState := { pre with call := postCall }
  have h_call_trace : InferenceCall.Trace pre.call postCall :=
    InferenceCall.live_trace_to_cancelled pre.call h_live
  have h_call_step : InferenceCall.Transition pre.call postCall := by
    cases h_live with
    | inl h_queued =>
        exact InferenceCall.cancel_before_stream_transition h_queued rfl
    | inr h_running =>
        exact InferenceCall.cancel_during_stream_transition h_running rfl
  have h_step : Transition pre post := by
    exact Transition.call_step h_call_step rfl rfl rfl rfl
  refine ⟨post, Trace.step h_step Trace.refl, rfl, ?_, ?_, ?_, h_call_trace⟩
  · exact h_interrupted
  · unfold InferenceCall.linkedTo
    change (pre.call.cancel).requestId = pre.requestId
    rw [InferenceCall.cancel_preserves_requestId pre.call]
    exact h_linked
  · exact InferenceCall.cancel_state pre.call


/-- C2: An interrupted request cancels every live linked tool call.

The witness preserves the parent request and re-emits its `.interrupted`
state before the tool-membership/cancelled/linkage facts. The inner tool
transition is tagged with `CancelCause.interrupted`; a future composed
`CauseCoherent` invariant can make that tag/state agreement global. -/
theorem interrupted_request_cancels_live_linked_tools
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_in           : toolPre ∈ pre.tools)
    (h_interrupted  : pre.request.state = .interrupted)
    (h_live         : toolPre.cancellable)
    (h_coherent     : Coherent pre toolPre) :
    ∃ post toolPost,
      Trace pre post ∧
      post.request = pre.request ∧
      post.request.state = .interrupted ∧
      toolPost ∈ post.tools ∧
      toolPost.state = .cancelled ∧
      toolPost.requestId = pre.requestId := by
  obtain ⟨h_linked, h_deadline_eq, h_time_eq⟩ := h_coherent
  obtain ⟨idx, h_idx⟩ := List.mem_iff_getElem?.mp h_in
  -- Case-split on cancellable: pending → cancelBeforeDispatch; running → cancelDuringRun
  cases h_live with
  | inl h_pending =>
    have h_t_step : ToolExecution.ToolCallContext.Transition toolPre
                      { toolPre with state := .cancelled } :=
      ToolExecution.ToolCallContext.Transition.cancelBeforeDispatch .interrupted h_pending rfl
    let toolPost : ToolExecution.ToolCallContext := { toolPre with state := .cancelled }
    let post : ComposedState := { pre with tools := pre.tools.set idx toolPost }
    refine ⟨post, toolPost, ?_, rfl, h_interrupted, ?_, rfl, h_linked⟩
    · refine Trace.step ?_ Trace.refl
      -- Discharge the foreground-flip guard: `cancelBeforeDispatch` only changes
      -- `state`, so `toolPost.awaitMode = toolPre.awaitMode`. If `toolPre` is
      -- background, `toolPost` is too — the consequent is unreachable.
      refine Transition.tool_step h_idx h_t_step rfl rfl rfl rfl rfl
              ⟨h_linked, h_deadline_eq, h_time_eq⟩
              (fun h_bg h_fg => ?_)
      simp [toolPost, h_bg] at h_fg
    · have h_lt : idx < pre.tools.length := (List.getElem?_eq_some_iff.mp h_idx).1
      simpa [post, toolPost] using List.mem_set pre.tools idx h_lt toolPost
  | inr h_running =>
    have h_t_step : ToolExecution.ToolCallContext.Transition toolPre
                      { toolPre with state := .cancelled } :=
      ToolExecution.ToolCallContext.Transition.cancelDuringRun .interrupted h_running rfl
    let toolPost : ToolExecution.ToolCallContext := { toolPre with state := .cancelled }
    let post : ComposedState := { pre with tools := pre.tools.set idx toolPost }
    refine ⟨post, toolPost, ?_, rfl, h_interrupted, ?_, rfl, h_linked⟩
    · refine Trace.step ?_ Trace.refl
      refine Transition.tool_step h_idx h_t_step rfl rfl rfl rfl rfl
              ⟨h_linked, h_deadline_eq, h_time_eq⟩
              (fun h_bg h_fg => ?_)
      simp [toolPost, h_bg] at h_fg
    · have h_lt : idx < pre.tools.length := (List.getElem?_eq_some_iff.mp h_idx).1
      simpa [post, toolPost] using List.mem_set pre.tools idx h_lt toolPost


/-- C1: A request whose deadline is exceeded times out every Running linked
    tool. Multi-flight form: any tool ∈ pre.tools that is running, linked, and
    deadline-synced reaches .timedOut. The composition theorem whose absence
    in the runtime caused issue #149. -/
theorem deadline_exceeded_request_timesOut_running_tools
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_in        : toolPre ∈ pre.tools)
    (h_running   : toolPre.state = .running)
    (h_coherent  : Coherent pre toolPre)
    (h_deadline  : pre.request.deadlineExceeded) :
    ∃ post toolPost,
      Trace pre post ∧
      toolPost ∈ post.tools ∧
      post.request = pre.request ∧
      toolPost.state = .timedOut ∧
      toolPost.requestId = pre.requestId := by
  -- Destructure h_coherent into its three component equalities.
  obtain ⟨h_linked, h_deadline_eq, h_time_eq⟩ := h_coherent
  -- Find the index of toolPre in pre.tools.
  obtain ⟨idx, h_idx⟩ := List.mem_iff_getElem?.mp h_in
  -- Apply the inner ToolCallContext.timeout transition on toolPre.
  have h_t_step : ToolExecution.ToolCallContext.Transition toolPre
                    { toolPre with state := .timedOut } := by
    refine ToolExecution.ToolCallContext.Transition.timeout
      h_running ?_ rfl
    -- discharge deadlineExceeded for toolPre using h_coherent + h_deadline
    show toolPre.currentTime > toolPre.deadline
    rw [h_deadline_eq, h_time_eq]
    exact h_deadline
  -- Construct the post composed state by setting idx to the timed-out tool.
  let toolPost : ToolExecution.ToolCallContext := { toolPre with state := .timedOut }
  let post : ComposedState := { pre with tools := pre.tools.set idx toolPost }
  refine ⟨post, toolPost, ?_, ?_, rfl, rfl, h_linked⟩
  · -- One-step trace via tool_step
    refine Trace.step ?_ Trace.refl
    -- Pass h_coherent directly as the Coherent witness; discharge the
    -- foreground-flip guard vacuously (timeout preserves awaitMode).
    refine Transition.tool_step h_idx h_t_step rfl rfl rfl rfl rfl
      ⟨h_linked, h_deadline_eq, h_time_eq⟩
      (fun h_bg h_fg => ?_)
    simp [toolPost, h_bg] at h_fg
  · -- toolPost ∈ post.tools — follows from `pre.tools.set idx toolPost`
    show toolPost ∈ pre.tools.set idx toolPost
    have h_lt : idx < pre.tools.length :=
      (List.getElem?_eq_some_iff.mp h_idx).1
    exact List.mem_set pre.tools idx h_lt toolPost


/-- C1': A request whose deadline is exceeded cancels every Pending linked
    tool. Companion to C1 — a Pending tool never ran, so it reaches
    .cancelled rather than .timedOut. The inner cancellation is tagged with
    `.deadline`, tying this composed path to the deadline hypothesis even
    though the single-machine pending-cancel transition itself has no deadline
    guard.

    The witness preserves the parent request and re-emits
    `post.request.deadlineExceeded` before the tool-membership/cancelled/linkage
    facts. -/
theorem deadline_exceeded_request_cancels_pending_tools
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_in        : toolPre ∈ pre.tools)
    (h_pending   : toolPre.state = .pending)
    (h_deadline  : pre.request.deadlineExceeded)
    (h_coherent  : Coherent pre toolPre) :
    ∃ post toolPost,
      Trace pre post ∧
      post.request = pre.request ∧
      post.request.deadlineExceeded ∧
      toolPost ∈ post.tools ∧
      toolPost.state = .cancelled ∧
      toolPost.requestId = pre.requestId := by
  obtain ⟨h_linked, h_deadline_eq, h_time_eq⟩ := h_coherent
  obtain ⟨idx, h_idx⟩ := List.mem_iff_getElem?.mp h_in
  -- Inner cancelBeforeDispatch transition
  have h_t_step : ToolExecution.ToolCallContext.Transition toolPre
                    { toolPre with state := .cancelled } :=
    ToolExecution.ToolCallContext.Transition.cancelBeforeDispatch .deadline h_pending rfl
  let toolPost : ToolExecution.ToolCallContext := { toolPre with state := .cancelled }
  let post : ComposedState := { pre with tools := pre.tools.set idx toolPost }
  refine ⟨post, toolPost, ?_, rfl, h_deadline, ?_, rfl, h_linked⟩
  · refine Trace.step ?_ Trace.refl
    refine Transition.tool_step h_idx h_t_step rfl rfl rfl rfl rfl
            ⟨h_linked, h_deadline_eq, h_time_eq⟩
            (fun h_bg h_fg => ?_)
    simp [toolPost, h_bg] at h_fg
  · -- toolPost ∈ post.tools via List.mem_set
    have h_lt : idx < pre.tools.length := (List.getElem?_eq_some_iff.mp h_idx).1
    simpa [post, toolPost] using List.mem_set pre.tools idx h_lt toolPost


/-- C3: A request whose linked tools are all terminal can resume making progress.
    Multi-flight form: ∀-quantified over `pre.tools`. Semantic complement of
    issue #149: when every linked tool is terminal, the foreground-blocking
    guard on `request_step` is satisfied and the request can `advance`.

    This theorem performs no tool-call cancellation, so `CancelCause` has no
    natural role here; future causal audit work should introduce a separate
    progress-unblock witness rather than forcing a cancel cause into C3. -/
theorem all_tools_terminal_unblocks_request_progress
    {pre : ComposedState}
    (h_all_terminal : ∀ t ∈ pre.tools, isTerminal t.state)
    (h_proc         : pre.request.state = .processing)
    (h_admission    : pre.request.admission = .executing) :
    ∃ post,
      Transition pre post ∧
      RequestContext.Transition pre.request post.request := by
  -- Build the post-state by firing `advance` on the request layer.
  let postReq : RequestContext :=
    { pre.request with progressSeq := pre.request.progressSeq + 1 }
  let post : ComposedState := { pre with request := postReq }
  -- Inner advance transition.
  have h_inner : RequestContext.Transition pre.request postReq :=
    RequestContext.Transition.advance h_proc h_admission rfl
  refine ⟨post, ?_, h_inner⟩
  -- Build the request_step lift. After Task 11, request_step takes:
  --   h_req, then 4 cross-layer rfls (process, call, tools, requestId),
  --   then the pending→acceptsWork gate, then h_no_block.
  refine Transition.request_step h_inner rfl rfl rfl rfl ?_ ?_
  · -- Pending gate: pre.request.state = .processing, not .pending — the
    -- antecedent is false, so the implication is vacuous.
    intro h_pending
    rw [h_proc] at h_pending
    cases h_pending
  · -- Discharge h_no_block: any candidate live-foreground tool is contradicted
    -- by h_all_terminal directly.
    intro _h_advance
    intro ⟨t, h_in, _h_fg, h_nt⟩
    exact h_nt (h_all_terminal t h_in)

end ComposedState
