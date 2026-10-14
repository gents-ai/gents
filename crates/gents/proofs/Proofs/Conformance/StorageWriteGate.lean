import Proofs.StorageWriteGate
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.StorageWriteGateContracts
open StorageWriteGate Conformance.Contracts

structure Case where
  name : String
  independentlyScheduled : Bool
  storageReturns : Bool
  rollbackOwnerScheduled : Bool := false
  cleanupTimesOut : Bool := false

def cases : List Case :=
  [ ⟨"shared_task_completion_suspended", false, true, false, false⟩
  , ⟨"independent_completion", true, true, false, false⟩
  , ⟨"elapsed_not_completion", true, false, false, false⟩
  , ⟨"cancelled_rollback_timeout_releases_canonical_gate", false, false, true, true⟩ ]

def finalState (c : Case) : State :=
  if c.rollbackOwnerScheduled then
    run ⟨.storage, c.independentlyScheduled, true⟩
      (if c.cleanupTimesOut then
        [.rollbackOwnerScheduled, .cleanupDeadlineObserved, .dropReturned, .release]
       else [.rollbackOwnerScheduled, .cleanupReturned, .release])
  else
    run ⟨.storage, c.independentlyScheduled, false⟩
      (if c.storageReturns then [.siblingWait, .storageReturned, .release]
       else [.siblingWait, .elapsed])

def gateHeldAfterOwnerHandoff (c : Case) : Bool :=
  held (if c.rollbackOwnerScheduled then
    step ⟨.storage, c.independentlyScheduled, true⟩ .rollbackOwnerScheduled
  else ⟨.storage, c.independentlyScheduled, false⟩)

private def jsonBool (b : Bool) : String := if b then "true" else "false"

def caseJson (c : Case) : String :=
  "{\"name\":" ++ jsonString c.name ++
  ",\"independently_scheduled\":" ++ jsonBool c.independentlyScheduled ++
  ",\"storage_returns\":" ++ jsonBool c.storageReturns ++
  ",\"rollback_owner_scheduled\":" ++ jsonBool c.rollbackOwnerScheduled ++
  ",\"cleanup_times_out\":" ++ jsonBool c.cleanupTimesOut ++
  ",\"expected_gate_held_after_owner_handoff\":" ++
    jsonBool (gateHeldAfterOwnerHandoff c) ++
  ",\"expected_gate_held\":" ++ jsonBool (held (finalState c)) ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

end Conformance.StorageWriteGateContracts

/-- Standalone narrow fixture; regeneration must not change unrelated contracts. -/
def main : IO Unit := IO.println Conformance.StorageWriteGateContracts.casesJson
