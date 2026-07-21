import Proofs.Request
import Proofs.Process
import Proofs.InferenceCall
import Proofs.Persistence
import Proofs.Conformance.Triggers

/-!
# Conformance Mapping: defra-agent → Ideal Model

Maps defra-agent's actual states and transitions to the ideal
agent state machine.

defra-agent local request states (from lifecycle.rs):
  Pending, Claimed, Streaming, Completed, Failed, Superseded, Dead, Interrupted

These are implementation-local states. The persisted DefraDB request view now
carries the lifecycle refinement via `AgentRequest.lifecycle_state`; call-level
admission state lives on `InferenceCall`.

Lean persisted vocabulary:
  pending, claimed, processing, inputRequired, completed, failed, superseded, dead, interrupted

`inputRequired` is reserved vocabulary only in the current product: Rust parses
and preserves the string for client/protocol parity, but the core request
transition machine has no writer path into that state today.
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

end DefraLifecycleState

/-- defra-agent's persisted `AgentRuntime.process_state` values. -/
inductive DefraProcessState where
  | uninitialized
  | recovering
  | ready
  | shuttingDown
  | shutdown
  deriving DecidableEq, Repr

namespace DefraProcessState

/-- Map persisted Rust runtime states to the Lean process state vocabulary. -/
def toIdeal : DefraProcessState → ProcessState
  | .uninitialized => .uninitialized
  | .recovering => .recovering
  | .ready => .ready
  | .shuttingDown => .shuttingDown
  | .shutdown => .shutdown

/-- Persisted string values for `AgentRuntime.process_state`. -/
def toDefraDB : DefraProcessState → String
  | .uninitialized => "uninitialized"
  | .recovering => "recovering"
  | .ready => "ready"
  | .shuttingDown => "shuttingDown"
  | .shutdown => "shutdown"

/-- The Rust/DefraDB mapping preserves process terminal status. -/
theorem toIdeal_preserves_terminal (s : DefraProcessState) :
    isTerminal s.toIdeal ↔ s = .shutdown := by
  cases s <;> simp [toIdeal, HasTerminal.isTerminal, ProcessState.instHasTerminal]

/-- Recovery is an explicit non-work-accepting startup state. -/
theorem recovering_blocks_work :
    ¬ (toIdeal .recovering).acceptsWork := by
  simp [toIdeal, ProcessState.acceptsWork]

end DefraProcessState

/-- defra-agent's persisted `InferenceCall.call_state` values. -/
inductive DefraInferenceCallState where
  | queued
  | running
  | cancelled
  | completed
  | failed
  deriving DecidableEq, Repr

namespace DefraInferenceCallState

/-- Map persisted Rust call states to the Lean call state vocabulary. -/
def toIdeal : DefraInferenceCallState → InferenceCallState
  | .queued => .queued
  | .running => .running
  | .cancelled => .cancelled
  | .completed => .completed
  | .failed => .failed

/-- Persisted string values for `InferenceCall.call_state`. -/
def toDefraDB : DefraInferenceCallState → String
  | .queued => "queued"
  | .running => "running"
  | .cancelled => "cancelled"
  | .completed => "completed"
  | .failed => "failed"

/-- The Rust/DefraDB mapping preserves terminal call states. -/
theorem toIdeal_preserves_terminal (s : DefraInferenceCallState) :
    isTerminal s.toIdeal ↔
    (s = .cancelled ∨ s = .completed ∨ s = .failed) := by
  cases s <;> simp [toIdeal, HasTerminal.isTerminal, InferenceCallState.instHasTerminal]

end DefraInferenceCallState
