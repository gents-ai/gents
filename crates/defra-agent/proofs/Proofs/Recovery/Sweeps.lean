import Proofs.Recovery.Contract
import Proofs.Properties.Liveness
import Proofs.InferenceCall
import Proofs.ToolExecution
import Proofs.Background
import Proofs.StreamingResponse.State

/-!
# Registered Recovery Sweeps

Concrete startup-recovery sweep contracts for persisted collections with
non-terminal rows.
-/

namespace Recovery

open ToolExecution

/-! ## Request lifecycle recovery -/

def requestRecoveryStale (row : RequestContext) : Prop :=
  row.state = .claimed ∨ row.state = .processing

instance (row : RequestContext) : Decidable (requestRecoveryStale row) := by
  unfold requestRecoveryStale
  infer_instance

def requestRecover (row : RequestContext) : RequestContext :=
  { row with state := .failed, admission := .released }

def requestRecoveryMeasure (row : RequestContext) : Nat :=
  if requestRecoveryStale row then 1 else 0

theorem requestRecovery_stale_positive :
    ∀ row, requestRecoveryStale row → requestRecoveryMeasure row > 0 := by
  intro row h_stale
  simp [requestRecoveryMeasure, h_stale]

theorem requestRecover_terminal :
    ∀ row, requestRecoveryStale row → isTerminal (requestRecover row).state := by
  intro row _h_stale
  simp [requestRecover, HasTerminal.isTerminal, RequestState.instHasTerminal]

theorem requestRecover_zero :
    ∀ row, requestRecoveryStale row → requestRecoveryMeasure (requestRecover row) = 0 := by
  intro row _h_stale
  have h_not : ¬ requestRecoveryStale (requestRecover row) := by
    intro h_stale
    cases h_stale with
    | inl h_claimed =>
        simp [requestRecover] at h_claimed
    | inr h_processing =>
        simp [requestRecover] at h_processing
  simp [requestRecoveryMeasure, h_not]

def requestRecoverySweep : RecoverySweep :=
  { Row := RequestContext
  , collection := .agentRequest
  , sweepId := "request_lifecycle_recover_all_requests"
  , rustFunction := "RequestLifecycle::recover_all"
  , cadence := .startup
  , implementationStatus := .implemented
  , stale := requestRecoveryStale
  , recover := requestRecover
  , terminal := fun row => isTerminal row.state
  , measure := requestRecoveryMeasure
  , h_stale_positive := requestRecovery_stale_positive
  , h_recover_terminal := requestRecover_terminal
  , h_recover_zero := requestRecover_zero
  }

/-! ## Streaming response recovery -/

abbrev ResponseRecoveryStatus := StreamingResponse.Status

namespace ResponseRecoveryStatus
  /-- Contract name (not the DefraDB persistence name). `toContract` and
  `StreamingResponse.Status.toDefraDB` serve different consumers: the
  contract uses Lean-variant names ("completed"), while the persistence
  field stringifies to "complete" (matching the Rust enum). -/
  def toContract : StreamingResponse.Status → String
    | .streaming => "streaming"
    | .completed => "completed"
    | .error => "error"
end ResponseRecoveryStatus

structure ResponseRecoveryRow where
  status : ResponseRecoveryStatus
  deriving Repr

def responseRecoveryStale (row : ResponseRecoveryRow) : Prop :=
  row.status = .streaming

instance (row : ResponseRecoveryRow) : Decidable (responseRecoveryStale row) := by
  unfold responseRecoveryStale
  infer_instance

def responseRecover (row : ResponseRecoveryRow) : ResponseRecoveryRow :=
  { row with status := .error }

def responseRecoveryMeasure (row : ResponseRecoveryRow) : Nat :=
  if responseRecoveryStale row then 1 else 0

theorem responseRecovery_stale_positive :
    ∀ row, responseRecoveryStale row → responseRecoveryMeasure row > 0 := by
  intro row h_stale
  simp [responseRecoveryMeasure, h_stale]

theorem responseRecover_terminal :
    ∀ row, responseRecoveryStale row → isTerminal (responseRecover row).status := by
  intro row _h_stale
  simp [responseRecover, HasTerminal.isTerminal, StreamingResponse.Status.instHasTerminal]

theorem responseRecover_zero :
    ∀ row, responseRecoveryStale row → responseRecoveryMeasure (responseRecover row) = 0 := by
  intro row _h_stale
  simp [responseRecover, responseRecoveryMeasure, responseRecoveryStale]

