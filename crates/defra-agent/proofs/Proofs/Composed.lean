import Proofs.Process
import Proofs.Request
import Proofs.InferenceCall
import Proofs.Persistence
import Proofs.ToolExecution
import Proofs.ManagedExec.Composed

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

/-- Coherence exposes the exact effective deadline shared by a tool and its
    parent request. -/
theorem coherent_tool_deadline_eq_request_deadline
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_coherent : Coherent pre toolPre) :
    toolPre.deadline = pre.request.deadline :=
  h_coherent.2.1

/-- Deadline-exceeded checks are synchronized for coherent linked tools. -/
theorem coherent_tool_deadlineExceeded_iff_request_deadlineExceeded
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_coherent : Coherent pre toolPre) :
    toolPre.deadlineExceeded ↔ pre.request.deadlineExceeded := by
  obtain ⟨_, h_deadline_eq, h_time_eq⟩ := h_coherent
  simp [ToolExecution.ToolCallContext.deadlineExceeded,
        RequestContext.deadlineExceeded, h_deadline_eq, h_time_eq]

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
      -- INV-FG composition guard: a background → foreground flip is only
      -- legal when the pre-state has no other foreground non-terminal tool.
      -- The antecedent fires only for the inner `foreground` constructor
      -- (the lone constructor that flips awaitMode background → foreground);
      -- every other inner transition either preserves awaitMode or already
      -- requires foreground in the pre-state, so the antecedent is false
      -- and the implication is vacuously discharged. Together with INV-FG
      -- itself (count ≤ 1), this guard makes `invFG_preserved` provable.
      (toolPre.awaitMode = .background → toolPost.awaitMode = .foreground →
        ¬ ∃ t ∈ pre.tools, t.awaitMode = .foreground ∧
                            ¬ isTerminal t.state) →
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
      ToolExecution.ToolCallContext.Transition.cancelDuringRun h_running rfl
    let toolPost : ToolExecution.ToolCallContext := { toolPre with state := .cancelled }
    let post : ComposedState := { pre with tools := pre.tools.set idx toolPost }
    refine ⟨post, toolPost, ?_, ?_, rfl, h_linked⟩
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

/-- Helper: setting at index `i` to a value `b` whose filter classification is
    implied by `a`'s never grows the filtered length. Concretely, if the new
    element passes the predicate, the old one did too — so the filter can only
    keep the same or fewer elements after `set`. -/
private lemma length_filter_set_le {α : Type _} (p : α → Bool) :
    ∀ (l : List α) (i : Nat) (a b : α),
      l[i]? = some a →
      (p b = true → p a = true) →
      ((l.set i b).filter p).length ≤ (l.filter p).length := by
  intro l
  induction l with
  | nil => intro i a b h _; simp at h
  | cons x xs ih =>
    intro i a b h_idx h_imp
    cases i with
    | zero =>
      -- l[0] = x, so a = x. set 0 = b :: xs.
      have hxa : x = a := by simpa using h_idx
      subst hxa
      simp only [List.set_cons_zero, List.filter_cons]
      by_cases hb : p b = true
      · -- p b true: both branches keep one element
        have ha : p x = true := h_imp hb
        simp [hb, ha]
      · -- p b false: post drops the element
        by_cases ha : p x = true
        · rw [if_neg hb, if_pos ha, List.length_cons]; omega
        · rw [if_neg hb, if_neg ha]
    | succ j =>
      -- Recurse on tail.
      simp only [List.set_cons_succ, List.filter_cons]
      have h_tail : xs[j]? = some a := by simpa using h_idx
      have ih' : ((xs.set j b).filter p).length ≤ (xs.filter p).length :=
        ih j a b h_tail h_imp
      by_cases hx : p x = true
      · rw [if_pos hx, if_pos hx, List.length_cons, List.length_cons]; omega
      · rw [if_neg hx, if_neg hx]; exact ih'

/-- Helper: setting at index `i` to a value `b` increases the filtered length
    by at most one — the old element either passed the filter (count
    unchanged or decreases) or didn't (count grows by ≤ 1). Together with
    the foreground-flip guard's "pre count = 0" precondition, this bounds
    `post.invFG` for the foreground constructor. -/
