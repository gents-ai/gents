import Proofs.Process
import Proofs.Request
import Proofs.InferenceCall
import Proofs.Persistence
import Proofs.ToolExecution

/-!
# Cross-Layer Composition

Combines the process, request, and persistence layers into a single
single-execution composed state.
-/

/-- The composed state of all single-execution layers, including the
    optional in-flight tool call. -/
structure ComposedState where
  requestId : RequestId
  process : ProcessState
  request : RequestContext
  call : InferenceCall
  tool : Option ToolExecution.ToolCallContext := none
  deriving Repr

namespace ComposedState

/-- A composed transition is valid only when cross-layer guards hold.
    Each constructor lifts a single-layer transition; the other layers must
    be unchanged across the composed step. -/
inductive Transition : ComposedState → ComposedState → Prop where
  | process_step {pre post : ComposedState} :
      ProcessState.Transition pre.process post.process →
      post.request = pre.request →
      post.call = pre.call →
      post.tool = pre.tool →
      post.requestId = pre.requestId →
      Transition pre post
  | request_step {pre post : ComposedState} :
      RequestContext.Transition pre.request post.request →
      post.process = pre.process →
      post.call = pre.call →
      post.tool = pre.tool →
      post.requestId = pre.requestId →
      (pre.request.state = .pending → pre.process.acceptsWork) →
      Transition pre post
  | persistence_step {pre post : ComposedState} (policy : PersistenceState.FailurePolicy)
      (nextPersistence : PersistenceState) :
      PersistenceState.Transition policy pre.request.persistence nextPersistence →
      post.request = { pre.request with persistence := nextPersistence } →
      post.process = pre.process →
      post.call = pre.call →
      post.tool = pre.tool →
      post.requestId = pre.requestId →
      Transition pre post
  | call_step {pre post : ComposedState} :
      InferenceCall.Transition pre.call post.call →
      post.request = pre.request →
      post.process = pre.process →
      post.tool = pre.tool →
      post.requestId = pre.requestId →
      Transition pre post
  | tool_step {pre post : ComposedState} {toolPre toolPost : ToolExecution.ToolCallContext} :
      pre.tool = some toolPre →
      ToolExecution.ToolCallContext.Transition toolPre toolPost →
      post.tool = some toolPost →
      post.request = pre.request →
      post.process = pre.process →
      post.call = pre.call →
      post.requestId = pre.requestId →
      -- structural composition guards: tool tracks the parent request
      toolPre.requestId = pre.requestId →
      toolPre.deadline = pre.request.deadline →
      toolPre.currentTime = pre.request.currentTime →
      Transition pre post

/-- A trace is a sequence of valid composed transitions. -/
inductive Trace : ComposedState → ComposedState → Prop where
  | refl {s : ComposedState} : Trace s s
  | step {s₁ s₂ s₃ : ComposedState} :
      Transition s₁ s₂ → Trace s₂ s₃ → Trace s₁ s₃

/-- The initial state of the system. -/
def initial : ComposedState :=
  { requestId := 0
  , process := .uninitialized
  , request :=
    { state := .pending
    , origin := .interactive
    , backend := { val := "initial-backend" }
    , admission := .released
    , deadline := 0
    , claimTime := 0
    , currentTime := 0
    , retryCount := 0
    , maxRetries := 3
    , progressSeq := 0
    , messageSeq := 0
    , isLatest := true
    , persistence := .uncommitted
    }
  , call :=
    { callId := 0
    , requestId := 0
    , backend := { val := "initial-backend" }
    , state := .queued
    }
  , tool := none
  }

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
    Mirror of `interrupted_request_cancels_live_linked_call`. -/
