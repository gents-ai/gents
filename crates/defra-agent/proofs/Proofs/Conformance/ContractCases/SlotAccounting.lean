import Proofs.Fleet
import Proofs.InferenceCall.SlotAccounting
import Proofs.Request
import Proofs.Conformance.ContractCases.Types

/-!
# Slot and Fleet Accounting Witness Cases
-/

namespace Conformance.ContractCases

def otherBackend : BackendId :=
  { val := "other-backend" }

def slotCall
    (callId : Nat)
    (backend : BackendId)
    (state : InferenceCallState) : InferenceCall :=
  { callId := callId
  , requestId := callId
  , backend := backend
  , state := state
  }

def inferenceRowsForBound : Nat → InferenceCall
  | 1 => slotCall 1 contractBackend .running
  | 2 => slotCall 2 contractBackend .queued
  | 3 => slotCall 3 contractBackend .completed
  | 4 => slotCall 4 otherBackend .running
  | n => slotCall n otherBackend .failed

def inferenceBoundedCallIds : Finset Nat :=
  {1, 2, 3, 4}

def inferenceBoundedRunningCount : Nat :=
  InferenceCall.reconstructedSlotCount
    inferenceBoundedCallIds
    inferenceRowsForBound
    contractBackend

def inferenceStateContributionCase
    (name : String)
    (state : InferenceCallState)
    (expected : Nat) : InferenceSlotAccountingCase :=
  let call := slotCall 1 contractBackend state
  let contribution := call.slotContribution contractBackend
  { name := name
  , property := "state_contribution"
  , backendId := contractBackend.val
  , preState := state.toDefraDB
  , postState := state.toDefraDB
  , contribution := contribution
  , expectedContribution := expected
  , preContribution := contribution
  , postContribution := contribution
  , releasedSlot := false
  , permitDropTerminalization := false
  , rowStates := [state.toDefraDB]
  , rowBackendIds := [contractBackend.val]
  , reconstructedRunningCount := contribution
  , maxConcurrent := 1
  , boundedByMaxConcurrent := decide (contribution ≤ 1)
  }

def inferenceReleaseCase
    (name : String)
    (terminal : InferenceCallState) : InferenceSlotAccountingCase :=
  let pre := slotCall 1 contractBackend .running
  let post := slotCall 1 contractBackend terminal
  let preContribution := pre.slotContribution contractBackend
  let postContribution := post.slotContribution contractBackend
  { name := name
  , property := "terminal_release"
  , backendId := contractBackend.val
  , preState := InferenceCallState.running.toDefraDB
  , postState := terminal.toDefraDB
  , contribution := postContribution
  , expectedContribution := 0
  , preContribution := preContribution
  , postContribution := postContribution
  , releasedSlot := decide (preContribution = 1 ∧ postContribution = 0)
  , permitDropTerminalization := false
  , rowStates := [terminal.toDefraDB]
  , rowBackendIds := [contractBackend.val]
  , reconstructedRunningCount := postContribution
  , maxConcurrent := 1
  , boundedByMaxConcurrent := decide (postContribution ≤ 1)
  }

def inferencePermitDropCase
    (name : String)
    (terminal : InferenceCallState) : InferenceSlotAccountingCase :=
  let pre := slotCall 1 contractBackend .running
  let post := slotCall 1 contractBackend terminal
  let preContribution := pre.slotContribution contractBackend
  let postContribution := post.slotContribution contractBackend
  { name := name
  , property := "permit_drop_terminalization"
  , backendId := contractBackend.val
  , preState := InferenceCallState.running.toDefraDB
  , postState := terminal.toDefraDB
  , contribution := postContribution
  , expectedContribution := 0
  , preContribution := preContribution
  , postContribution := postContribution
  , releasedSlot := decide (preContribution = 1 ∧ postContribution = 0)
  , permitDropTerminalization := true
  , rowStates := [terminal.toDefraDB]
  , rowBackendIds := [contractBackend.val]
  , reconstructedRunningCount := postContribution
  , maxConcurrent := 1
  , boundedByMaxConcurrent := decide (postContribution ≤ 1)
  }

def inferenceBoundedCase : InferenceSlotAccountingCase :=
  { name := "reconstructed_running_count_bounded_by_max_concurrent"
  , property := "reconstructed_running_bound"
  , backendId := contractBackend.val
  , preState := ""
  , postState := ""
  , contribution := inferenceBoundedRunningCount
  , expectedContribution := 1
  , preContribution := 0
  , postContribution := 0
  , releasedSlot := false
  , permitDropTerminalization := false
  , rowStates :=
      [ InferenceCallState.running.toDefraDB
      , InferenceCallState.queued.toDefraDB
      , InferenceCallState.completed.toDefraDB
      , InferenceCallState.running.toDefraDB
      ]
  , rowBackendIds :=
      [ contractBackend.val
      , contractBackend.val
      , contractBackend.val
      , otherBackend.val
      ]
  , reconstructedRunningCount := inferenceBoundedRunningCount
  , maxConcurrent := 1
  , boundedByMaxConcurrent := decide (inferenceBoundedRunningCount ≤ 1)
  }

