import Proofs.Conformance.ContractCases.SlotAccounting
import Proofs.Conformance.Contracts.Machines.Catalog
import Proofs.Conformance.Functions.Session
import Proofs.Conformance.Family

namespace Conformance.FleetSlotAccounting

open Lean Conformance.ContractCases

deriving instance ToJson for AdmissionState

structure ContributionInput where
  property : String
  backendId : String
  requestState : RequestState
  admissionState : AdmissionState
  /-- A bound: the capacity the reconstructed count is compared with. -/
  maxConcurrent : Nat
  deriving ToJson

structure SlotObservation where
  contribution : Nat
  rowStates : List String
  rowBackendIds : List String
  reconstructedRunningCount : Nat
  slotCount : Nat
  boundedByMaxConcurrent : Bool
  deriving ToJson

/-- A claimed request on one admission phase, observed through its persisted
InferenceCall row. -/
@[conformance_family]
def contribution : Family ContributionInput SlotObservation where
  inputs :=
    [(RequestState.claimed, AdmissionState.waiting), (.claimed, .acquired),
      (.processing, .executing), (.completed, .released)].map fun (requestState, admissionState) =>
      { property := "admission_contribution", backendId := contractBackend.val, requestState,
        admissionState, maxConcurrent := 1 }
  model := fun i =>
    let witness := fleetSlotContributionCase "" i.requestState i.admissionState 0
    { contribution := witness.contribution
      rowStates := witness.rowStates
      rowBackendIds := witness.rowBackendIds
      reconstructedRunningCount := witness.reconstructedRunningCount
      slotCount := witness.slotCount
      boundedByMaxConcurrent := decide (witness.slotCount ≤ i.maxConcurrent) }

structure BoundInput where
  property : String
  backendId : String
  activeCount : Nat
  /-- A bound: the backend's configured capacity. -/
  maxConcurrent : Nat
  deriving ToJson

/-- Two running calls, one queued and one completed on a backend of capacity
two, observed through their persisted InferenceCall rows. -/
@[conformance_family]
def bound : Family BoundInput SlotObservation where
  inputs :=
    [{ property := "fleet_reconstructed_running_bound"
       backendId := contractBackend.val
       activeCount := fleetBoundedState.activeIds.card
       maxConcurrent := (fleetBoundedState.scheduler.backends contractBackend).max_concurrent }]
  model := fun i =>
    { contribution := fleetBoundedCase.contribution
      rowStates := fleetBoundedCase.rowStates
      rowBackendIds := fleetBoundedCase.rowBackendIds
      reconstructedRunningCount := fleetBoundedCase.reconstructedRunningCount
      slotCount := fleetBoundedCase.slotCount
      boundedByMaxConcurrent := decide (fleetBoundedCase.slotCount ≤ i.maxConcurrent) }

example : fleetSlotAccountingCases.all fun c =>
    c.reconstructedRunningCount == c.slotCount && c.contribution == c.expectedContribution := by
  native_decide

example : fleetReleasedProjectionState .completed = .completed ∧
    fleetReleasedProjectionState .failed = .failed ∧
    fleetReleasedProjectionState .interrupted = .cancelled ∧
    fleetReleasedProjectionState .superseded = .cancelled ∧
    fleetReleasedProjectionState .dead = .cancelled := by
  decide

end Conformance.FleetSlotAccounting
