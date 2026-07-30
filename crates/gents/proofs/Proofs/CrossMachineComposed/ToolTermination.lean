import Proofs.CrossMachineComposed.WellFormed

namespace ComposedState

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

theorem interrupted_request_cancels_live_linked_tools
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_in           : toolPre ∈ pre.tools)
    (h_wf           : pre.WellFormed)
    (h_live_kind    : ¬ IsDetached toolPre)
    (h_interrupted  : pre.request.state = .interrupted)
    (h_live         : toolPre.cancellable) :
    ∃ post toolPost,
      Trace pre post ∧
      post.request = pre.request ∧
      post.request.state = .interrupted ∧
      toolPost ∈ post.tools ∧
      toolPost.state = .cancelled ∧
      toolPost.requestId = pre.requestId := by
  have h_coherent : Coherent pre toolPre :=
    h_wf.allToolsCoherent toolPre h_in h_live_kind
  obtain ⟨h_linked, h_deadline_eq, h_time_eq⟩ := h_coherent
  obtain ⟨idx, h_idx⟩ := List.mem_iff_getElem?.mp h_in
  cases h_live with
  | inl h_pending =>
    have h_t_step : ToolExecution.ToolCallContext.Transition toolPre
                      { toolPre with state := .cancelled } :=
      ToolExecution.ToolCallContext.Transition.cancelBeforeDispatch .interrupted h_pending rfl
    let toolPost : ToolExecution.ToolCallContext := { toolPre with state := .cancelled }
    let post : ComposedState := { pre with tools := pre.tools.set idx toolPost }
    refine ⟨post, toolPost, ?_, rfl, h_interrupted, ?_, rfl, h_linked⟩
    · refine Trace.step ?_ Trace.refl
      refine Transition.tool_step h_idx h_t_step rfl rfl rfl rfl rfl
              ⟨h_linked, h_deadline_eq, h_time_eq⟩
              ⟨h_linked, h_deadline_eq, h_time_eq⟩
              (fun h_det =>
                absurd (show IsDetached toolPre by simpa [IsDetached, toolPost] using h_det)
                  h_live_kind)
              (fun h_bg h_fg => ?_)
      simp [toolPost, h_bg] at h_fg
    · have h_lt : idx < pre.tools.length := (List.getElem?_eq_some_iff.mp h_idx).1
      simpa [post, toolPost] using List.mem_set pre.tools idx h_lt toolPost
  | inr h_rest =>
    cases h_rest with
    | inl h_held =>
      have h_t_step : ToolExecution.ToolCallContext.Transition toolPre
                        { toolPre with state := .cancelled } :=
        ToolExecution.ToolCallContext.Transition.cancelWhileHeld .interrupted h_held rfl
      let toolPost : ToolExecution.ToolCallContext := { toolPre with state := .cancelled }
      let post : ComposedState := { pre with tools := pre.tools.set idx toolPost }
      refine ⟨post, toolPost, ?_, rfl, h_interrupted, ?_, rfl, h_linked⟩
      · refine Trace.step ?_ Trace.refl
        refine Transition.tool_step h_idx h_t_step rfl rfl rfl rfl rfl
                ⟨h_linked, h_deadline_eq, h_time_eq⟩
                ⟨h_linked, h_deadline_eq, h_time_eq⟩
                (fun h_det =>
                  absurd (show IsDetached toolPre by simpa [IsDetached, toolPost] using h_det)
                    h_live_kind)
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
                ⟨h_linked, h_deadline_eq, h_time_eq⟩
                (fun h_det =>
                  absurd (show IsDetached toolPre by simpa [IsDetached, toolPost] using h_det)
                    h_live_kind)
                (fun h_bg h_fg => ?_)
        simp [toolPost, h_bg] at h_fg
      · have h_lt : idx < pre.tools.length := (List.getElem?_eq_some_iff.mp h_idx).1
        simpa [post, toolPost] using List.mem_set pre.tools idx h_lt toolPost

