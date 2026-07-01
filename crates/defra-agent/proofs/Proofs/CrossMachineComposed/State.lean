import Proofs.Process
import Proofs.Request
import Proofs.InferenceCall
import Proofs.Persistence
import Proofs.ToolExecution
import Proofs.ManagedExec.Composed

/-!
# Cross-Machine Composition

Combines the process, request/persistence, inference-call, and tool-execution
machines into one composed state.
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
    unique within a well-formed composed state; callers that need uniqueness
    should consume the global `WellFormed`/`UniqueCallIds` invariant. -/
def findToolByCallId (s : ComposedState) (callId : ToolExecution.ToolCallId) :
    Option ToolExecution.ToolCallContext :=
  s.tools.find? (fun t => t.callId = callId)

/-- Structural coherence between a composed state and one of its in-flight
    tool calls: the tool's identifier, deadline, and clock track the parent
    request. Promoted from inline conjuncts in the original `tool_step`
    constructor and C1/C1'/C2 theorem signatures.

    The predicate is per-tool: `Coherent pre toolPre` says `toolPre` (one
    element of `pre.tools`) is structurally synced with `pre.request`. The
    global well-formedness invariant lifts this predicate over the *live*
    (non-detached) tools; detached tools are governed by the complementary
    `Persistent` predicate (linkage only), since they own their own lifetime.

    NOTE (modeling scope): `clock_advance` currently still maps every tool's
    clock — including detached ones — so a detached tool happens to remain
    clock-coherent in this model. `AllToolsPersistent` only *requires* linkage
    of it; giving detached tools genuinely independent clocks is a follow-up. -/
def Coherent (pre : ComposedState) (toolPre : ToolExecution.ToolCallContext) : Prop :=
  toolPre.requestId = pre.requestId ∧
  toolPre.deadline = pre.request.deadline ∧
  toolPre.currentTime = pre.request.currentTime

/-- A tool is *persistent* (detached) when its cancel policy is `detach`: it
    outlives the parent request, so it is governed by linkage (`Persistent`)
    rather than by the live clock/deadline `Coherent` predicate. -/
def IsDetached (t : ToolExecution.ToolCallContext) : Prop :=
  t.cancelPolicy = .detach

instance (t : ToolExecution.ToolCallContext) : Decidable (IsDetached t) := by
  unfold IsDetached; infer_instance

/-- Persistent-process well-formedness: a detached tool still belongs to its
    parent request (linkage, for audit/lineage) and is a real bridged subagent
    (`childRequestId` set). Unlike `Coherent`, it imposes no clock/deadline sync,
    reflecting that a detached child owns its own lifetime. -/
def Persistent (s : ComposedState) (t : ToolExecution.ToolCallContext) : Prop :=
  t.requestId = s.requestId ∧ t.childRequestId.isSome

/-- List-level coherence: every *live* (non-detached) tool currently carried by
    the composed state is structurally synced with the parent request. Detached
    tools are governed by `AllToolsPersistent` instead. -/
def AllToolsCoherent (s : ComposedState) : Prop :=
  ∀ t ∈ s.tools, ¬ IsDetached t → Coherent s t

/-- List-level persistence: every detached tool is a linked bridged subagent. -/
def AllToolsPersistent (s : ComposedState) : Prop :=
  ∀ t ∈ s.tools, IsDetached t → Persistent s t

/-- List-level linkage: every tool row belongs to the composed request id. This
    is redundant with `AllToolsCoherent`, but it is named separately because
    linkage is a common audit boundary in higher-level composed theorems. -/
def AllToolsLinked (s : ComposedState) : Prop :=
  ∀ t ∈ s.tools, t.requestId = s.requestId

/-- Tool rows are only present once the parent request has reached processing
    or a later terminal state. This excludes malformed pending/claimed states
    with pre-existing tools, which would otherwise be able to take request
    transitions that rewrite request deadline state while leaving tools stale. -/
def NoToolsBeforeProcessing (s : ComposedState) : Prop :=
  ∀ t ∈ s.tools, s.request.state ≠ .pending ∧ s.request.state ≠ .claimed

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

    `slot_acquire` abstracts the external fleet/scheduler grant that moves a
    claimed request from waiting to acquired inside this composed boundary.
    `request_interrupt` abstracts the external interrupt signal that latches
    `interruptRequestedAt`, enabling the `interrupt_*` request transitions.
    `clock_advance` is the synchronized time path: the parent request clock
    and all carried tool clocks move together, preserving `Coherent`.
    `tool_spawn` is the list-growth constructor. `tool_step` is deliberately
    coherence-preserving: standalone tool clock/deadline drift is not a valid
    composed step unless the parent request snapshot remains synchronized.

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
  | slot_acquire {pre post : ComposedState} :
      pre.request.state = .claimed →
      pre.request.admission = .waiting →
      post.request = { pre.request with admission := .acquired } →
      post.process = pre.process →
      post.call = pre.call →
      post.tools = pre.tools →
      post.requestId = pre.requestId →
      Transition pre post
  | request_interrupt {pre post : ComposedState} (t : Time) :
      -- Abstracts the external interrupt signal that latches
      -- `interruptRequestedAt`, enabling the `interrupt_*` request transitions.
      -- It touches no coherence-relevant field (state/admission/deadline/clock/
      -- tools are all unchanged), so it preserves `WellFormed` trivially.
      post.request = { pre.request with interruptRequestedAt := some t } →
      post.process = pre.process →
      post.call = pre.call →
      post.tools = pre.tools →
      post.requestId = pre.requestId →
      Transition pre post
  | clock_advance {pre post : ComposedState} (t : Time) :
      pre.request.currentTime ≤ t →
      post.request = { pre.request with currentTime := t } →
      post.process = pre.process →
      post.call = pre.call →
      post.tools = pre.tools.map (fun tool => { tool with currentTime := t }) →
      post.requestId = pre.requestId →
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
  | tool_spawn {pre post : ComposedState}
               {newTool : ToolExecution.ToolCallContext} :
      pre.request.state = .processing →
      newTool.state = .pending →
      post.tools = pre.tools ++ [newTool] →
      post.request = pre.request →
      post.process = pre.process →
      post.call = pre.call →
      post.requestId = pre.requestId →
      Coherent post newTool →
      -- Persistence guard: a detached spawn must be a linked bridged subagent.
      (IsDetached newTool → Persistent post newTool) →
      (∀ t ∈ pre.tools, t.callId ≠ newTool.callId) →
      -- A newly spawned foreground pending tool is live immediately, so it is
      -- admitted only when no other foreground live tool already exists.
      (newTool.awaitMode = .foreground →
        ¬ ∃ t ∈ pre.tools, t.awaitMode = .foreground ∧
                            ¬ isTerminal t.state) →
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
      -- request before and after the inner step. The post guard rules out
      -- standalone tool clock/deadline drift that would break global
      -- `AllToolsCoherent` preservation.
      Coherent pre toolPre →
      Coherent post toolPost →
      -- Persistence guard: if the inner step detaches the tool (cancelPolicy →
      -- detach), the result must be a linked bridged subagent.
      (IsDetached toolPost → Persistent post toolPost) →
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

end ComposedState
