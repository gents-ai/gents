import Proofs.CrossMachineComposed
import Proofs.Background.State

namespace Subagent

namespace ChildTerminal

def projectedToolState : ChildTerminal → ToolExecution.ToolCallState
  | .interrupted => .cancelled
  | _ => .failed

theorem projected_failure_state
    (t : ChildTerminal)
    (h : t.isFailure) :
    t.projectedToolState = .failed ∨ t.projectedToolState = .cancelled := by
  cases t <;> simp [ChildTerminal.isFailure, projectedToolState] at h ⊢

end ChildTerminal

structure BridgedState where
  parent       : ComposedState
  child        : ComposedState
  bridgeCallId : ToolExecution.ToolCallId
  deriving Repr

namespace BridgedState

/-- The child request is the sole execution-state owner for a subagent bridge.
Native background tools retain their own ToolExecution/ManagedExec owners. -/
def terminalOf (s : BridgedState) : ChildTerminal :=
  match s.child.request.state with
  | .completed => .completed
  | .failed => .failed
  | .dead => .dead
  | .interrupted => .interrupted
  | .superseded => .superseded
  | _ => .running

/-- Bridge observations follow the actual child, including after child steps. -/
theorem terminalOf_completed (s : BridgedState)
    (h : s.child.request.state = .completed) : s.terminalOf = .completed := by
  simp [terminalOf, h]

theorem terminalOf_interrupted (s : BridgedState)
    (h : s.child.request.state = .interrupted) : s.terminalOf = .interrupted := by
  simp [terminalOf, h]

def parentLink (s : BridgedState) : Prop :=
  ∃ t ∈ s.parent.tools,
    t.callId = s.bridgeCallId ∧
    t.childRequestId = some s.child.requestId

def childLink (s : BridgedState) : Prop :=
  s.child.request.causedByParentRequestId = some s.parent.requestId ∧
  s.child.request.causedByParentToolCallId = some s.bridgeCallId

def linked (s : BridgedState) : Prop :=
  s.parentLink ∧ s.childLink

def bridgeObservedCompleted (s : BridgedState) : Prop :=
  s.terminalOf = .completed

def bridgeChildFailed (s : BridgedState) : Prop :=
  s.terminalOf.isFailure

end BridgedState

end Subagent
