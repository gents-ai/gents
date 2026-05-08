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
      -- INV-FG (scoped): foreground-blocking guard. If the inner request
      -- transition is `advance` (progressSeq strictly increases) or
      -- `begin_inference` (claimed → processing), no foreground tool may be
      -- non-terminal. Other transitions (interrupt_*, fail, expire) are
      -- unaffected — the antecedent is false and the implication is
      -- vacuously discharged.
      (post.request.progressSeq > pre.request.progressSeq ∨
        (pre.request.state = .claimed ∧ post.request.state = .processing) →
        ¬ ∃ t ∈ pre.tools, t.awaitMode = .foreground ∧
                            ¬ isTerminal t.state) →
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


/-- C2: An interrupted request cancels every live linked tool call. -/
theorem interrupted_request_cancels_live_linked_tools
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_in           : toolPre ∈ pre.tools)
    (h_interrupted  : pre.request.state = .interrupted)  -- documentary, tracked by #153
    (h_live         : toolPre.cancellable)
    (h_coherent     : Coherent pre toolPre) :
    ∃ post toolPost,
      Trace pre post ∧
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
      ToolExecution.ToolCallContext.Transition.cancelBeforeDispatch h_pending rfl
    let toolPost : ToolExecution.ToolCallContext := { toolPre with state := .cancelled }
    let post : ComposedState := { pre with tools := pre.tools.set idx toolPost }
    refine ⟨post, toolPost, ?_, ?_, rfl, h_linked⟩
    · refine Trace.step ?_ Trace.refl
      exact Transition.tool_step h_idx h_t_step rfl rfl rfl rfl rfl
              ⟨h_linked, h_deadline_eq, h_time_eq⟩
    · have h_lt : idx < pre.tools.length := (List.getElem?_eq_some_iff.mp h_idx).1
      simpa [post, toolPost] using List.mem_set pre.tools idx h_lt toolPost
  | inr h_running =>
    have h_t_step : ToolExecution.ToolCallContext.Transition toolPre
                      { toolPre with state := .cancelled } :=
      ToolExecution.ToolCallContext.Transition.cancelDuringRun h_running rfl
    let toolPost : ToolExecution.ToolCallContext := { toolPre with state := .cancelled }
    let post : ComposedState := { pre with tools := pre.tools.set idx toolPost }
    refine ⟨post, toolPost, ?_, ?_, rfl, h_linked⟩
    · refine Trace.step ?_ Trace.refl
      exact Transition.tool_step h_idx h_t_step rfl rfl rfl rfl rfl
              ⟨h_linked, h_deadline_eq, h_time_eq⟩
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
    -- Pass h_coherent directly as the Coherent witness
    exact Transition.tool_step h_idx h_t_step rfl rfl rfl rfl rfl
      ⟨h_linked, h_deadline_eq, h_time_eq⟩
  · -- toolPost ∈ post.tools — follows from `pre.tools.set idx toolPost`
    show toolPost ∈ pre.tools.set idx toolPost
    have h_lt : idx < pre.tools.length :=
      (List.getElem?_eq_some_iff.mp h_idx).1
    exact List.mem_set pre.tools idx h_lt toolPost


/-- C1': A request whose deadline is exceeded cancels every Pending linked
    tool. Companion to C1 — a Pending tool never ran, so it reaches
    .cancelled rather than .timedOut.

    Note: `h_deadline` is documentary — `cancelBeforeDispatch` has no deadline
    guard. The hypothesis captures the operational context (deadline-driven
    cancellation path) rather than a proof-relevant constraint. Tracked under
    issue #153 alongside C2/C3's documentary hypotheses for a future
    CancelCause tightening pass. -/
theorem deadline_exceeded_request_cancels_pending_tools
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_in        : toolPre ∈ pre.tools)
    (h_pending   : toolPre.state = .pending)
    (h_deadline  : pre.request.deadlineExceeded)
    (h_coherent  : Coherent pre toolPre) :
    ∃ post toolPost,
      Trace pre post ∧
      toolPost ∈ post.tools ∧
      toolPost.state = .cancelled ∧
      toolPost.requestId = pre.requestId := by
  obtain ⟨h_linked, h_deadline_eq, h_time_eq⟩ := h_coherent
  obtain ⟨idx, h_idx⟩ := List.mem_iff_getElem?.mp h_in
  -- Inner cancelBeforeDispatch transition
  have h_t_step : ToolExecution.ToolCallContext.Transition toolPre
                    { toolPre with state := .cancelled } :=
    ToolExecution.ToolCallContext.Transition.cancelBeforeDispatch h_pending rfl
  let toolPost : ToolExecution.ToolCallContext := { toolPre with state := .cancelled }
  let post : ComposedState := { pre with tools := pre.tools.set idx toolPost }
  refine ⟨post, toolPost, ?_, ?_, rfl, h_linked⟩
  · refine Trace.step ?_ Trace.refl
    exact Transition.tool_step h_idx h_t_step rfl rfl rfl rfl rfl
            ⟨h_linked, h_deadline_eq, h_time_eq⟩
  · -- toolPost ∈ post.tools via List.mem_set
    have h_lt : idx < pre.tools.length := (List.getElem?_eq_some_iff.mp h_idx).1
    simpa [post, toolPost] using List.mem_set pre.tools idx h_lt toolPost


/-- C3: A request whose linked tools are all terminal can resume making progress.
    Multi-flight form: ∀-quantified over `pre.tools`. Semantic complement of
    issue #149: when every linked tool is terminal, the foreground-blocking
    guard on `request_step` is satisfied and the request can `advance`. -/
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

/-!
## INV-FG: foreground-blocking structural invariant

At most one foreground non-terminal tool may be live at any time per
`ComposedState`. Combined with the scoped `h_no_block` guard on
`request_step`, this gives the parent narrative the foreground-blocking
property: while a foreground tool is in flight, `advance` /
`begin_inference` cannot fire.

INV-FG is a structural witness; no C-theorem currently consumes it. It is
preserved across every composed transition. The four non-tool arms are
trivial (they don't touch `tools`); the `tool_step` arm requires
case-analysis on the inner `ToolCallContext.Transition`.
-/

/-- INV-FG: at most one foreground non-terminal tool per composed state. -/
def invFG (s : ComposedState) : Prop :=
  (s.tools.filter (fun t => decide (t.awaitMode = .foreground) ∧
                              ¬ isTerminal t.state)).length ≤ 1

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
  | tool_step _ _ _ _ _ _ _ _ =>
    -- A single tool transitions; the count of foreground non-terminal tools
    -- can only stay the same or decrease (state advancing toward terminal,
    -- background flipping awaitMode away from foreground), or — in the
    -- foreground-flip case — INV-FG in the pre-state forces the count to
    -- have been 0 (since pre.toolPre had awaitMode := .background), so the
    -- post count is ≤ 1. Closing this is intricate and not load-bearing
    -- for any current C-theorem. Tracked as future work.
    sorry

end ComposedState