theorem interrupted_request_cancels_live_linked_tools_from_initial
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_reach        : Trace initial pre)
    (h_in           : toolPre ∈ pre.tools)
    (h_live_kind    : ¬ IsDetached toolPre)
    (h_interrupted  : pre.request.state = .interrupted)
    (h_live         : toolPre.cancellable) :
    ∃ post toolPost,
      Trace pre post ∧
      post.request = pre.request ∧
      post.request.state = .interrupted ∧
      toolPost ∈ post.tools ∧
      toolPost.state = .cancelled ∧
      toolPost.requestId = pre.requestId :=
  interrupted_request_cancels_live_linked_tools
    h_in (wellFormed_from_initial h_reach) h_live_kind h_interrupted h_live

theorem deadline_exceeded_request_timesOut_running_tools
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_in        : toolPre ∈ pre.tools)
    (h_wf        : pre.WellFormed)
    (h_live_kind : ¬ IsDetached toolPre)
    (h_running   : toolPre.state = .running)
    (h_deadline  : pre.request.deadlineExceeded) :
    ∃ post toolPost,
      Trace pre post ∧
      toolPost ∈ post.tools ∧
      post.request = pre.request ∧
      toolPost.state = .timedOut ∧
      toolPost.requestId = pre.requestId := by
  have h_coherent : Coherent pre toolPre :=
    h_wf.allToolsCoherent toolPre h_in h_live_kind
  obtain ⟨h_linked, h_deadline_eq, h_time_eq⟩ := h_coherent
  obtain ⟨idx, h_idx⟩ := List.mem_iff_getElem?.mp h_in
  have h_t_step : ToolExecution.ToolCallContext.Transition toolPre
                    { toolPre with state := .timedOut } := by
    refine ToolExecution.ToolCallContext.Transition.timeout
      h_running ?_ rfl
    show toolPre.currentTime > toolPre.deadline
    rw [h_deadline_eq, h_time_eq]
    exact h_deadline
  let toolPost : ToolExecution.ToolCallContext := { toolPre with state := .timedOut }
  let post : ComposedState := { pre with tools := pre.tools.set idx toolPost }
  refine ⟨post, toolPost, ?_, ?_, rfl, rfl, h_linked⟩
  ·
    refine Trace.step ?_ Trace.refl
    refine Transition.tool_step h_idx h_t_step rfl rfl rfl rfl rfl
      ⟨h_linked, h_deadline_eq, h_time_eq⟩
      ⟨h_linked, h_deadline_eq, h_time_eq⟩
      (fun h_det =>
        absurd (show IsDetached toolPre by simpa [IsDetached, toolPost] using h_det) h_live_kind)
      (fun h_bg h_fg => ?_)
    simp [toolPost, h_bg] at h_fg
  ·
    show toolPost ∈ pre.tools.set idx toolPost
    have h_lt : idx < pre.tools.length :=
      (List.getElem?_eq_some_iff.mp h_idx).1
    exact List.mem_set pre.tools idx h_lt toolPost

theorem deadline_exceeded_request_timesOut_running_tools_from_initial
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_reach     : Trace initial pre)
    (h_in        : toolPre ∈ pre.tools)
    (h_live_kind : ¬ IsDetached toolPre)
    (h_running   : toolPre.state = .running)
    (h_deadline  : pre.request.deadlineExceeded) :
    ∃ post toolPost,
      Trace pre post ∧
      toolPost ∈ post.tools ∧
      post.request = pre.request ∧
      toolPost.state = .timedOut ∧
      toolPost.requestId = pre.requestId :=
  deadline_exceeded_request_timesOut_running_tools
    h_in (wellFormed_from_initial h_reach) h_live_kind h_running h_deadline

