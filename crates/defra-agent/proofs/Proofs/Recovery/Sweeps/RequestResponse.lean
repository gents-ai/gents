import Proofs.Recovery.Contract
import Proofs.Properties.Liveness
import Proofs.StreamingResponse.State

/-! Request and streaming response startup-recovery sweep contracts. -/

namespace Recovery

/-! ## Request lifecycle recovery -/

def requestRecoveryStale (row : RequestContext) : Prop :=
  row.state = .claimed ∨ row.state = .processing

instance (row : RequestContext) : Decidable (requestRecoveryStale row) := by
  unfold requestRecoveryStale
  infer_instance

def requestRecover (row : RequestContext) : RequestContext :=
  { row with state := .failed, admission := .released }

def requestUninterruptedTerminalize (row : RequestContext) : RequestContext :=
  { row with state := .failed, admission := .released }

def requestRecoveryMeasure (row : RequestContext) : Nat :=
  if requestRecoveryStale row then 1 else 0

theorem requestRecover_matches_uninterrupted :
    ∀ row, requestRecoveryStale row →
      requestRecover row = requestUninterruptedTerminalize row := by
  intro row _h_stale
  simp [requestRecover, requestUninterruptedTerminalize]

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

def requestRecoveryEquivalence : RecoveryEquivalence requestRecoverySweep :=
  { uninterrupted := requestUninterruptedTerminalize
  , h_recover_eq_uninterrupted := requestRecover_matches_uninterrupted
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

def responseUninterruptedTerminalize (row : ResponseRecoveryRow) : ResponseRecoveryRow :=
  { row with status := .error }

def responseRecoveryMeasure (row : ResponseRecoveryRow) : Nat :=
  if responseRecoveryStale row then 1 else 0

theorem responseRecover_matches_uninterrupted :
    ∀ row, responseRecoveryStale row →
      responseRecover row = responseUninterruptedTerminalize row := by
  intro row _h_stale
  simp [responseRecover, responseUninterruptedTerminalize]

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

def responseRecoveryEquivalence : RecoveryEquivalence responseRecoverySweep :=
  { uninterrupted := responseUninterruptedTerminalize
  , h_recover_eq_uninterrupted := responseRecover_matches_uninterrupted
  }

end Recovery
