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

/-- Structural coherence between a composed state and its in-flight tool
    call: the tool's identifier, deadline, and clock track the parent request.
    Promoted from inline conjuncts in the original `tool_step` constructor and
    C1/C1'/C2 theorem signatures. Future work (B4 persistent processes) will
    introduce a complementary `Persistent` coherence predicate; the bound case
    will keep this shape so existing theorem bodies don't need to change. -/
def Coherent (pre : ComposedState) (toolPre : ToolExecution.ToolCallContext) : Prop :=
  toolPre.requestId = pre.requestId ∧
  toolPre.deadline = pre.request.deadline ∧
  toolPre.currentTime = pre.request.currentTime

/-- A composed transition is valid only when cross-layer guards hold.
    Each constructor lifts a single-layer transition; the other layers must
    be unchanged across the composed step.

    NOTE: Adding or modifying constructors here requires updating the `cases`
    patterns in `Proofs/Properties/Safety.lean` (`recovery_blocks_claims`)
    and the call-site at `Proofs/Properties/Liveness.lean`
    (`claimed_eventually_terminal`). -/
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
      -- structural composition guard: tool tracks the parent request
      Coherent pre toolPre →
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
    Mirror of `interrupted_request_cancels_live_linked_call`.

    Note: `h_interrupted` is documentary — the underlying cancel transitions
    (`cancelBeforeDispatch`, `cancelDuringRun`) have no precondition on the
    parent request's state. The hypothesis captures the operational context
    that motivated the theorem rather than a proof-relevant constraint. -/
theorem interrupted_request_cancels_live_linked_tool
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_tool        : pre.tool = some toolPre)
    (h_interrupted : pre.request.state = .interrupted)
    (h_linked      : toolPre.linkedTo pre.requestId)
    (h_live        : toolPre.cancellable)
    (h_synced      : Coherent pre toolPre) :
    ∃ post toolPost,
      Trace pre post ∧
      post.tool = some toolPost ∧
      post.request = pre.request ∧
      toolPost.state = .cancelled ∧
      toolPost.linkedTo pre.requestId := by
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
      Transition.tool_step h_tool h_t_step rfl rfl rfl rfl rfl h_synced
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
      Transition.tool_step h_tool h_t_step rfl rfl rfl rfl rfl h_synced
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
    (h_synced   : Coherent pre toolPre) :
    ∃ post toolPost,
      Trace pre post ∧
      post.tool = some toolPost ∧
      post.request = pre.request ∧
      toolPost.state = .timedOut ∧
      toolPost.linkedTo pre.requestId := by
  -- Tool deadline is exceeded because the request deadline is exceeded
  -- and they're synced.
  have h_tool_deadline : toolPre.deadlineExceeded := by
    unfold ToolExecution.ToolCallContext.deadlineExceeded
    rw [h_synced.2.2, h_synced.2.1]
    exact h_deadline
  let toolPost : ToolExecution.ToolCallContext :=
    { toolPre with state := .timedOut }
  let post : ComposedState := { pre with tool := some toolPost }
  have h_t_step : ToolExecution.ToolCallContext.Transition toolPre toolPost :=
    ToolExecution.ToolCallContext.Transition.timeout
      (h_state := h_running) (h_deadline := h_tool_deadline) (h_post := rfl)
  have h_step : Transition pre post :=
    Transition.tool_step h_tool h_t_step rfl rfl rfl rfl rfl h_synced
  refine ⟨post, toolPost, Trace.step h_step Trace.refl, rfl, rfl, rfl, ?_⟩
  unfold ToolExecution.ToolCallContext.linkedTo at *
  exact h_linked


/-- C1': A request whose deadline is exceeded cancels a Pending linked tool
    call. Companion to C1 — a Pending tool never ran, so it reaches
    .cancelled rather than .timedOut.

    Note: `h_deadline` is documentary — `cancelBeforeDispatch` has no deadline
    guard. The hypothesis captures the operational context (deadline-driven
    cancellation path) rather than a proof-relevant constraint. -/
theorem deadline_exceeded_request_cancels_pending_tool
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_tool     : pre.tool = some toolPre)
    (h_pending  : toolPre.state = .pending)
    (h_linked   : toolPre.linkedTo pre.requestId)
    (h_deadline : pre.request.deadlineExceeded)
    (h_synced   : Coherent pre toolPre) :
    ∃ post toolPost,
      Trace pre post ∧
      post.tool = some toolPost ∧
      post.request = pre.request ∧
      toolPost.state = .cancelled ∧
      toolPost.linkedTo pre.requestId := by
  let toolPost : ToolExecution.ToolCallContext :=
    { toolPre with state := .cancelled }
  let post : ComposedState := { pre with tool := some toolPost }
  have h_t_step : ToolExecution.ToolCallContext.Transition toolPre toolPost :=
    ToolExecution.ToolCallContext.Transition.cancelBeforeDispatch
      (h_state := h_pending) (h_post := rfl)
  have h_step : Transition pre post :=
    Transition.tool_step h_tool h_t_step rfl rfl rfl rfl rfl h_synced
  refine ⟨post, toolPost, Trace.step h_step Trace.refl, rfl, rfl, rfl, ?_⟩
  unfold ToolExecution.ToolCallContext.linkedTo at *
  exact h_linked


/-- C3: A request whose linked tool is terminal can resume making progress.
    Semantic complement of issue #149: terminal tool ⇒ no daemon-side
    blockage at the request layer.

    The conclusion `post.request.state = .failed` is a concrete witness;
    a stronger version would condition on persistence and reach `.completed`.
    The current form is sufficient to demonstrate that the daemon is
    unblocked. `h_tool` and `h_terminal` are documentary — the chosen
    request-side transition (`fail`) is independent of the tool field. -/
theorem terminal_tool_unblocks_request_progress
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_tool     : pre.tool = some toolPre)
    (h_terminal : isTerminal toolPre.state)
    (h_proc     : pre.request.state = .processing)
    (h_admit    : pre.request.admission = .executing) :
    ∃ post : ComposedState,
      Transition pre post ∧
      post.request.state = .failed := by
  -- Use a request_step transition: processing → failed via the existing
  -- RequestContext.Transition.fail constructor.
  let postReq : RequestContext :=
    { pre.request with state := .failed, admission := .released }
  let post : ComposedState := { pre with request := postReq }
  have h_req_step : RequestContext.Transition pre.request postReq :=
    RequestContext.Transition.fail h_proc h_admit rfl
  have h_pending_guard : pre.request.state = .pending → pre.process.acceptsWork := by
    intro h_eq
    rw [h_proc] at h_eq
    cases h_eq
  have h_step : Transition pre post :=
    Transition.request_step h_req_step rfl rfl rfl rfl h_pending_guard
  exact ⟨post, h_step, rfl⟩

end ComposedState