def inferenceSlotAccountingCases : List InferenceSlotAccountingCase :=
  [ inferenceStateContributionCase "queued_contributes_zero" .queued 0
  , inferenceStateContributionCase "running_contributes_one" .running 1
  , inferenceStateContributionCase "cancelled_terminal_contributes_zero" .cancelled 0
  , inferenceStateContributionCase "completed_terminal_contributes_zero" .completed 0
  , inferenceStateContributionCase "failed_terminal_contributes_zero" .failed 0
  , inferenceReleaseCase "cancelled_releases_slot" .cancelled
  , inferenceReleaseCase "completed_releases_slot" .completed
  , inferenceReleaseCase "failed_releases_slot" .failed
  , inferencePermitDropCase "permit_drop_failed_terminalization_not_counted" .failed
  , inferencePermitDropCase "permit_drop_cancelled_terminalization_not_counted" .cancelled
  , inferenceBoundedCase
  ]

def slotContext
    (state : RequestState)
    (admission : AdmissionState)
    (backend : BackendId := contractBackend) : RequestContext :=
  { state := state
  , origin := .interactive
  , backend := backend
  , admission := admission
  , deadline := 10
  , claimTime := 0
  , currentTime := 0
  , retryCount := 0
  , maxRetries := 3
  , progressSeq := 0
  , messageSeq := 0
  , isLatest := true
  , persistence := .uncommitted
  }

def fleetRowsForBound : Nat → RequestContext
  | 1 => slotContext .claimed .acquired contractBackend
  | 2 => slotContext .processing .executing contractBackend
  | 3 => slotContext .claimed .waiting contractBackend
  | 4 => slotContext .completed .released contractBackend
  | _ => slotContext .processing .executing otherBackend

def fleetBoundedState : FleetState :=
  { activeIds := {1, 2, 3, 4}
  , ctx := fleetRowsForBound
  , scheduler :=
      { running := fun bid => if bid = contractBackend then 2 else 0
      , backends := fun bid =>
          if bid = contractBackend then
            { max_concurrent := 2, available := true }
          else
            { max_concurrent := 1, available := true }
      }
  }

def fleetAdmissionProjectionState : AdmissionState → InferenceCallState
  | .released => .completed
  | .waiting => .queued
  | .acquired => .running
  | .executing => .running

def fleetSlotContributionCase
    (name : String)
    (state : RequestState)
    (admission : AdmissionState)
    (expected : Nat) : FleetSlotAccountingCase :=
  let ctx := slotContext state admission contractBackend
  let contribution := FleetState.slotContribution ctx contractBackend
  let projectedState := fleetAdmissionProjectionState admission
  let reconstructed :=
    (slotCall 1 contractBackend projectedState).slotContribution contractBackend
  { name := name
  , property := "admission_contribution"
  , backendId := contractBackend.val
  , requestState := state.toDefraDB
  , admissionState := admissionName admission
  , contribution := contribution
  , expectedContribution := expected
  , activeCount := 1
  , schedulerRunning := contribution
  , slotCount := contribution
  , rowStates := [projectedState.toDefraDB]
  , rowBackendIds := [contractBackend.val]
  , reconstructedRunningCount := reconstructed
  , maxConcurrent := 1
  , boundedByMaxConcurrent := decide (contribution ≤ 1)
  , aggregateReconstructedNotPersisted := true
  }

def fleetBoundedCase : FleetSlotAccountingCase :=
  let slotCount := fleetBoundedState.slotCountFor contractBackend
  let schedulerRunning := fleetBoundedState.scheduler.running contractBackend
  let maxConcurrent := (fleetBoundedState.scheduler.backends contractBackend).max_concurrent
  let projectedRows : Nat → InferenceCall :=
    fun
      | 1 => slotCall 1 contractBackend .running
      | 2 => slotCall 2 contractBackend .running
      | 3 => slotCall 3 contractBackend .queued
      | 4 => slotCall 4 contractBackend .completed
      | n => slotCall n otherBackend .failed
  let reconstructed :=
    InferenceCall.reconstructedSlotCount fleetBoundedState.activeIds projectedRows contractBackend
  let rowStates :=
    [ InferenceCallState.running.toDefraDB
    , InferenceCallState.running.toDefraDB
    , InferenceCallState.queued.toDefraDB
    , InferenceCallState.completed.toDefraDB
    ]
  let rowBackendIds :=
    [ contractBackend.val
    , contractBackend.val
    , contractBackend.val
    , contractBackend.val
    ]
  { name := "fleet_reconstructed_running_count_bounded_by_max_concurrent"
  , property := "fleet_reconstructed_running_bound"
  , backendId := contractBackend.val
  , requestState := ""
  , admissionState := ""
  , contribution := slotCount
  , expectedContribution := 2
  , activeCount := fleetBoundedState.activeIds.card
  , schedulerRunning := schedulerRunning
  , slotCount := slotCount
  , rowStates := rowStates
  , rowBackendIds := rowBackendIds
  , reconstructedRunningCount := reconstructed
  , maxConcurrent := maxConcurrent
  , boundedByMaxConcurrent := decide (slotCount ≤ maxConcurrent)
  , aggregateReconstructedNotPersisted := true
  }

def fleetSlotAccountingCases : List FleetSlotAccountingCase :=
  [ fleetSlotContributionCase "fleet_waiting_contributes_zero" .claimed .waiting 0
  , fleetSlotContributionCase "fleet_acquired_contributes_one" .claimed .acquired 1
  , fleetSlotContributionCase "fleet_executing_contributes_one" .processing .executing 1
  , fleetSlotContributionCase "fleet_released_terminal_contributes_zero" .completed .released 0
  , fleetBoundedCase
  ]

end Conformance.ContractCases
