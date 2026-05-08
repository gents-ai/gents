import Proofs.Subagent.State
import Proofs.Subagent.Bridge

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
      (h_parent_proc   : pre.parent.request.state = .processing)
      (h_depth_ok      : pre.parent.request.subagentDepth + 1 ≤ maxSubagentDepth)
      -- new parent tool with the right shape:
      (h_post_parent_tool :
         ∃ t ∈ post.parent.tools,
           t.callId = post.bridgeCallId ∧
           t.state = .pending ∧
           t.childRequestId = some post.child.requestId)
      -- new child request with parent linkage and depth = parent depth + 1:
      (h_post_child :
         post.child.request.state = .pending ∧
         post.child.request.causedByParentRequestId = some pre.parent.requestId ∧
         post.child.request.causedByParentToolCallId = some post.bridgeCallId ∧
         post.child.request.subagentDepth = pre.parent.request.subagentDepth + 1)
      -- spawn doesn't progress the parent's narrative:
      (h_request_eq    : post.parent.request = pre.parent.request)
      : Transition pre post

  | bridge_complete {pre post : BridgedState}
      (h_child_done    : pre.child.request.state = .completed)
      (h_running       : ∃ t ∈ pre.parent.tools,
                           t.callId = pre.bridgeCallId ∧ t.state = .running)
      (h_persisted     : ∃ t ∈ pre.parent.tools,
                           t.callId = pre.bridgeCallId ∧
                           t.persistence = .committed)
      (h_post_tool     : ∃ t ∈ post.parent.tools,
                           t.callId = pre.bridgeCallId ∧ t.state = .completed)
      (h_others_eq     : ∀ t ∈ pre.parent.tools, t.callId ≠ pre.bridgeCallId →
                          t ∈ post.parent.tools)
      (h_request_eq    : post.parent.request = pre.parent.request)
      (h_child_eq      : post.child = pre.child)
      (h_bridgeId_eq   : post.bridgeCallId = pre.bridgeCallId)
      : Transition pre post

  | bridge_failure {pre post : BridgedState}
      (h_child_term    : pre.child.request.state = .failed ∨
                         pre.child.request.state = .dead ∨
                         pre.child.request.state = .interrupted ∨
                         pre.child.request.state = .superseded)
      (h_running       : ∃ t ∈ pre.parent.tools,
                           t.callId = pre.bridgeCallId ∧ t.state = .running)
      (h_post_tool     : ∃ t ∈ post.parent.tools,
                           t.callId = pre.bridgeCallId ∧
                           (t.state = .failed ∨ t.state = .cancelled))
      (h_others_eq     : ∀ t ∈ pre.parent.tools, t.callId ≠ pre.bridgeCallId →
                          t ∈ post.parent.tools)
      (h_request_eq    : post.parent.request = pre.parent.request)
      (h_child_eq      : post.child = pre.child)
      (h_bridgeId_eq   : post.bridgeCallId = pre.bridgeCallId)
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
      : Transition pre post

/-- Reflexive-transitive closure for liveness statements. -/
inductive Trace : BridgedState → BridgedState → Prop where
  | refl {s : BridgedState} : Trace s s
  | step {s₁ s₂ s₃ : BridgedState} :
      Transition s₁ s₂ → Trace s₂ s₃ → Trace s₁ s₃

end BridgedState
end Subagent