private lemma length_filter_set_le_succ {α : Type _} (p : α → Bool) :
    ∀ (l : List α) (i : Nat) (b : α),
      ((l.set i b).filter p).length ≤ (l.filter p).length + 1 := by
  intro l
  induction l with
  | nil => intro i b; simp
  | cons x xs ih =>
    intro i b
    cases i with
    | zero =>
      simp only [List.set_cons_zero, List.filter_cons]
      by_cases hb : p b = true
      · by_cases hx : p x = true
        · rw [if_pos hb, if_pos hx]; simp [List.length_cons]
        · rw [if_pos hb, if_neg hx, List.length_cons]
      · by_cases hx : p x = true
        · rw [if_neg hb, if_pos hx, List.length_cons]; omega
        · rw [if_neg hb, if_neg hx]; omega
    | succ j =>
      simp only [List.set_cons_succ, List.filter_cons]
      have ih' : ((xs.set j b).filter p).length ≤ (xs.filter p).length + 1 :=
        ih j b
      by_cases hx : p x = true
      · rw [if_pos hx, if_pos hx]; simp [List.length_cons]; omega
      · rw [if_neg hx, if_neg hx]; exact ih'

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
  | @tool_step idx toolPre toolPost h_idx h_t_step h_tools _ _ _ _ _ h_fg_guard =>
    -- A single tool transitions. Case-split on the inner ToolCallContext.Transition.
    -- For all 11 non-`foreground` constructors: `toolPost.awaitMode = toolPre.awaitMode`
    -- AND if toolPost passes the filter (foreground + non-terminal) then so does
    -- toolPre. Hence by `length_filter_set_le`, post count ≤ pre count ≤ 1.
    -- For `foreground`: the guard `h_fg_guard` fires, forcing the pre-state to
    -- have no foreground non-terminal tool, i.e. `pre.filter ... = []`. By
    -- `length_filter_set_le_succ`, post count ≤ 0 + 1 = 1.
    unfold invFG
    rw [h_tools]
    set p : ToolExecution.ToolCallContext → Bool :=
      fun t => decide (t.awaitMode = .foreground) ∧ ¬ isTerminal t.state with hp
    -- Helper: every non-foreground inner constructor has the property that
    -- p toolPost → p toolPre, so `length_filter_set_le` closes the case.
    -- The `foreground` constructor is the lone exception, handled by the guard.
    cases h_t_step with
    | dispatch h_state h_post =>
      -- toolPost = { toolPre with state := .running, ... }. awaitMode preserved.
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp [hp, h_post] at h_post_p ⊢
      refine ⟨h_post_p.1, ?_⟩
      intro h_term
      rw [h_state] at h_term
      rcases h_term with h' | h' | h' | h' <;> cases h'
    | spawnFailed failure h_state h_post =>
      -- toolPost.state = .failed (terminal); p toolPost = false.
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp [hp, h_post] at h_post_p
      exact absurd h_post_p.2 (fun h => h (Or.inr (Or.inl rfl)))
    | complete h_state _ _ h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp [hp, h_post] at h_post_p
      exact absurd h_post_p.2 (fun h => h (Or.inl rfl))
    | fail failure h_state h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp [hp, h_post] at h_post_p
      exact absurd h_post_p.2 (fun h => h (Or.inr (Or.inl rfl)))
    | timeout h_state _ h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp [hp, h_post] at h_post_p
      exact absurd h_post_p.2 (fun h => h (Or.inr (Or.inr (Or.inl rfl))))
    | cancelBeforeDispatch h_state h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp [hp, h_post] at h_post_p
      exact absurd h_post_p.2 (fun h => h (Or.inr (Or.inr (Or.inr rfl))))
    | cancelDuringRun h_state h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp [hp, h_post] at h_post_p
      exact absurd h_post_p.2 (fun h => h (Or.inr (Or.inr (Or.inr rfl))))
    | background h_state h_mode h_post =>
      -- toolPost.awaitMode = .background; p toolPost = false (foreground required).
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      -- After simp, h_post_p will simplify to False (awaitMode = .background ≠ .foreground)
      -- so this branch becomes unreachable; absurd via False.elim.
      exfalso
      simp [hp, h_post] at h_post_p
    | foreground h_state h_mode h_post =>
      -- The lone case where post passes the filter but pre doesn't. Use the
      -- foreground-flip guard `h_fg_guard` to conclude pre's filter is empty.
      have h_post_fg : toolPost.awaitMode = .foreground := by simp [h_post]
      have h_no_other : ¬ ∃ t ∈ pre.tools, t.awaitMode = .foreground ∧
                            ¬ isTerminal t.state :=
        h_fg_guard h_mode h_post_fg
      have h_filter_nil : pre.tools.filter p = [] := by
        rw [List.filter_eq_nil_iff]
        intro t h_in h_pt
        apply h_no_other
        refine ⟨t, h_in, ?_, ?_⟩
        · simp [hp] at h_pt; exact h_pt.1
        · simp [hp] at h_pt; exact h_pt.2
      have h_pre_zero : (pre.tools.filter p).length = 0 := by
        rw [h_filter_nil]; rfl
      have h_le := length_filter_set_le_succ p pre.tools idx toolPost
      omega
    | detach h_live h_pol h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      -- toolPost = { toolPre with cancelPolicy := .detach }; awaitMode and state preserved.
      simp only [hp, h_post] at h_post_p ⊢
      exact h_post_p
    | timeAdvance t h_le h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp only [hp, h_post] at h_post_p ⊢
      exact h_post_p
    | persistenceStep policy next h_p_step h_post =>
      refine le_trans (length_filter_set_le p pre.tools idx toolPre toolPost h_idx ?_) h_inv
      intro h_post_p
      simp only [hp, h_post] at h_post_p ⊢
      exact h_post_p

