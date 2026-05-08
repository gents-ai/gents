import Proofs.Subagent.State
import Proofs.Subagent.Bridge

/-!
# Subagent Bridge Transitions

Six transitions on `BridgedState`. Three landed here:
  • parent_step  — lift any ComposedState transition on the parent
  • child_step   — lift any ComposedState transition on the child
  • bridge_spawn — materialize the bridge edge (new parent tool + new child request)

The other three (bridge_complete, bridge_failure, bridge_cancel_cascade) land
in the next task.

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

/-- Reflexive-transitive closure for liveness statements. -/
inductive Trace : BridgedState → BridgedState → Prop where
  | refl {s : BridgedState} : Trace s s
  | step {s₁ s₂ s₃ : BridgedState} :
      Transition s₁ s₂ → Trace s₂ s₃ → Trace s₁ s₃

end BridgedState
end Subagent
