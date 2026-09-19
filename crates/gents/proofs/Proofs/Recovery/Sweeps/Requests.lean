import Proofs.Recovery.Contract
import Proofs.Properties.Liveness

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

def requestRecoveryMeasure (row : RequestRecoveryRow) : Nat :=
  if requestRecoveryStale row then 1 else 0

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

end Recovery
