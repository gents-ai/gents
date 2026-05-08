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
    list of concurrently-live in-flight tool calls.

    Multi-flight: a single composed state may carry multiple
    `ToolCallContext`s simultaneously (e.g., a foreground tool waiting on a
    background subagent in a sibling slot). Single-flight is the special
    case `tools = [t]`. -/
structure ComposedState where
  requestId : RequestId
  process : ProcessState
  request : RequestContext
  call : InferenceCall
  tools : List ToolExecution.ToolCallContext := []
  deriving Repr

namespace ComposedState

/-- A tool is linked to this composed state if it is in the tools list. -/
def hasToolByCallId (s : ComposedState) (callId : ToolExecution.ToolCallId) : Prop :=
  ∃ t ∈ s.tools, t.callId = callId

instance (s : ComposedState) (callId : ToolExecution.ToolCallId) :
    Decidable (s.hasToolByCallId callId) := by
  unfold hasToolByCallId; infer_instance

/-- The first tool with a given callId, if any. CallIds are intended to be
    unique within a composed state, but we don't enforce that as a Prop here
    — Tasks 7+ will introduce a `UniqueCallIds` invariant alongside the
    multi-flight C-theorems if needed. -/
def findToolByCallId (s : ComposedState) (callId : ToolExecution.ToolCallId) :
    Option ToolExecution.ToolCallContext :=
  s.tools.find? (fun t => t.callId = callId)

/-- Structural coherence between a composed state and one of its in-flight
    tool calls: the tool's identifier, deadline, and clock track the parent
    request. Promoted from inline conjuncts in the original `tool_step`
    constructor and C1/C1'/C2 theorem signatures.

    The predicate is per-tool: `Coherent pre toolPre` says `toolPre` (one
    element of `pre.tools`) is structurally synced with `pre.request`. The
    multi-flight `tool_step` constructor applies it to the single tool being
    stepped. A list-level "all tools coherent" form, if needed by Tasks 7+,
    can be expressed as `∀ t ∈ pre.tools, Coherent pre t`.

    Future work (B4 persistent processes) will introduce a complementary
    `Persistent` coherence predicate; the bound case will keep this shape so
    existing theorem bodies don't need to change. -/
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
      post.tools = pre.tools →
      post.requestId = pre.requestId →
      Transition pre post
  | request_step {pre post : ComposedState} :
      RequestContext.Transition pre.request post.request →
      post.process = pre.process →
      post.call = pre.call →
      post.tools = pre.tools →
      post.requestId = pre.requestId →
      (pre.request.state = .pending → pre.process.acceptsWork) →
      Transition pre post
  | persistence_step {pre post : ComposedState} (policy : PersistenceState.FailurePolicy)
      (nextPersistence : PersistenceState) :
      PersistenceState.Transition policy pre.request.persistence nextPersistence →
      post.request = { pre.request with persistence := nextPersistence } →
      post.process = pre.process →
      post.call = pre.call →
      post.tools = pre.tools →
      post.requestId = pre.requestId →
      Transition pre post
  | call_step {pre post : ComposedState} :
      InferenceCall.Transition pre.call post.call →
      post.request = pre.request →
      post.process = pre.process →
      post.tools = pre.tools →
      post.requestId = pre.requestId →
      Transition pre post
  | tool_step {pre post : ComposedState} {idx : Nat}
              {toolPre toolPost : ToolExecution.ToolCallContext} :
      pre.tools[idx]? = some toolPre →
      ToolExecution.ToolCallContext.Transition toolPre toolPost →
      post.tools = pre.tools.set idx toolPost →
      post.request = pre.request →
      post.process = pre.process →
      post.call = pre.call →
      post.requestId = pre.requestId →
      -- structural composition guard: the stepping tool tracks the parent
      -- request. Other tools in `pre.tools` are unconstrained at this
      -- layer; Tasks 7+ may add list-level invariants if needed.
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
  , tools := []
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
    that motivated the theorem rather than a proof-relevant constraint.

    STUBBED for Task 6 (multi-flight refactor). Task 9 restates this with
    multi-flight quantification (`_tools` plural) and a fresh proof. -/
theorem interrupted_request_cancels_live_linked_tool
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_tool        : pre.tools = [toolPre])
    (h_interrupted : pre.request.state = .interrupted)
    (h_linked      : toolPre.linkedTo pre.requestId)
    (h_live        : toolPre.cancellable)
    (h_synced      : Coherent pre toolPre) :
    ∃ post toolPost,
      Trace pre post ∧
      post.tools = [toolPost] ∧
      post.request = pre.request ∧
      toolPost.state = .cancelled ∧
      toolPost.linkedTo pre.requestId := by
  sorry


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
    -- Pass h_coherent directly as the Coherent witness
    exact Transition.tool_step h_idx h_t_step rfl rfl rfl rfl rfl
      ⟨h_linked, h_deadline_eq, h_time_eq⟩
  · -- toolPost ∈ post.tools — follows from `pre.tools.set idx toolPost`
    show toolPost ∈ pre.tools.set idx toolPost
    have h_lt : idx < pre.tools.length :=
      (List.getElem?_eq_some_iff.mp h_idx).1
    exact List.mem_set pre.tools idx h_lt toolPost


/-- C1': A request whose deadline is exceeded cancels a Pending linked tool
    call. Companion to C1 — a Pending tool never ran, so it reaches
    .cancelled rather than .timedOut.

    Note: `h_deadline` is documentary — `cancelBeforeDispatch` has no deadline
    guard. The hypothesis captures the operational context (deadline-driven
    cancellation path) rather than a proof-relevant constraint.

    STUBBED for Task 6 (multi-flight refactor). Task 8 restates this with
    multi-flight quantification (`_tools` plural) and a fresh proof. -/
theorem deadline_exceeded_request_cancels_pending_tool
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_tool     : pre.tools = [toolPre])
    (h_pending  : toolPre.state = .pending)
    (h_linked   : toolPre.linkedTo pre.requestId)
    (h_deadline : pre.request.deadlineExceeded)
    (h_synced   : Coherent pre toolPre) :
    ∃ post toolPost,
      Trace pre post ∧
      post.tools = [toolPost] ∧
      post.request = pre.request ∧
      toolPost.state = .cancelled ∧
      toolPost.linkedTo pre.requestId := by
  sorry


/-- C3: A request whose linked tool is terminal can resume making progress.
    Semantic complement of issue #149: terminal tool ⇒ no daemon-side
    blockage at the request layer.

    The conclusion `post.request.state = .failed` is a concrete witness;
    a stronger version would condition on persistence and reach `.completed`.
    The current form is sufficient to demonstrate that the daemon is
    unblocked. `h_tool` and `h_terminal` are documentary — the chosen
    request-side transition (`fail`) is independent of the tool field.

    STUBBED for Task 6 (multi-flight refactor). Task 10 restates this as
    `all_tools_terminal_unblocks_request_progress`, quantifying over
    every element of `pre.tools`, with a fresh proof. -/
theorem terminal_tool_unblocks_request_progress
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_tool     : pre.tools = [toolPre])
    (h_terminal : isTerminal toolPre.state)
    (h_proc     : pre.request.state = .processing)
    (h_admit    : pre.request.admission = .executing) :
    ∃ post : ComposedState,
      Transition pre post ∧
      post.request.state = .failed := by
  sorry

end ComposedState
