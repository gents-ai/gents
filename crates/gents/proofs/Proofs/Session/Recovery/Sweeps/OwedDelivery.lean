import Proofs.Inference.Recovery.Contract
import Proofs.Session.Hooks.OwedDelivery

namespace Recovery

open Hooks.OwedDelivery

/-- One owed delivery as the periodic sweep sees it: the record, what the
redrive observed as the recorded actor, and how the plugin's staging went.
Decision 2026-11-11, maintainer-approved: the sweep over pending
`OwedDelivery` records is the loop over `Hooks.Delivery.pending` that the
delivery contract leaves to recovery. -/
structure OwedDeliveryRow where
  record : Record
  observed : Observation
  attempt : Attempt
  deriving DecidableEq, Repr

/-- The rows one redrive settles: an owed record whose actor lost its
authority, whose transition is no longer current, or whose staging commits.
An owed record whose staging fails stays owed for a later sweep; it is not
stale. -/
def owedDeliveryStale (row : OwedDeliveryRow) : Prop :=
  row.record.pending = true ∧
    (row.observed.authorized = false ∨ row.observed.current = false ∨
      row.attempt = .applied)

instance (row : OwedDeliveryRow) : Decidable (owedDeliveryStale row) := by
  unfold owedDeliveryStale
  infer_instance

def owedDeliveryRecover (row : OwedDeliveryRow) : OwedDeliveryRow :=
  { row with record := redrive row.record row.observed row.attempt }

def owedDeliveryMeasure (row : OwedDeliveryRow) : Nat :=
  if owedDeliveryStale row then 1 else 0

theorem owedDelivery_stale_positive :
    ∀ row, owedDeliveryStale row → owedDeliveryMeasure row > 0 := by
  intro row stale
  simp [owedDeliveryMeasure, stale]

theorem owedDelivery_recover_settled :
    ∀ row, owedDeliveryStale row → (owedDeliveryRecover row).record.pending = false := by
  intro row ⟨pending, cause⟩
  have unsettled : row.record.settled = none := by
    simpa [Record.pending, Option.isNone_iff_eq_none] using pending
  rcases cause with revoked | stale | applied
  · simp [owedDeliveryRecover, redrive, unsettled, revoked, Record.pending]
  · simp [owedDeliveryRecover, redrive, unsettled, stale, Record.pending]
  · cases authorized : row.observed.authorized <;> cases current : row.observed.current <;>
      simp [owedDeliveryRecover, redrive, unsettled, authorized, current, applied,
        Record.pending]

theorem owedDelivery_recover_zero :
    ∀ row, owedDeliveryStale row → owedDeliveryMeasure (owedDeliveryRecover row) = 0 := by
  intro row stale
  have settled := owedDelivery_recover_settled row stale
  have notStale : ¬ owedDeliveryStale (owedDeliveryRecover row) := by
    intro again
    rw [again.1] at settled
    exact Bool.noConfusion settled
  simp [owedDeliveryMeasure, notStale]

def owedDeliveryRecoverySweep : RecoverySweep :=
  { Row := OwedDeliveryRow
  , collection := .owedDelivery
  , sweepId := "lifecycle_hook_redrive_owed_deliveries"
  , rustFunction := "hooks::owed::redrive_pending"
  , cadence := .periodic
  , implementationStatus := .implemented
  , stale := owedDeliveryStale
  , recover := owedDeliveryRecover
  , terminal := fun row => row.record.pending = false
  , measure := owedDeliveryMeasure
  , h_stale_positive := owedDelivery_stale_positive
  , h_recover_terminal := owedDelivery_recover_settled
  , h_recover_zero := owedDelivery_recover_zero
  }

end Recovery