def responseRecoverySweep : RecoverySweep :=
  { Row := ResponseRecoveryRow
  , collection := .agentResponse
  , sweepId := "request_lifecycle_recover_all_streaming_responses"
  , rustFunction := "RequestLifecycle::recover_all"
  , cadence := .startup
  , implementationStatus := .implemented
  , stale := responseRecoveryStale
  , recover := responseRecover
  , terminal := fun row => isTerminal row.status
  , measure := responseRecoveryMeasure
  , h_stale_positive := responseRecovery_stale_positive
  , h_recover_terminal := responseRecover_terminal
  , h_recover_zero := responseRecover_zero
  }

/-! ## Tool-call recovery -/

def isDetachedBridgeCall (call : ToolCallContext) : Prop :=
  call.childRequestId.isSome ∧ call.cancelPolicy = .detach

instance (call : ToolCallContext) : Decidable (isDetachedBridgeCall call) := by
  unfold isDetachedBridgeCall
  infer_instance

inductive ToolRecoveryCause where
  | deadlineExceeded
  | parentInterrupted
  | parentTerminal
  | childCompleted
  | childFailed
  | childDead
  | childInterrupted
  | childSuperseded
  deriving DecidableEq, Repr

namespace ToolRecoveryCause

def toContract : ToolRecoveryCause → String
  | .deadlineExceeded => "deadlineExceeded"
  | .parentInterrupted => "parentInterrupted"
  | .parentTerminal => "parentTerminal"
  | .childCompleted => "childCompleted"
  | .childFailed => "childFailed"
  | .childDead => "childDead"
  | .childInterrupted => "childInterrupted"
  | .childSuperseded => "childSuperseded"

def terminalState : ToolRecoveryCause → ToolCallState
  | .deadlineExceeded => .timedOut
  | .parentInterrupted => .cancelled
  | .parentTerminal => .failed
  | .childCompleted => .completed
  | .childFailed => .failed
  | .childDead => .failed
  | .childInterrupted => .cancelled
  | .childSuperseded => .failed

theorem terminalState_terminal (cause : ToolRecoveryCause) :
    isTerminal cause.terminalState := by
  cases cause <;>
    simp [terminalState, HasTerminal.isTerminal, ToolCallState.instHasTerminal]

end ToolRecoveryCause

structure ToolCallRecoveryRow where
  call : ToolCallContext
  cause : ToolRecoveryCause
  deriving Repr

def toolCallRecoveryStale (row : ToolCallRecoveryRow) : Prop :=
  row.call.state = .running ∧ ¬ isDetachedBridgeCall row.call

instance (row : ToolCallRecoveryRow) : Decidable (toolCallRecoveryStale row) := by
  unfold toolCallRecoveryStale
  infer_instance

def toolCallRecover (row : ToolCallRecoveryRow) : ToolCallRecoveryRow :=
  { row with call := { row.call with state := row.cause.terminalState } }

def toolCallRecoveryMeasure (row : ToolCallRecoveryRow) : Nat :=
  if toolCallRecoveryStale row then 1 else 0

theorem toolCallRecovery_stale_positive :
    ∀ row, toolCallRecoveryStale row → toolCallRecoveryMeasure row > 0 := by
  intro row h_stale
  simp [toolCallRecoveryMeasure, h_stale]

theorem toolCallRecover_terminal :
    ∀ row, toolCallRecoveryStale row → isTerminal (toolCallRecover row).call.state := by
  intro row _h_stale
  rcases row with ⟨call, cause⟩
  cases cause <;>
    simp [toolCallRecover, ToolRecoveryCause.terminalState,
      HasTerminal.isTerminal, ToolCallState.instHasTerminal]

theorem toolCallRecover_zero :
    ∀ row, toolCallRecoveryStale row → toolCallRecoveryMeasure (toolCallRecover row) = 0 := by
  intro row _h_stale
  have h_terminal_not_running : row.cause.terminalState ≠ .running := by
    cases row.cause <;> simp [ToolRecoveryCause.terminalState]
  have h_not : ¬ toolCallRecoveryStale (toolCallRecover row) := by
    intro h_stale
    rcases h_stale with ⟨h_running, _h_not_detached⟩
    simp [toolCallRecover] at h_running
    exact h_terminal_not_running h_running
  simp [toolCallRecoveryMeasure, h_not]

def toolCallRecoverySweep : RecoverySweep :=
  { Row := ToolCallRecoveryRow
  , collection := .agentToolCall
  , sweepId := "tool_call_lifecycle_recover_all_running_calls"
  , rustFunction := "ToolCallLifecycle::recover_all"
  , cadence := .startup
  , implementationStatus := .implemented
  , stale := toolCallRecoveryStale
  , recover := toolCallRecover
  , terminal := fun row => isTerminal row.call.state
  , measure := toolCallRecoveryMeasure
  , h_stale_positive := toolCallRecovery_stale_positive
  , h_recover_terminal := toolCallRecover_terminal
  , h_recover_zero := toolCallRecover_zero
  }