theorem interrupted_request_cancels_live_linked_tool
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_tool        : pre.tool = some toolPre)
    (h_interrupted : pre.request.state = .interrupted)
    (h_linked      : toolPre.linkedTo pre.requestId)
    (h_live        : toolPre.cancellable)
    (h_synced      : toolPre.requestId = pre.requestId ∧
                     toolPre.deadline = pre.request.deadline ∧
                     toolPre.currentTime = pre.request.currentTime) :
    ∃ post toolPost,
      Trace pre post ∧
      post.tool = some toolPost ∧
      post.request = pre.request ∧
      toolPost.state = .cancelled ∧
      toolPost.linkedTo pre.requestId := by
  obtain ⟨h_sync_id, h_sync_deadline, h_sync_time⟩ := h_synced
  -- Case-split on toolPre.state via h_live (which is .pending ∨ .running).
  rcases h_live with h_pending | h_running
  · -- Pending → cancelBeforeDispatch → Cancelled
    let toolPost : ToolExecution.ToolCallContext :=
      { toolPre with state := .cancelled }
    let post : ComposedState := { pre with tool := some toolPost }
    have h_t_step : ToolExecution.ToolCallContext.Transition toolPre toolPost :=
      ToolExecution.ToolCallContext.Transition.cancelBeforeDispatch
        (h_state := h_pending) (h_post := rfl)
    have h_step : Transition pre post :=
      Transition.tool_step h_tool h_t_step rfl rfl rfl rfl rfl
        h_sync_id h_sync_deadline h_sync_time
    refine ⟨post, toolPost, Trace.step h_step Trace.refl, rfl, rfl, rfl, ?_⟩
    unfold ToolExecution.ToolCallContext.linkedTo at *
    exact h_linked
  · -- Running → cancelDuringRun → Cancelled
    let toolPost : ToolExecution.ToolCallContext :=
      { toolPre with state := .cancelled }
    let post : ComposedState := { pre with tool := some toolPost }
    have h_t_step : ToolExecution.ToolCallContext.Transition toolPre toolPost :=
      ToolExecution.ToolCallContext.Transition.cancelDuringRun
        (h_state := h_running) (h_post := rfl)
    have h_step : Transition pre post :=
      Transition.tool_step h_tool h_t_step rfl rfl rfl rfl rfl
        h_sync_id h_sync_deadline h_sync_time
    refine ⟨post, toolPost, Trace.step h_step Trace.refl, rfl, rfl, rfl, ?_⟩
    unfold ToolExecution.ToolCallContext.linkedTo at *
    exact h_linked


/-- C1: A request whose deadline is exceeded times out a Running linked
    tool call via the timeout transition. The composition theorem whose
    absence in the runtime caused issue #149. -/
theorem deadline_exceeded_request_timesOut_running_tool
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_tool     : pre.tool = some toolPre)
    (h_running  : toolPre.state = .running)
    (h_linked   : toolPre.linkedTo pre.requestId)
    (h_deadline : pre.request.deadlineExceeded)
    (h_synced   : toolPre.requestId = pre.requestId ∧
                  toolPre.deadline = pre.request.deadline ∧
                  toolPre.currentTime = pre.request.currentTime) :
    ∃ post toolPost,
      Trace pre post ∧
      post.tool = some toolPost ∧
      post.request = pre.request ∧
      toolPost.state = .timedOut ∧
      toolPost.linkedTo pre.requestId := by
  obtain ⟨h_sync_id, h_sync_deadline, h_sync_time⟩ := h_synced
  -- Tool deadline is exceeded because the request deadline is exceeded
  -- and they're synced.
  have h_tool_deadline : toolPre.deadlineExceeded := by
    unfold ToolExecution.ToolCallContext.deadlineExceeded
    rw [h_sync_time, h_sync_deadline]
    exact h_deadline
  let toolPost : ToolExecution.ToolCallContext :=
    { toolPre with state := .timedOut }
  let post : ComposedState := { pre with tool := some toolPost }
  have h_t_step : ToolExecution.ToolCallContext.Transition toolPre toolPost :=
    ToolExecution.ToolCallContext.Transition.timeout
      (h_state := h_running) (h_deadline := h_tool_deadline) (h_post := rfl)
  have h_step : Transition pre post :=
    Transition.tool_step h_tool h_t_step rfl rfl rfl rfl rfl
      h_sync_id h_sync_deadline h_sync_time
  refine ⟨post, toolPost, Trace.step h_step Trace.refl, rfl, rfl, rfl, ?_⟩
  unfold ToolExecution.ToolCallContext.linkedTo at *
  exact h_linked

end ComposedState
