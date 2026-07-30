import Proofs.Recovery.Contract
import Proofs.Properties.Liveness
import Proofs.StreamingResponse.State

namespace Recovery

inductive DurableRequestOutcome where
  | absent
  | completed
  | failed
  | interrupted
  deriving DecidableEq, Repr

structure RequestRecoveryRow where
  request : RequestContext
  durableOutcome : DurableRequestOutcome
  deriving Repr

def requestRecoveryStale (row : RequestRecoveryRow) : Prop :=
  (row.request.state = .claimed ∨ row.request.state = .processing) ∧
    row.durableOutcome ≠ .absent

instance (row : RequestRecoveryRow) : Decidable (requestRecoveryStale row) := by
  unfold requestRecoveryStale
  infer_instance

def recoveredRequestState : DurableRequestOutcome → RequestState
  | .completed => .completed
  | .failed => .failed
  | .interrupted => .interrupted
  | .absent => .failed

def requestRecover (row : RequestRecoveryRow) : RequestRecoveryRow :=
  { row with
      request :=
        { row.request with
            state := recoveredRequestState row.durableOutcome
            admission := .released } }

def requestUninterruptedTerminalize (row : RequestRecoveryRow) : RequestRecoveryRow :=
  { row with
      request :=
        { row.request with
            state := recoveredRequestState row.durableOutcome
            admission := .released } }

def requestRecoveryMeasure (row : RequestRecoveryRow) : Nat :=
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
    ∀ row, requestRecoveryStale row → isTerminal (requestRecover row).request.state := by
  intro row h_stale
  rcases h_stale with ⟨_h_active, h_outcome⟩
  cases h_outcome_value : row.durableOutcome with
  | absent =>
      exact False.elim (h_outcome h_outcome_value)
  | completed =>
      simp [requestRecover, recoveredRequestState, h_outcome_value,
        HasTerminal.isTerminal, RequestState.instHasTerminal]
  | failed =>
      simp [requestRecover, recoveredRequestState, h_outcome_value,
        HasTerminal.isTerminal, RequestState.instHasTerminal]
  | interrupted =>
      simp [requestRecover, recoveredRequestState, h_outcome_value,
        HasTerminal.isTerminal, RequestState.instHasTerminal]

theorem requestRecover_zero :
    ∀ row, requestRecoveryStale row → requestRecoveryMeasure (requestRecover row) = 0 := by
  intro row _h_stale
  have h_not : ¬ requestRecoveryStale (requestRecover row) := by
    intro h_stale
    rcases h_stale with ⟨h_active, _h_outcome⟩
    cases h_active with
    | inl h_claimed =>
        cases h_outcome_value : row.durableOutcome <;>
          simp [requestRecover, recoveredRequestState, h_outcome_value] at h_claimed
    | inr h_processing =>
        cases h_outcome_value : row.durableOutcome <;>
          simp [requestRecover, recoveredRequestState, h_outcome_value] at h_processing
  simp [requestRecoveryMeasure, h_not]

def requestRecoverySweep : RecoverySweep :=
  { Row := RequestRecoveryRow
  , collection := .agentRequest
  , sweepId := "request_lifecycle_recover_all_requests"
  , rustFunction := "RequestLifecycle::repair_terminal_requests"
  , cadence := .periodic
  , implementationStatus := .implemented
  , stale := requestRecoveryStale
  , recover := requestRecover
  , terminal := fun row => isTerminal row.request.state
  , measure := requestRecoveryMeasure
  , h_stale_positive := requestRecovery_stale_positive
  , h_recover_terminal := requestRecover_terminal
  , h_recover_zero := requestRecover_zero
  }

def requestRecoveryEquivalence : RecoveryEquivalence requestRecoverySweep :=
  { uninterrupted := requestUninterruptedTerminalize
  , h_recover_eq_uninterrupted := requestRecover_matches_uninterrupted
  }

abbrev ResponseRecoveryStatus := StreamingResponse.Status

namespace ResponseRecoveryStatus
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
