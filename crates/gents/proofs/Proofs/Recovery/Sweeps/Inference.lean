import Proofs.Recovery.Contract
import Proofs.InferenceCall

namespace Recovery

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

def inferenceCallUninterruptedTerminalize
    (row : InferenceCallRecoveryRow) : InferenceCallRecoveryRow :=
  match row.cause with
  | .staleQueued => { row with call := { row.call with state := .cancelled } }
  | .staleRunning => { row with call := { row.call with state := .failed } }
  | .interruptedParent => { row with call := { row.call with state := .cancelled } }

def inferenceCallRecoveryMeasure (row : InferenceCallRecoveryRow) : Nat :=
  if inferenceCallRecoveryStale row then 1 else 0

theorem inferenceCallRecover_matches_uninterrupted :
    ∀ row, inferenceCallRecoveryStale row →
      inferenceCallRecover row = inferenceCallUninterruptedTerminalize row := by
  intro row _h_stale
  rcases row with ⟨call, cause⟩
  cases cause <;>
    simp [inferenceCallRecover, inferenceCallUninterruptedTerminalize]

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
  , implementationStatus := .implemented
  , stale := inferenceCallRecoveryStale
  , recover := inferenceCallRecover
  , terminal := fun row => isTerminal row.call.state
  , measure := inferenceCallRecoveryMeasure
  , h_stale_positive := inferenceCallRecovery_stale_positive
  , h_recover_terminal := inferenceCallRecover_terminal
  , h_recover_zero := inferenceCallRecover_zero
  }

def inferenceCallRecoveryEquivalence :
    RecoveryEquivalence inferenceCallRecoverySweep :=
  { uninterrupted := inferenceCallUninterruptedTerminalize
  , h_recover_eq_uninterrupted := inferenceCallRecover_matches_uninterrupted
  }

end Recovery
