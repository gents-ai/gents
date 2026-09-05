import Proofs.Request
import Proofs.Process
import Proofs.InferenceCall
import Proofs.Persistence
import Proofs.Conformance.Triggers

inductive GentsProcessState where
  | uninitialized
  | recovering
  | ready
  | shuttingDown
  | shutdown
  deriving DecidableEq, Repr

namespace GentsProcessState

def toIdeal : GentsProcessState → ProcessState
  | .uninitialized => .uninitialized
  | .recovering => .recovering
  | .ready => .ready
  | .shuttingDown => .shuttingDown
  | .shutdown => .shutdown

def toDefraDB : GentsProcessState → String
  | .uninitialized => "uninitialized"
  | .recovering => "recovering"
  | .ready => "ready"
  | .shuttingDown => "shuttingDown"
  | .shutdown => "shutdown"

theorem toIdeal_preserves_terminal (s : GentsProcessState) :
    isTerminal s.toIdeal ↔ s = .shutdown := by
  cases s <;> simp [toIdeal, HasTerminal.isTerminal, ProcessState.instHasTerminal]

theorem recovering_blocks_work :
    ¬ (toIdeal .recovering).acceptsWork := by
  simp [toIdeal, ProcessState.acceptsWork]

end GentsProcessState

inductive GentsInferenceCallState where
  | queued
  | running
  | cancelled
  | completed
  | failed
  deriving DecidableEq, Repr

namespace GentsInferenceCallState

def toIdeal : GentsInferenceCallState → InferenceCallState
  | .queued => .queued
  | .running => .running
  | .cancelled => .cancelled
  | .completed => .completed
  | .failed => .failed

def toDefraDB : GentsInferenceCallState → String
  | .queued => "queued"
  | .running => "running"
  | .cancelled => "cancelled"
  | .completed => "completed"
  | .failed => "failed"

theorem toIdeal_preserves_terminal (s : GentsInferenceCallState) :
    isTerminal s.toIdeal ↔
    (s = .cancelled ∨ s = .completed ∨ s = .failed) := by
  cases s <;> simp [toIdeal, HasTerminal.isTerminal, InferenceCallState.instHasTerminal]

end GentsInferenceCallState
