import Proofs.Inference.Recovery.Contract
import Proofs.Session.Hooks.OwedDelivery

namespace Recovery

open Hooks.OwedDelivery

/-- One owed delivery as the periodic sweep sees it: the record, what the
redrive observed as the record's runner, how the plugin's staging went, and the
writes native ACP refused the runner. Decision 2026-11-11, maintainer-approved:
the sweep over pending `OwedDelivery` records is the loop over
`Hooks.Delivery.pending` that the delivery contract leaves to recovery. The same
sweep re-derives owed halts from core state (`Hooks.OwedDelivery.deriveHalts`)
and prunes settled records past the retention (`Hooks.OwedDelivery.prune`);
those actions keep no row of their own and are modelled there. -/
structure OwedDeliveryRow where
  record : Record
  observed : Observation
  attempt : Attempt
  refused : List Write
  deriving DecidableEq, Repr

def OwedDeliveryRow.mayWrite (row : OwedDeliveryRow) (write : Write) : Bool :=
  !row.refused.contains write

/-- The rows one redrive settles: an owed record whose subject is no longer the
runner's, whose transition is no longer current, or whose staging is performed.
An owed record whose staging fails stays owed for a later sweep; it is not
stale. -/
def owedDeliveryStale (row : OwedDeliveryRow) : Prop :=
  row.record.pending = true ∧
    (redrive row.record row.observed row.attempt row.mayWrite).pending = false

instance (row : OwedDeliveryRow) : Decidable (owedDeliveryStale row) := by
  unfold owedDeliveryStale
  infer_instance

def owedDeliveryRecover (row : OwedDeliveryRow) : OwedDeliveryRow :=
  { row with record := redrive row.record row.observed row.attempt row.mayWrite }

def owedDeliveryMeasure (row : OwedDeliveryRow) : Nat :=
  if owedDeliveryStale row then 1 else 0

theorem owedDelivery_stale_positive :
    ∀ row, owedDeliveryStale row → owedDeliveryMeasure row > 0 := by
  intro row stale
  simp [owedDeliveryMeasure, stale]

theorem owedDelivery_recover_settled :
    ∀ row, owedDeliveryStale row → (owedDeliveryRecover row).record.pending = false := by
  intro row stale
  exact stale.2

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
