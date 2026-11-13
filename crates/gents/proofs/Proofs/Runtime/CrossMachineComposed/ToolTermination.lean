import Proofs.Runtime.CrossMachineComposed.WellFormed
import Proofs.Session.Request.Executable

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
  have h_t_step : ToolExecution.ToolCallContext.Transition toolPre
      { toolPre with state := .cancelled } := by
    rcases h_live with h_pending | h_running
    · exact ToolExecution.ToolCallContext.Transition.cancelBeforeDispatch .interrupted h_pending rfl
    · exact ToolExecution.ToolCallContext.Transition.cancelDuringRun .interrupted h_running rfl
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

theorem all_tools_terminal_admits_processing_continuation
    {pre : ComposedState}
    (h_all_terminal : ∀ t ∈ pre.tools, isTerminal t.state)
    (h_proc         : pre.request.state = .processing)
    (h_admission    : pre.request.admission = .executing) :
    ∃ post,
      Transition pre post ∧
      RequestContext.step? pre.request .continueProcessing = some post.request := by
  let postReq : RequestContext := pre.request
  let post : ComposedState := { pre with request := postReq }
  have h_inner : RequestContext.Transition pre.request postReq :=
    RequestContext.Transition.continue_processing h_proc h_admission rfl
  have h_action : RequestContext.step? pre.request .continueProcessing = some postReq := by
    simp [RequestContext.step?, h_proc, h_admission, postReq]
  refine ⟨post, ?_, h_action⟩
  refine Transition.request_step h_inner rfl rfl rfl rfl ?_ ?_
  ·
    intro h_pending
    rw [h_proc] at h_pending
    cases h_pending
  ·
    intro _h_continuation
    intro ⟨t, h_in, _h_fg, h_nt⟩
    exact h_nt (h_all_terminal t h_in)

/-- What a request stop does to one linked tool. A request past its deadline
cancels a pending tool and times out a running one; both record the deadline
cause, and the timeout records an external failure because the tool did not
finish on its own. An interrupted request cancels a live tool with the
interrupted cause. A terminal tool, or a request neither late nor interrupted,
is left alone. -/
structure Termination where
  state : ToolExecution.ToolCallState
  cancelCause : Option ToolExecution.CancelCause
  failureClass : Option ToolExecution.FailureClass
  deriving DecidableEq, Repr

def linkedTermination (request : RequestContext) (tool : ToolExecution.ToolCallContext) :
    Option Termination :=
  if request.deadlineExceeded then
    match tool.state with
    | .pending => some ⟨.cancelled, some .deadline, none⟩
    | .running => some ⟨.timedOut, some .deadline, some .external⟩
    | _ => none
  else if request.state = .interrupted then
    match tool.state with
    | .pending | .running => some ⟨.cancelled, some .interrupted, none⟩
    | _ => none
  else none

theorem linkedTermination_reachable_from_initial
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext} {termination : Termination}
    (h_reach     : Trace initial pre)
    (h_in        : toolPre ∈ pre.tools)
    (h_live_kind : ¬ IsDetached toolPre)
    (h_step      : linkedTermination pre.request toolPre = some termination) :
    ∃ post toolPost,
      Trace pre post ∧
      post.request = pre.request ∧
      toolPost ∈ post.tools ∧
      toolPost.state = termination.state ∧
      toolPost.requestId = pre.requestId := by
  unfold linkedTermination at h_step
  by_cases h_deadline : pre.request.deadlineExceeded
  · rw [if_pos h_deadline] at h_step
    cases h_state : toolPre.state <;> rw [h_state] at h_step <;> simp at h_step
    · subst h_step
      obtain ⟨post, toolPost, h_trace, h_req, _, h_mem, h_post, h_id⟩ :=
        deadline_exceeded_request_cancels_pending_tools_from_initial
          h_reach h_in h_live_kind h_state h_deadline
      exact ⟨post, toolPost, h_trace, h_req, h_mem, h_post, h_id⟩
    · subst h_step
      obtain ⟨post, toolPost, h_trace, h_mem, h_req, h_post, h_id⟩ :=
        deadline_exceeded_request_timesOut_running_tools_from_initial
          h_reach h_in h_live_kind h_state h_deadline
      exact ⟨post, toolPost, h_trace, h_req, h_mem, h_post, h_id⟩
  · rw [if_neg h_deadline] at h_step
    by_cases h_interrupted : pre.request.state = .interrupted
    · rw [if_pos h_interrupted] at h_step
      have h_live : toolPre.cancellable ∧ termination.state = .cancelled := by
        cases h_state : toolPre.state <;> rw [h_state] at h_step <;> simp at h_step
        · exact ⟨Or.inl h_state, by rw [← h_step]⟩
        · exact ⟨Or.inr h_state, by rw [← h_step]⟩
      obtain ⟨post, toolPost, h_trace, h_req, _, h_mem, h_post, h_id⟩ :=
        interrupted_request_cancels_live_linked_tools_from_initial
          h_reach h_in h_live_kind h_interrupted h_live.1
      exact ⟨post, toolPost, h_trace, h_req, h_mem, h_post.trans h_live.2.symm, h_id⟩
    · rw [if_neg h_interrupted] at h_step
      cases h_step

end ComposedState
