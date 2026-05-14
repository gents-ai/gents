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

/-- The bridge's second leg: either a child request (R4) or an in-process
    tool execution (R6). -/
inductive SecondLeg where
  | subagent (child : ComposedState)
  | tool (ctx : ToolExecution.ToolCallContext)
  deriving Repr

namespace SecondLeg

def kind : SecondLeg → BackgroundedKind
  | .subagent _ => .Subagent
  | .tool _ => .Tool

/-- Project the observed second-leg terminal into the bridge vocabulary. -/
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

/-- Rust projection used by `bridge_failure`: interrupted maps to cancelled;
    all other non-completed terminals map to failed. -/
def projectedToolState : ChildTerminal → ToolExecution.ToolCallState
  | .interrupted => .cancelled
  | _ => .failed

theorem projected_failure_state
    (t : ChildTerminal)
    (h : t.isFailure) :
    t.projectedToolState = .failed ∨ t.projectedToolState = .cancelled := by
  cases t <;> simp [ChildTerminal.isFailure, projectedToolState] at h ⊢

end ChildTerminal

/-- A paired parent/child composed state representing one subagent invocation
    edge. Structural guards are stated as predicates rather than baked into
    the constructor; they hold for any state reachable from `bridge_spawn`. -/
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
  s.terminalOf = .completed

/-- The child terminated in any non-completed terminal state. -/
def bridgeChildFailed (s : BridgedState) : Prop :=
  s.terminalOf.isFailure

end BridgedState

end Subagent