/-! ## Detached bridge recovery obligation -/

inductive DetachedBridgeRecoveryCause where
  | deadlineExceeded
  | terminalParent
  | childCompleted
  | childFailed
  | childDead
  | childInterrupted
  | childSuperseded
  deriving DecidableEq, Repr

namespace DetachedBridgeRecoveryCause

def toContract : DetachedBridgeRecoveryCause → String
  | .deadlineExceeded => "deadlineExceeded"
  | .terminalParent => "terminalParent"
  | .childCompleted => "childCompleted"
  | .childFailed => "childFailed"
  | .childDead => "childDead"
  | .childInterrupted => "childInterrupted"
  | .childSuperseded => "childSuperseded"

def terminalState : DetachedBridgeRecoveryCause → ToolCallState
  | .deadlineExceeded => .timedOut
  | .terminalParent => .failed
  | .childCompleted => .completed
  | .childFailed => .failed
  | .childDead => .failed
  | .childInterrupted => .cancelled
  | .childSuperseded => .failed

theorem terminalState_terminal (cause : DetachedBridgeRecoveryCause) :
    isTerminal cause.terminalState := by
  cases cause <;>
    simp [terminalState, HasTerminal.isTerminal, ToolCallState.instHasTerminal]

end DetachedBridgeRecoveryCause

structure DetachedBridgeRecoveryRow where
  call : ToolCallContext
  cause : DetachedBridgeRecoveryCause
  deriving Repr

def detachedBridgeRecoveryStale (row : DetachedBridgeRecoveryRow) : Prop :=
  row.call.state = .running ∧ isDetachedBridgeCall row.call

instance (row : DetachedBridgeRecoveryRow) : Decidable (detachedBridgeRecoveryStale row) := by
  unfold detachedBridgeRecoveryStale
  infer_instance

def detachedBridgeRecover (row : DetachedBridgeRecoveryRow) : DetachedBridgeRecoveryRow :=
  { row with call := { row.call with state := row.cause.terminalState } }

def detachedBridgeRecoveryMeasure (row : DetachedBridgeRecoveryRow) : Nat :=
  if detachedBridgeRecoveryStale row then 1 else 0

theorem detachedBridgeRecovery_stale_positive :
    ∀ row, detachedBridgeRecoveryStale row → detachedBridgeRecoveryMeasure row > 0 := by
  intro row h_stale
  simp [detachedBridgeRecoveryMeasure, h_stale]

theorem detachedBridgeRecover_terminal :
    ∀ row, detachedBridgeRecoveryStale row → isTerminal (detachedBridgeRecover row).call.state := by
  intro row _h_stale
  rcases row with ⟨call, cause⟩
  cases cause <;>
    simp [detachedBridgeRecover, DetachedBridgeRecoveryCause.terminalState,
      HasTerminal.isTerminal, ToolCallState.instHasTerminal]

theorem detachedBridgeRecover_zero :
    ∀ row, detachedBridgeRecoveryStale row → detachedBridgeRecoveryMeasure (detachedBridgeRecover row) = 0 := by
  intro row _h_stale
  have h_terminal_not_running : row.cause.terminalState ≠ .running := by
    cases row.cause <;> simp [DetachedBridgeRecoveryCause.terminalState]
  have h_not : ¬ detachedBridgeRecoveryStale (detachedBridgeRecover row) := by
    intro h_stale
    rcases h_stale with ⟨h_running, _h_detached⟩
    simp [detachedBridgeRecover] at h_running
    exact h_terminal_not_running h_running
  simp [detachedBridgeRecoveryMeasure, h_not]

def detachedBridgeRecoverySweep : RecoverySweep :=
  { Row := DetachedBridgeRecoveryRow
  , collection := .agentToolCall
  , sweepId := "tool_call_lifecycle_recover_detached_bridge_rows"
  , rustFunction := "ToolCallLifecycle::recover_detached_bridge_rows"
  , cadence := .startup
  , implementationStatus := .obligation
  , stale := detachedBridgeRecoveryStale
  , recover := detachedBridgeRecover
  , terminal := fun row => isTerminal row.call.state
  , measure := detachedBridgeRecoveryMeasure
  , h_stale_positive := detachedBridgeRecovery_stale_positive
  , h_recover_terminal := detachedBridgeRecover_terminal
  , h_recover_zero := detachedBridgeRecover_zero
  }

/-! ## Inference-call recovery obligation -/

inductive InferenceRecoveryCause where
  | staleQueued
  | staleRunning
  | interruptedParent
  deriving DecidableEq, Repr

namespace InferenceRecoveryCause

def toContract : InferenceRecoveryCause → String
  | .staleQueued => "staleQueued"
  | .staleRunning => "staleRunning"
  | .interruptedParent => "interruptedParent"

