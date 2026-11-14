import Proofs.Conformance.ContractCases.SlotAccounting
import Proofs.Conformance.Family

namespace Conformance.InferenceSlotAccounting

open Lean Conformance.ContractCases

def callStateOf (name : String) : Option InferenceCallState :=
  [InferenceCallState.queued, .running, .cancelled, .completed, .failed].find? (·.toDefraDB == name)

def rowContribution (backend rowBackend state : String) : Nat :=
  match callStateOf state with
  | some state => (slotCall 0 { val := rowBackend } state).slotContribution { val := backend }
  | none => 0

def reconstructed (backend : String) (rowBackends rowStates : List String) : Nat :=
  ((rowBackends.zip rowStates).map fun (rowBackend, state) => rowContribution backend rowBackend state).sum

structure ReconstructionInput where
  property : String
  backendId : String
  preState : String
  postState : String
  permitDropTerminalization : Bool
  rowStates : List String
  rowBackendIds : List String
  /-- A bound: the capacity the reconstructed count is compared with. -/
  maxConcurrent : Nat
  deriving ToJson

structure ReconstructionOutput where
  contribution : Nat
  preContribution : Nat
  postContribution : Nat
  releasedSlot : Bool
  reconstructedRunningCount : Nat
  boundedByMaxConcurrent : Bool
  deriving ToJson

@[conformance_family]
def reconstruction : Family ReconstructionInput ReconstructionOutput where
  inputs := inferenceSlotAccountingCases.map fun c =>
    { property := c.property, backendId := c.backendId, preState := c.preState
      postState := c.postState, permitDropTerminalization := c.permitDropTerminalization
      rowStates := c.rowStates, rowBackendIds := c.rowBackendIds, maxConcurrent := c.maxConcurrent }
  model := fun i =>
    let count := reconstructed i.backendId i.rowBackendIds i.rowStates
    let pre := rowContribution i.backendId i.backendId i.preState
    let post := rowContribution i.backendId i.backendId i.postState
    { contribution := count
      preContribution := pre
      postContribution := post
      releasedSlot := pre == 1 && post == 0
      reconstructedRunningCount := count
      boundedByMaxConcurrent := decide (count ≤ i.maxConcurrent) }

example : inferenceSlotAccountingCases.all fun c =>
    let o := reconstruction.model
      { property := c.property, backendId := c.backendId, preState := c.preState
        postState := c.postState, permitDropTerminalization := c.permitDropTerminalization
        rowStates := c.rowStates, rowBackendIds := c.rowBackendIds, maxConcurrent := c.maxConcurrent }
    o.contribution == c.expectedContribution && o.contribution == c.contribution
      && o.preContribution == c.preContribution && o.postContribution == c.postContribution
      && o.releasedSlot == c.releasedSlot
      && o.reconstructedRunningCount == c.reconstructedRunningCount
      && o.boundedByMaxConcurrent == c.boundedByMaxConcurrent := by
  native_decide

example : inferenceSlotAccountingCases.all fun c =>
    (c.property != "terminal_release" && c.property != "permit_drop_terminalization")
      || (c.preState == "running" && c.preContribution == 1 && c.postContribution == 0
        && c.releasedSlot) := by
  native_decide

example : inferenceSlotAccountingCases.all fun c =>
    c.property != "permit_drop_terminalization" || c.permitDropTerminalization := by
  native_decide

end Conformance.InferenceSlotAccounting