/-!
## UniqueCallIds: structural invariant on the tool list

Every tool in `pre.tools` has a distinct `callId`. The runtime mints fresh
callIds at spawn time and never reuses them, so this is a structural fact
about any reachable state, not a conditional assumption.

Used by `Subagent.Properties.detach_does_not_cancel_child` (B3') to discharge
the cascade-vs-detach contradiction without taking callId-uniqueness as a
hypothesis.
-/

/-- UniqueCallIds: every tool in the list has a distinct callId. -/
def UniqueCallIds (s : ComposedState) : Prop :=
  ∀ (i j : Nat) (h_i : i < s.tools.length) (h_j : j < s.tools.length),
    s.tools[i].callId = s.tools[j].callId → i = j

/-- Pairwise corollary: from UniqueCallIds, any two `∈ s.tools` tools with
    the same callId are equal. The form consumed by B3' (cascade-vs-detach
    contradiction) and similar pairwise-uniqueness arguments. -/
theorem UniqueCallIds.eq_of_callId_eq
    {s : ComposedState} (h_uniq : s.UniqueCallIds)
    {t₁ t₂ : ToolExecution.ToolCallContext}
    (h_in₁ : t₁ ∈ s.tools) (h_in₂ : t₂ ∈ s.tools)
    (h_eq  : t₁.callId = t₂.callId) :
    t₁ = t₂ := by
  obtain ⟨i, h_i⟩ := List.mem_iff_getElem?.mp h_in₁
  obtain ⟨j, h_j⟩ := List.mem_iff_getElem?.mp h_in₂
  have h_i_lt : i < s.tools.length := (List.getElem?_eq_some_iff.mp h_i).1
  have h_j_lt : j < s.tools.length := (List.getElem?_eq_some_iff.mp h_j).1
  have h_t1_eq : s.tools[i] = t₁ := by
    have := (List.getElem?_eq_some_iff.mp h_i).2
    simpa using this
  have h_t2_eq : s.tools[j] = t₂ := by
    have := (List.getElem?_eq_some_iff.mp h_j).2
    simpa using this
  have h_idx_eq : i = j := by
    apply h_uniq i j h_i_lt h_j_lt
    rw [h_t1_eq, h_t2_eq]; exact h_eq
  -- Substitute i := j to identify s.tools[i] with s.tools[j].
  subst h_idx_eq
  -- Now h_t1_eq, h_t2_eq both refer to s.tools[i], so t₁ = s.tools[i] = t₂.
  rw [← h_t1_eq, h_t2_eq]

/-- Helper: `set` preserves length. -/
private theorem length_set_eq {α : Type _} (l : List α) (i : Nat) (a : α) :
    (l.set i a).length = l.length := by
  exact List.length_set l i a

/-- UniqueCallIds is preserved by every composed transition. The four non-tool
    arms simply propagate `pre.tools = post.tools`. The `tool_step` arm uses
    the inner `transition_preserves_callId` to argue that the (single) tool
    swapped at `idx` keeps its original callId; uniqueness is therefore
    inherited from the pre-state. -/