theorem deadline_exceeded_request_cancels_pending_tools
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_in        : toolPre ∈ pre.tools)
    (h_wf        : pre.WellFormed)
    (h_live_kind : ¬ IsDetached toolPre)
    (h_pending   : toolPre.state = .pending)
    (h_deadline  : pre.request.deadlineExceeded) :
    ∃ post toolPost,
      Trace pre post ∧
      post.request = pre.request ∧
      post.request.deadlineExceeded ∧
      toolPost ∈ post.tools ∧
      toolPost.state = .cancelled ∧
      toolPost.requestId = pre.requestId := by
  have h_coherent : Coherent pre toolPre :=
    h_wf.allToolsCoherent toolPre h_in h_live_kind
  obtain ⟨h_linked, h_deadline_eq, h_time_eq⟩ := h_coherent
  obtain ⟨idx, h_idx⟩ := List.mem_iff_getElem?.mp h_in
  have h_t_step : ToolExecution.ToolCallContext.Transition toolPre
                    { toolPre with state := .cancelled } :=
    ToolExecution.ToolCallContext.Transition.cancelBeforeDispatch .deadline h_pending rfl
  let toolPost : ToolExecution.ToolCallContext := { toolPre with state := .cancelled }
  let post : ComposedState := { pre with tools := pre.tools.set idx toolPost }
  refine ⟨post, toolPost, ?_, rfl, h_deadline, ?_, rfl, h_linked⟩
  · refine Trace.step ?_ Trace.refl
    refine Transition.tool_step h_idx h_t_step rfl rfl rfl rfl rfl
            ⟨h_linked, h_deadline_eq, h_time_eq⟩
            ⟨h_linked, h_deadline_eq, h_time_eq⟩
            (fun h_det =>
              absurd (show IsDetached toolPre by simpa [IsDetached, toolPost] using h_det)
                h_live_kind)
            (fun h_bg h_fg => ?_)
    simp [toolPost, h_bg] at h_fg
  ·
    have h_lt : idx < pre.tools.length := (List.getElem?_eq_some_iff.mp h_idx).1
    simpa [post, toolPost] using List.mem_set pre.tools idx h_lt toolPost

theorem deadline_exceeded_request_cancels_pending_tools_from_initial
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_reach     : Trace initial pre)
    (h_in        : toolPre ∈ pre.tools)
    (h_live_kind : ¬ IsDetached toolPre)
    (h_pending   : toolPre.state = .pending)
    (h_deadline  : pre.request.deadlineExceeded) :
    ∃ post toolPost,
      Trace pre post ∧
      post.request = pre.request ∧
      post.request.deadlineExceeded ∧
      toolPost ∈ post.tools ∧
      toolPost.state = .cancelled ∧
      toolPost.requestId = pre.requestId :=
  deadline_exceeded_request_cancels_pending_tools
    h_in (wellFormed_from_initial h_reach) h_live_kind h_pending h_deadline

theorem all_tools_terminal_unblocks_request_progress
    {pre : ComposedState}
    (h_all_terminal : ∀ t ∈ pre.tools, isTerminal t.state)
    (h_proc         : pre.request.state = .processing)
    (h_admission    : pre.request.admission = .executing) :
    ∃ post,
      Transition pre post ∧
      RequestContext.Transition pre.request post.request := by
  let postReq : RequestContext :=
    { pre.request with progressSeq := pre.request.progressSeq + 1 }
  let post : ComposedState := { pre with request := postReq }
  have h_inner : RequestContext.Transition pre.request postReq :=
    RequestContext.Transition.advance h_proc h_admission rfl
  refine ⟨post, ?_, h_inner⟩
  refine Transition.request_step h_inner rfl rfl rfl rfl ?_ ?_
  ·
    intro h_pending
    rw [h_proc] at h_pending
    cases h_pending
  ·
    intro _h_advance
    intro ⟨t, h_in, _h_fg, h_nt⟩
    exact h_nt (h_all_terminal t h_in)

end ComposedState
