import Proofs.Process
import Proofs.Request
import Proofs.InferenceCall
import Proofs.Persistence
import Proofs.ToolExecution
import Proofs.ManagedExec.Composed

structure ComposedState where
  requestId : RequestId
  process : ProcessState
  request : RequestContext
  call : InferenceCall
  tools : List ToolExecution.ToolCallContext := []
  deriving Repr

namespace ComposedState

def hasToolByCallId (s : ComposedState) (callId : ToolExecution.ToolCallId) : Prop :=
  ∃ t ∈ s.tools, t.callId = callId

instance (s : ComposedState) (callId : ToolExecution.ToolCallId) :
    Decidable (s.hasToolByCallId callId) := by
  unfold hasToolByCallId; infer_instance

def findToolByCallId (s : ComposedState) (callId : ToolExecution.ToolCallId) :
    Option ToolExecution.ToolCallContext :=
  s.tools.find? (fun t => t.callId = callId)

def Coherent (pre : ComposedState) (toolPre : ToolExecution.ToolCallContext) : Prop :=
  toolPre.requestId = pre.requestId ∧
  toolPre.deadline = pre.request.deadline ∧
  toolPre.currentTime = pre.request.currentTime

def IsDetached (t : ToolExecution.ToolCallContext) : Prop :=
  t.cancelPolicy = .detach

instance (t : ToolExecution.ToolCallContext) : Decidable (IsDetached t) := by
  unfold IsDetached; infer_instance

def Persistent (s : ComposedState) (t : ToolExecution.ToolCallContext) : Prop :=
  t.requestId = s.requestId ∧ t.childRequestId.isSome

def AllToolsCoherent (s : ComposedState) : Prop :=
  ∀ t ∈ s.tools, ¬ IsDetached t → Coherent s t

def AllToolsPersistent (s : ComposedState) : Prop :=
  ∀ t ∈ s.tools, IsDetached t → Persistent s t

def AllToolsLinked (s : ComposedState) : Prop :=
  ∀ t ∈ s.tools, t.requestId = s.requestId

def NoToolsBeforeProcessing (s : ComposedState) : Prop :=
  ∀ t ∈ s.tools, s.request.state ≠ .pending ∧ s.request.state ≠ .claimed

theorem coherent_tool_deadline_eq_request_deadline
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_coherent : Coherent pre toolPre) :
    toolPre.deadline = pre.request.deadline :=
  h_coherent.2.1

theorem coherent_tool_deadlineExceeded_iff_request_deadlineExceeded
    {pre : ComposedState} {toolPre : ToolExecution.ToolCallContext}
    (h_coherent : Coherent pre toolPre) :
    toolPre.deadlineExceeded ↔ pre.request.deadlineExceeded := by
  obtain ⟨_, h_deadline_eq, h_time_eq⟩ := h_coherent
  simp [ToolExecution.ToolCallContext.deadlineExceeded,
        RequestContext.deadlineExceeded, h_deadline_eq, h_time_eq]

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
      (IsDetached newTool → Persistent post newTool) →
      (∀ t ∈ pre.tools, t.callId ≠ newTool.callId) →
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
      Coherent pre toolPre →
      Coherent post toolPost →
      (IsDetached toolPost → Persistent post toolPost) →
      (toolPre.awaitMode = .background → toolPost.awaitMode = .foreground →
        ¬ ∃ t ∈ pre.tools, t.awaitMode = .foreground ∧
                            ¬ isTerminal t.state) →
      Transition pre post

inductive Trace : ComposedState → ComposedState → Prop where
  | refl {s : ComposedState} : Trace s s
  | step {s₁ s₂ s₃ : ComposedState} :
      Transition s₁ s₂ → Trace s₂ s₃ → Trace s₁ s₃

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
