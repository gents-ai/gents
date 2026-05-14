import Proofs.Composed
import Proofs.Background.State

/-!
# BridgedState

A paired parent/child composed state representing one subagent invocation
edge.  Structural guards are stated as predicates rather than baked into
the constructor; they hold for any state reachable from `bridge_spawn`
(proved in `Subagent.Transition`).

Lives in a separate file from `Subagent.State` to avoid the import cycle:
  `Subagent.State → (would need) Composed → Request → ToolExecution.State → Subagent.State`
-/

namespace Subagent

/-- A paired parent/child composed state representing one subagent invocation
    edge. Structural guards are stated as predicates rather than baked into
    the constructor; they hold for any state reachable from `bridge_spawn`. -/
structure BridgedState where
  parent       : ComposedState
  child        : ComposedState
  bridgeCallId : ToolExecution.ToolCallId
  deriving Repr

namespace BridgedState

/-- The bridge tool exists on the parent and points to the child. -/
def parentLink (s : BridgedState) : Prop :=
  ∃ t ∈ s.parent.tools,
    t.callId = s.bridgeCallId ∧
    t.childRequestId = some s.child.requestId

/-- The child request points back to the parent. -/
def childLink (s : BridgedState) : Prop :=
  s.child.request.causedByParentRequestId = some s.parent.requestId ∧
  s.child.request.causedByParentToolCallId = some s.bridgeCallId

/-- The full link is symmetric. -/
def linked (s : BridgedState) : Prop :=
  s.parentLink ∧ s.childLink

/-- The child has been observed reaching .completed. -/
def bridgeObservedCompleted (s : BridgedState) : Prop :=
  s.child.request.state = .completed

/-- The child terminated in any non-completed terminal state. -/
def bridgeChildFailed (s : BridgedState) : Prop :=
  s.child.request.state = .failed ∨
  s.child.request.state = .dead ∨
  s.child.request.state = .interrupted ∨
  s.child.request.state = .superseded

end BridgedState

end Subagent
