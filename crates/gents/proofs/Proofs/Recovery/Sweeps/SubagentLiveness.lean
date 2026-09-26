import Proofs.Recovery.Contract
import Proofs.Request.State

namespace Recovery

structure ExpiredChildRow where
  state : RequestState
  deadlineExpired : Bool
  deriving Repr

def expiredChildStale (row : ExpiredChildRow) : Prop :=
  (row.state = .claimed ∨ row.state = .processing) ∧ row.deadlineExpired = true

instance (row : ExpiredChildRow) : Decidable (expiredChildStale row) := by
  unfold expiredChildStale
  infer_instance

def expiredChildRecover (row : ExpiredChildRow) : ExpiredChildRow :=
  { row with state := .dead }

def expiredChildMeasure (row : ExpiredChildRow) : Nat :=
  if expiredChildStale row then 1 else 0

theorem expiredChild_stale_positive :
    ∀ row, expiredChildStale row → expiredChildMeasure row > 0 := by
  intro row h_stale
  simp [expiredChildMeasure, h_stale]

theorem expiredChildRecover_terminal :
    ∀ row, expiredChildStale row → isTerminal (expiredChildRecover row).state := by
  intro row _h_stale
  simp [expiredChildRecover, HasTerminal.isTerminal, RequestState.instHasTerminal]

theorem expiredChildRecover_zero :
    ∀ row, expiredChildStale row →
      expiredChildMeasure (expiredChildRecover row) = 0 := by
  intro row _h_stale
  have h_not : ¬ expiredChildStale (expiredChildRecover row) := by
    intro h_stale
    rcases h_stale with ⟨h_state, _⟩
    cases h_state with
    | inl h_claimed => simp [expiredChildRecover] at h_claimed
    | inr h_processing => simp [expiredChildRecover] at h_processing
  simp [expiredChildMeasure, h_not]

def expiredSubagentChildSweep : RecoverySweep :=
  { Row := ExpiredChildRow
  , collection := .agentRequest
  , sweepId := "subagent_liveness_terminalize_expired_children"
  , rustFunction := "ToolCallLifecycle::reconcile_subagent_liveness"
  , cadence := .periodic
  , implementationStatus := .implemented
  , stale := expiredChildStale
  , recover := expiredChildRecover
  , terminal := fun row => isTerminal row.state
  , measure := expiredChildMeasure
  , h_stale_positive := expiredChild_stale_positive
  , h_recover_terminal := expiredChildRecover_terminal
  , h_recover_zero := expiredChildRecover_zero
  }

/- There is no queued-descendant sweep: a parent's terminal state — completed,
   interrupted, failed, dead or superseded — never releases its queued
   subagents. Only an explicit bridge cancellation reaches a child. -/

end Recovery