end InferenceRecoveryCause

structure InferenceCallRecoveryRow where
  call : InferenceCall
  cause : InferenceRecoveryCause
  deriving Repr

def inferenceCallRecoveryStale (row : InferenceCallRecoveryRow) : Prop :=
  match row.cause with
  | .staleQueued => row.call.state = .queued
  | .staleRunning => row.call.state = .running
  | .interruptedParent => row.call.state = .queued ∨ row.call.state = .running

instance (row : InferenceCallRecoveryRow) : Decidable (inferenceCallRecoveryStale row) := by
  unfold inferenceCallRecoveryStale
  cases row.cause <;> infer_instance

def inferenceCallRecover (row : InferenceCallRecoveryRow) : InferenceCallRecoveryRow :=
  match row.cause with
  | .staleQueued => { row with call := { row.call with state := .cancelled } }
  | .staleRunning => { row with call := { row.call with state := .failed } }
  | .interruptedParent => { row with call := { row.call with state := .cancelled } }

def inferenceCallRecoveryMeasure (row : InferenceCallRecoveryRow) : Nat :=
  if inferenceCallRecoveryStale row then 1 else 0

theorem inferenceCallRecovery_stale_positive :
    ∀ row, inferenceCallRecoveryStale row → inferenceCallRecoveryMeasure row > 0 := by
  intro row h_stale
  simp [inferenceCallRecoveryMeasure, h_stale]

theorem inferenceCallRecover_terminal :
    ∀ row, inferenceCallRecoveryStale row → isTerminal (inferenceCallRecover row).call.state := by
  intro row _h_stale
  rcases row with ⟨call, cause⟩
  cases cause <;>
    simp [inferenceCallRecover, HasTerminal.isTerminal, InferenceCallState.instHasTerminal]

theorem inferenceCallRecover_zero :
    ∀ row, inferenceCallRecoveryStale row → inferenceCallRecoveryMeasure (inferenceCallRecover row) = 0 := by
  intro row _h_stale
  have h_not : ¬ inferenceCallRecoveryStale (inferenceCallRecover row) := by
    intro h_stale
    rcases row with ⟨call, cause⟩
    cases cause <;>
      simp [inferenceCallRecoveryStale, inferenceCallRecover] at h_stale
  simp [inferenceCallRecoveryMeasure, h_not]

theorem inferenceCallRecover_contributes_zero
    (row : InferenceCallRecoveryRow)
    (h_stale : inferenceCallRecoveryStale row)
    (bid : BackendId) :
    (inferenceCallRecover row).call.slotContribution bid = 0 :=
  InferenceCall.terminal_call_contributes_zero (inferenceCallRecover_terminal row h_stale)

def inferenceCallRecoverySweep : RecoverySweep :=
  { Row := InferenceCallRecoveryRow
  , collection := .inferenceCall
  , sweepId := "inference_call_recover_all_stale_calls"
  , rustFunction := "InferenceCall::recover_all"
  , cadence := .startup
  , implementationStatus := .obligation
  , stale := inferenceCallRecoveryStale
  , recover := inferenceCallRecover
  , terminal := fun row => isTerminal row.call.state
  , measure := inferenceCallRecoveryMeasure
  , h_stale_positive := inferenceCallRecovery_stale_positive
  , h_recover_terminal := inferenceCallRecover_terminal
  , h_recover_zero := inferenceCallRecover_zero
  }

def registeredRecoverySweeps : List RecoverySweep :=
  [ requestRecoverySweep
  , responseRecoverySweep
  , toolCallRecoverySweep
  , detachedBridgeRecoverySweep
  , inferenceCallRecoverySweep
  ]

def registeredRecoverySweepIds : List String :=
  registeredRecoverySweeps.map fun sweep => sweep.sweepId

def registeredRecoverySweepContracts : List (String × String) :=
  registeredRecoverySweeps.map fun sweep =>
    (sweep.sweepId, sweep.collection.toContract)

theorem registered_sweeps_cover_persisted_collections :
    ∀ collection,
      collection ∈ PersistedRecoveryCollection.all →
      ∃ sweep,
        sweep ∈ registeredRecoverySweeps ∧
        sweep.collection = collection := by
  intro collection _h_collection
  cases collection with
  | agentRequest =>
      exact ⟨requestRecoverySweep, by simp [registeredRecoverySweeps], rfl⟩
  | agentResponse =>
      exact ⟨responseRecoverySweep, by simp [registeredRecoverySweeps], rfl⟩
  | agentToolCall =>
      exact ⟨toolCallRecoverySweep, by simp [registeredRecoverySweeps], rfl⟩
  | inferenceCall =>
      exact ⟨inferenceCallRecoverySweep, by simp [registeredRecoverySweeps], rfl⟩

end Recovery
