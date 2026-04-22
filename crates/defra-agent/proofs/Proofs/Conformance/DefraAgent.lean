import Proofs.Request
import Proofs.Process
import Proofs.Persistence

/-!
# Conformance Mapping: defra-agent → Ideal Model

Maps defra-agent's actual states and transitions to the ideal
agent state machine.

defra-agent states (from lifecycle.rs):
  Pending, Claimed, Streaming, Completed, Failed, Superseded

These are implementation-local states. The persisted DefraDB request view now
carries the lifecycle refinement via `AgentRequest.lifecycle_state`; call-level
admission state lives on `InferenceCall`.

Ideal model states:
  pending, claimed, processing, inputRequired, completed, failed, superseded, dead
-/

/-- defra-agent's local lifecycle states (from lifecycle.rs). -/
inductive DefraLifecycleState where
  | pending
  | claimed
  | streaming
  | completed
  | failed
  | superseded
  | dead
  | interrupted
  deriving DecidableEq, Repr

namespace DefraLifecycleState

/-- Map defra-agent's local in-process state to the ideal request state.
    Key: local `claimed` refines to persisted `claimed / waiting|acquired`;
    local `streaming` refines to persisted `processing / executing`. -/
def toIdeal : DefraLifecycleState → RequestState
  | .pending => .pending
  | .claimed => .claimed
  | .streaming => .processing
  | .completed => .completed
  | .failed => .failed
  | .superseded => .superseded
  | .dead => .dead
  | .interrupted => .interrupted

/-- The mapping preserves terminal status. -/
theorem toIdeal_preserves_terminal (s : DefraLifecycleState) :
    isTerminal s.toIdeal ↔
    (s = .completed ∨ s = .failed ∨ s = .superseded ∨ s = .dead ∨ s = .interrupted) := by
  cases s <;> simp [toIdeal, HasTerminal.isTerminal, RequestState.instHasTerminal]
  all_goals decide

/-- defra-agent has no recovering process state. -/
def defraProcessToIdeal (hasRecovered : Bool) : ProcessState :=
  if hasRecovered then .ready else .uninitialized

end DefraLifecycleState
