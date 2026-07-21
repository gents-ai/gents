import Proofs.Background.State
import Proofs.Background.Bridge
import Proofs.Session.Transition
import Proofs.Transcript.Transition

/-!
# Subagent Bridge Transitions

Six transitions on `BridgedState`:
  • parent_step           — lift any ComposedState transition on the parent
  • child_step            — lift any ComposedState transition on the child
  • bridge_spawn          — materialize the bridge edge (new parent tool + new child request)
  • bridge_complete       — child .completed → parent tool .completed (with persistence)
  • bridge_failure        — child non-.completed terminal → parent tool .failed/.cancelled
  • bridge_cancel_cascade — parent terminal w/ cascade → child interruptRequestedAt set

Plus `Trace`, the reflexive-transitive closure used in liveness statements.
-/

namespace Subagent
namespace BridgedState

inductive Transition : BridgedState → BridgedState → Prop where

  | parent_step {pre post : BridgedState}
      (h_step          : ComposedState.Transition pre.parent post.parent)
      (h_child_eq      : post.child = pre.child)
      (h_bridgeId_eq   : post.bridgeCallId = pre.bridgeCallId)
      (h_link_pre      : pre.linked)
      (h_link_post     : post.linked)
      : Transition pre post

  | child_step {pre post : BridgedState}
      (h_step          : ComposedState.Transition pre.child post.child)
      (h_parent_eq     : post.parent = pre.parent)
      (h_bridgeId_eq   : post.bridgeCallId = pre.bridgeCallId)
      (h_link_pre      : pre.linked)
      (h_link_post     : post.linked)
      : Transition pre post

  | bridge_spawn {pre post : BridgedState}
      {newTool : ToolExecution.ToolCallContext}
      (h_parent_proc   : pre.parent.request.state = .processing)
      (h_depth_ok      : pre.parent.request.subagentDepth + 1 ≤ maxSubagentDepth)
      -- new parent tool with the right shape:
      (h_newTool_callId : newTool.callId = post.bridgeCallId)
      (h_newTool_state  : newTool.state = .pending)
      (h_newTool_child  : newTool.childRequestId = some post.child.requestId)
      -- post.parent.tools is fully described by appending the new bridge tool.
      -- Set-style/append-style description: closes the underdetermination
      -- that would otherwise allow adversarial duplicates to slip into
      -- post.parent.tools, supporting INV-UNIQUE preservation.
      (h_tools_append  : post.parent.tools = pre.parent.tools ++ [newTool])
      -- new child request with parent linkage and depth = parent depth + 1:
      (h_post_child :
         post.child.request.state = .pending ∧
         post.child.request.causedByParentRequestId = some pre.parent.requestId ∧
         post.child.request.causedByParentToolCallId = some post.bridgeCallId ∧
         post.child.request.subagentDepth = pre.parent.request.subagentDepth + 1 ∧
         post.child.request.interruptRequestedAt = none)
      -- structural-identity guard: the new child request is freshly minted
      -- with no tools yet. Required for INV-UNIQUE preservation on the child
      -- side (an empty tool list trivially satisfies `UniqueCallIds`).
      (h_post_child_tools : post.child.tools = [])
      -- spawn doesn't progress the parent's narrative:
      (h_request_eq    : post.parent.request = pre.parent.request)
      -- structural-identity guard: the parent's top-level requestId is
      -- preserved across the spawn. Required for INV-LINK's parentLink ↔
      -- childLink symmetry under any reachable trace.
      (h_parent_id_eq  : post.parent.requestId = pre.parent.requestId)
      -- callId-freshness guard: the new bridge tool's callId is not already
      -- present in `pre.parent.tools`. Operationally true at the runtime —
      -- fresh callIds are minted at spawn time, never reused. Required for
      -- INV-UNIQUE (`UniqueCallIds`) preservation across `bridge_spawn`.
      (h_callId_fresh  : ∀ t ∈ pre.parent.tools, t.callId ≠ post.bridgeCallId)
      : Transition pre post

  | bridge_complete {pre post : BridgedState}
      {idx : Nat} {tPre tPost : ToolExecution.ToolCallContext}
      (h_second_done   : pre.terminalOf = .completed)
      -- pre bridge tool, located at a specific index. Pinning the index +
      -- describing post.parent.tools via .set fully determines the post-state
      -- (matches `tool_step`'s pattern), which is what makes
      -- `UniqueCallIds`-style invariants liftable to `BridgedState`.
      (h_idx_pre       : pre.parent.tools[idx]? = some tPre)
      (h_pre_callId    : tPre.callId = pre.bridgeCallId)
      (h_pre_state     : tPre.state = .running)
      (h_pre_persisted : tPre.persistence = .committed)
      (h_pre_child     : tPre.childRequestId = some pre.child.requestId)
      -- post bridge tool: state advances to .completed; callId and childRequestId
      -- are preserved (the bridge tool retains its identity and link).
      (h_post_callId   : tPost.callId = pre.bridgeCallId)
      (h_post_state    : tPost.state = .completed)
      (h_post_child    : tPost.childRequestId = some pre.child.requestId)
      -- post.parent.tools is fully described by replacing the bridge tool at
      -- idx with tPost. Closes the underdetermination that would otherwise
      -- allow adversarial duplicates to slip into post.parent.tools.
      (h_tools_set     : post.parent.tools = pre.parent.tools.set idx tPost)
      (h_request_eq    : post.parent.request = pre.parent.request)
      (h_child_eq      : post.child = pre.child)
      (h_bridgeId_eq   : post.bridgeCallId = pre.bridgeCallId)
      -- structural-identity guard: the parent's top-level requestId is
      -- preserved (only the bridge tool's state advances at the parent).
      (h_parent_id_eq  : post.parent.requestId = pre.parent.requestId)
      : Transition pre post

  | bridge_failure {pre post : BridgedState}
      {idx : Nat} {tPre tPost : ToolExecution.ToolCallContext}
      (h_second_term   : pre.terminalOf.isFailure)
      -- pre bridge tool, located at a specific index. Same set-style description
      -- as `bridge_complete` — see the comment there.
      (h_idx_pre       : pre.parent.tools[idx]? = some tPre)
      (h_pre_callId    : tPre.callId = pre.bridgeCallId)
      (h_pre_state     : tPre.state = .running)
      (h_pre_child     : tPre.childRequestId = some pre.child.requestId)
      -- post bridge tool: state moves to .failed/.cancelled; callId and
      -- childRequestId preserved.
      (h_post_callId   : tPost.callId = pre.bridgeCallId)
      (h_post_state    : tPost.state = .failed ∨ tPost.state = .cancelled)
      (h_post_child    : tPost.childRequestId = some pre.child.requestId)
      -- Set-style description of post.parent.tools.
      (h_tools_set     : post.parent.tools = pre.parent.tools.set idx tPost)
      (h_request_eq    : post.parent.request = pre.parent.request)
      (h_child_eq      : post.child = pre.child)
      (h_bridgeId_eq   : post.bridgeCallId = pre.bridgeCallId)
      -- structural-identity guard: the parent's top-level requestId is
      -- preserved (only the bridge tool's state advances at the parent).
      (h_parent_id_eq  : post.parent.requestId = pre.parent.requestId)
      : Transition pre post

  | bridge_cancel_cascade {pre post : BridgedState}
      (h_parent_term   : isTerminal pre.parent.request.state ∨
                         (∃ t ∈ pre.parent.tools,
                            t.callId = pre.bridgeCallId ∧
                            t.state = .cancelled))
      (h_cascade_pol   : ∃ t ∈ pre.parent.tools,
                           t.callId = pre.bridgeCallId ∧
                           t.cancelPolicy = .cascade)
      (h_interrupt_set : post.child.request.interruptRequestedAt.isSome)
      (h_parent_eq     : post.parent = pre.parent)
      (h_bridgeId_eq   : post.bridgeCallId = pre.bridgeCallId)
      -- structural-identity guards on the child: only `interruptRequestedAt`
      -- is allowed to change. Top-level child requestId, lineage fields, and
      -- depth are preserved. (Subsequent inner transitions on the child use
      -- `child_step` to drive .interrupted; bridge_cancel_cascade is
      -- structurally inert except for setting the interrupt timestamp.)
      (h_child_id_eq   : post.child.requestId = pre.child.requestId)
      (h_child_caused_req_eq :
         post.child.request.causedByParentRequestId =
           pre.child.request.causedByParentRequestId)
      (h_child_caused_tool_eq :
         post.child.request.causedByParentToolCallId =
           pre.child.request.causedByParentToolCallId)
      (h_child_depth_eq :
         post.child.request.subagentDepth = pre.child.request.subagentDepth)
      -- structural-identity guard: child's tool list is unchanged. The cascade
      -- step is operationally a single-field update on `interruptRequestedAt`;
      -- child tools survive verbatim. Required for INV-UNIQUE preservation
      -- (and would also be required for any future child-tools invariant).
      (h_child_tools_eq : post.child.tools = pre.child.tools)
      : Transition pre post

/-- Reflexive-transitive closure for liveness statements. -/
inductive Trace : BridgedState → BridgedState → Prop where
  | refl {s : BridgedState} : Trace s s
  | step {s₁ s₂ s₃ : BridgedState} :
      Transition s₁ s₂ → Trace s₂ s₃ → Trace s₁ s₃

/-- Named wrapper around the existing `bridge_cancel_cascade` constructor.
    This is not a new transition; it packages the constructor arguments so
    higher-level composed witnesses can identify that the cascade arm fired. -/
structure BridgeCancelCascadeStep
    (pre post : BridgedState) : Prop where
  h_parent_term   : isTerminal pre.parent.request.state ∨
                    (∃ t ∈ pre.parent.tools,
                       t.callId = pre.bridgeCallId ∧
                       t.state = .cancelled)
  h_cascade_pol   : ∃ t ∈ pre.parent.tools,
                      t.callId = pre.bridgeCallId ∧
                      t.cancelPolicy = .cascade
  h_interrupt_set : post.child.request.interruptRequestedAt.isSome
  h_child_request_eq :
    post.child.request =
      { pre.child.request with
        interruptRequestedAt := post.child.request.interruptRequestedAt }
  h_parent_eq     : post.parent = pre.parent
  h_bridgeId_eq   : post.bridgeCallId = pre.bridgeCallId
  h_child_id_eq   : post.child.requestId = pre.child.requestId
  h_child_caused_req_eq :
    post.child.request.causedByParentRequestId =
      pre.child.request.causedByParentRequestId
  h_child_caused_tool_eq :
    post.child.request.causedByParentToolCallId =
      pre.child.request.causedByParentToolCallId
  h_child_depth_eq :
    post.child.request.subagentDepth = pre.child.request.subagentDepth
  h_child_tools_eq : post.child.tools = pre.child.tools

theorem BridgeCancelCascadeStep.to_transition
    {pre post : BridgedState}
    (h : BridgeCancelCascadeStep pre post) :
    Transition pre post :=
  Transition.bridge_cancel_cascade
    h.h_parent_term
    h.h_cascade_pol
    h.h_interrupt_set
    h.h_parent_eq
    h.h_bridgeId_eq
    h.h_child_id_eq
    h.h_child_caused_req_eq
    h.h_child_caused_tool_eq
    h.h_child_depth_eq
    h.h_child_tools_eq

/-- The request-level interrupt transition used by
    `steer_subagent(interrupt=true)`. This mirrors the existing
    `RequestContext.Transition.interrupt_*` constructors so future request
    transitions cannot accidentally satisfy the steering witness just because
    they also end in `.interrupted`. -/
inductive RequestInterruptStep : RequestContext → RequestContext → Prop where
  | before_claim {pre post : RequestContext} :
      pre.state = .pending →
      pre.admission = .released →
      pre.interruptRequestedAt.isSome →
      post = { pre with state := .interrupted, admission := .released } →
      RequestInterruptStep pre post
  | claimed {pre post : RequestContext} :
      pre.state = .claimed →
      (pre.admission = .waiting ∨ pre.admission = .acquired) →
      pre.interruptRequestedAt.isSome →
      post = { pre with state := .interrupted, admission := .released } →
      RequestInterruptStep pre post
  | processing {pre post : RequestContext} :
      pre.state = .processing →
      pre.admission = .executing →
      pre.interruptRequestedAt.isSome →
      post = { pre with state := .interrupted, admission := .released } →
      RequestInterruptStep pre post

theorem RequestInterruptStep.to_transition
    {pre post : RequestContext}
    (h : RequestInterruptStep pre post) :
    RequestContext.Transition pre post := by
  cases h with
  | before_claim h_state h_admission h_interrupt h_post =>
      exact RequestContext.Transition.interrupt_before_claim
        h_state h_admission h_interrupt h_post
  | claimed h_state h_admission h_interrupt h_post =>
      exact RequestContext.Transition.interrupt_claimed
        h_state h_admission h_interrupt h_post
  | processing h_state h_admission h_interrupt h_post =>
      exact RequestContext.Transition.interrupt_processing
        h_state h_admission h_interrupt h_post

/-- Named wrapper for the child-side request interrupt after cascade has set
    `interruptRequestedAt`. It lifts one of the existing request-level
    interrupt constructors through `ComposedState.request_step`; no new
    transition logic is introduced here. -/
structure ChildInterruptStep
    (pre post : BridgedState) : Prop where
  h_request_interrupt :
    RequestInterruptStep pre.child.request post.child.request
  h_process_eq : post.child.process = pre.child.process
  h_call_eq : post.child.call = pre.child.call
  h_tools_eq : post.child.tools = pre.child.tools
  h_requestId_eq : post.child.requestId = pre.child.requestId
  h_pending_accepts :
    pre.child.request.state = .pending → pre.child.process.acceptsWork
  h_no_foreground_block :
    post.child.request.progressSeq > pre.child.request.progressSeq ∨
      (pre.child.request.state = .claimed ∧
       post.child.request.state = .processing) →
      ¬ ∃ t ∈ pre.child.tools, t.awaitMode = .foreground ∧
                            ¬ isTerminal t.state
  h_parent_eq : post.parent = pre.parent
  h_bridgeId_eq : post.bridgeCallId = pre.bridgeCallId
  h_link_pre : pre.linked
  h_link_post : post.linked

theorem ChildInterruptStep.to_transition
    {pre post : BridgedState}
    (h : ChildInterruptStep pre post) :
    Transition pre post :=
  Transition.child_step
    (ComposedState.Transition.request_step
      (RequestInterruptStep.to_transition h.h_request_interrupt)
      h.h_process_eq
      h.h_call_eq
      h.h_tools_eq
      h.h_requestId_eq
      h.h_pending_accepts
      h.h_no_foreground_block)
    h.h_parent_eq
    h.h_bridgeId_eq
    h.h_link_pre
    h.h_link_post

/-- Canonical proof-model key for a child session's background-completion
    wake-up queue. Runtime encodes this as `background_completion:<session>`;
    the Lean queue key is opaque Nat, so the session id itself is the stable
    representative. -/
def backgroundCompletionQueueKey (childSessionId : SessionId) :
    SessionQueue.QueueKey :=
  childSessionId

/-- Current-surface witness for `steer_subagent(interrupt=true)`.
    The bridge-affecting part is the existing cancel cascade plus the child
    interrupt transition. The concrete queue/transcript states are parameters
    of the witness because those models live outside `BridgedState`. -/
structure SteerWithInterrupt
    (pre post : BridgedState)
    (childSessionId : SessionId)
    (queuePre queueDrained queuePost : SessionQueue.SessionQueueState)
    (transcriptPre transcriptPost : Transcript.TranscriptState)
    (childRequestId : RequestId)
    (steeringRequestId : RequestId)
    (steeringMessage : String) : Prop where
  h_child_active :
    pre.child.requestId = childRequestId ∧
    ¬ isTerminal pre.child.request.state
  h_bridge_compose :
    ∃ cascaded interrupted : BridgedState,
      BridgeCancelCascadeStep pre cascaded ∧
      ChildInterruptStep cascaded interrupted ∧
      interrupted.child.requestId = childRequestId ∧
      Trace interrupted post
  h_queue_pre_session : queuePre.sessionId = childSessionId
  h_queue_drained_session : queueDrained.sessionId = childSessionId
  h_queue_post_session : queuePost.sessionId = childSessionId
  h_drain_transition :
    SessionQueue.Transition queuePre queueDrained
  h_drain_shape :
    queueDrained =
      queuePre.drainAutomatedWakeups
        SessionQueue.QueueSource.backgroundCompletion
        (some (backgroundCompletionQueueKey childSessionId))
  h_append_compose :
    ∃ entry : SessionQueue.QueueEntry,
      SessionQueue.Transition queueDrained queuePost ∧
      queuePost = queueDrained.appendPending entry ∧
      entry.requestId = steeringRequestId ∧
      entry.source = SessionQueue.QueueSource.steering ∧
      entry.policy = SessionQueue.QueuePolicy.append ∧
      entry.queueKey = none
  h_transcript_pre_session : transcriptPre.sessionId = childSessionId
  h_transcript_post_session : transcriptPost.sessionId = childSessionId
  h_transcript_append :
    ∃ messageId : Transcript.MessageId,
      steeringMessage ≠ "" ∧
      Transcript.Transition transcriptPre transcriptPost ∧
      transcriptPost =
        transcriptPre.appendUserMessage
          messageId
          Transcript.MessageKind.ordinary

end BridgedState
end Subagent
