import Proofs.CrossMachineComposed
import Proofs.Background.State

namespace Subagent

inductive SecondLeg where
  | subagent (child : ComposedState)
  | tool (ctx : ToolExecution.ToolCallContext)
  deriving Repr

namespace SecondLeg

def kind : SecondLeg → BackgroundedKind
  | .subagent _ => .Subagent
  | .tool _ => .Tool

def terminalOf : SecondLeg → ChildTerminal
  | .subagent child =>
      match child.request.state with
      | .completed => .completed
      | .failed => .failed
      | .dead => .dead
      | .interrupted => .interrupted
      | .superseded => .superseded
      | _ => .running
  | .tool ctx =>
      match ctx.state with
      | .completed => .completed
      | .failed => .failed
      | .timedOut => .dead
      | .cancelled => .interrupted
      | _ => .running

end SecondLeg

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
  secondLeg    : SecondLeg := .subagent child
  bridgeCallId : ToolExecution.ToolCallId
  deriving Repr

namespace BridgedState

def kind (s : BridgedState) : BackgroundedKind :=
  s.secondLeg.kind

def terminalOf (s : BridgedState) : ChildTerminal :=
  s.secondLeg.terminalOf

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