theorem uniqueCallIds_preserved
    {pre post : ComposedState}
    (h_inv  : pre.UniqueCallIds)
    (h_step : Transition pre post) :
    post.UniqueCallIds := by
  cases h_step with
  | process_step _ _ _ h_tools _ =>
    intro i j h_i h_j h_eq
    have h_i' : i < pre.tools.length := by rw [h_tools] at h_i; exact h_i
    have h_j' : j < pre.tools.length := by rw [h_tools] at h_j; exact h_j
    apply h_inv i j h_i' h_j'
    have hi : pre.tools[i] = post.tools[i] := by congr 1 <;> rw [h_tools]
    have hj : pre.tools[j] = post.tools[j] := by congr 1 <;> rw [h_tools]
    rw [hi, hj]; exact h_eq
  | request_step _ _ _ h_tools _ _ _ =>
    intro i j h_i h_j h_eq
    have h_i' : i < pre.tools.length := by rw [h_tools] at h_i; exact h_i
    have h_j' : j < pre.tools.length := by rw [h_tools] at h_j; exact h_j
    apply h_inv i j h_i' h_j'
    have hi : pre.tools[i] = post.tools[i] := by congr 1 <;> rw [h_tools]
    have hj : pre.tools[j] = post.tools[j] := by congr 1 <;> rw [h_tools]
    rw [hi, hj]; exact h_eq
  | persistence_step _ _ _ _ _ _ h_tools _ =>
    intro i j h_i h_j h_eq
    have h_i' : i < pre.tools.length := by rw [h_tools] at h_i; exact h_i
    have h_j' : j < pre.tools.length := by rw [h_tools] at h_j; exact h_j
    apply h_inv i j h_i' h_j'
    have hi : pre.tools[i] = post.tools[i] := by congr 1 <;> rw [h_tools]
    have hj : pre.tools[j] = post.tools[j] := by congr 1 <;> rw [h_tools]
    rw [hi, hj]; exact h_eq
  | call_step _ _ _ h_tools _ =>
    intro i j h_i h_j h_eq
    have h_i' : i < pre.tools.length := by rw [h_tools] at h_i; exact h_i
    have h_j' : j < pre.tools.length := by rw [h_tools] at h_j; exact h_j
    apply h_inv i j h_i' h_j'
    have hi : pre.tools[i] = post.tools[i] := by congr 1 <;> rw [h_tools]
    have hj : pre.tools[j] = post.tools[j] := by congr 1 <;> rw [h_tools]
    rw [hi, hj]; exact h_eq
  | @tool_step idx toolPre toolPost h_idx h_t_step h_tools _ _ _ _ _ _ =>
    -- post.tools = pre.tools.set idx toolPost. Since
    -- transition_preserves_callId says toolPost.callId = toolPre.callId, the
    -- callId at every index is the same in pre and post. UniqueCallIds carries
    -- straight through.
    have h_callId_eq : toolPost.callId = toolPre.callId :=
      ToolExecution.ToolCallContext.transition_preserves_callId h_t_step
    have h_len : post.tools.length = pre.tools.length := by
      rw [h_tools]; exact List.length_set _ _ _
    have h_idx_lt : idx < pre.tools.length :=
      (List.getElem?_eq_some_iff.mp h_idx).1
    have h_pre_idx_eq : pre.tools[idx] = toolPre := by
      have := (List.getElem?_eq_some_iff.mp h_idx).2
      simpa using this
    intro i j h_i h_j h_eq
    have h_i' : i < pre.tools.length := by rw [h_len] at h_i; exact h_i
    have h_j' : j < pre.tools.length := by rw [h_len] at h_j; exact h_j
    -- For each index k, post.tools[k].callId = pre.tools[k].callId.
    have h_callId_at : ∀ (k : Nat) (h_k : k < pre.tools.length),
        (post.tools[k]'(by rw [h_len]; exact h_k)).callId = pre.tools[k].callId := by
      intro k h_k
      by_cases h_eq_idx : k = idx
      · subst h_eq_idx
        have : (post.tools[k]'(by rw [h_len]; exact h_k)) = toolPost := by
          have h_k_set : (pre.tools.set k toolPost)[k]'(by rw [List.length_set]; exact h_k)
                          = toolPost :=
            List.getElem_set_self (l := pre.tools) (i := k) (a := toolPost)
              (h := by rw [List.length_set]; exact h_k)
          have hk1 : post.tools[k]'(by rw [h_len]; exact h_k)
                      = (pre.tools.set k toolPost)[k]'(by rw [List.length_set]; exact h_k) := by
            congr 1 <;> rw [h_tools]
          rw [hk1]; exact h_k_set
        rw [this, h_callId_eq, ← h_pre_idx_eq]
      · -- k ≠ idx: set leaves the element unchanged.
        have h_k_set : (pre.tools.set idx toolPost)[k]'(by rw [List.length_set]; exact h_k)
                        = pre.tools[k] :=
          List.getElem_set_ne (l := pre.tools) (i := idx) (j := k) (a := toolPost)
            (h := fun h => h_eq_idx h.symm) (hj := by rw [List.length_set]; exact h_k)
        have hk1 : post.tools[k]'(by rw [h_len]; exact h_k)
                    = (pre.tools.set idx toolPost)[k]'(by rw [List.length_set]; exact h_k) := by
          congr 1 <;> rw [h_tools]
        rw [hk1, h_k_set]
    have h_eq' : pre.tools[i].callId = pre.tools[j].callId := by
      rw [← h_callId_at i h_i', ← h_callId_at j h_j']; exact h_eq
    exact h_inv i j h_i' h_j' h_eq'

end ComposedState
