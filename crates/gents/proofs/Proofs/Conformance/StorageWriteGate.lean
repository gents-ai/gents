import Proofs.StorageWriteGate
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.StorageWriteGateContracts
open StorageWriteGate Conformance.Contracts

structure Case where
  name : String
  independentlyScheduled : Bool
  storageReturns : Bool

def cases : List Case :=
  [ ⟨"shared_task_completion_suspended", false, true⟩
  , ⟨"independent_completion", true, true⟩
  , ⟨"elapsed_not_completion", true, false⟩ ]

def finalState (c : Case) : State :=
  run ⟨.storage, c.independentlyScheduled, false⟩
    (if c.storageReturns then [.siblingWait, .storageReturned, .release]
     else [.siblingWait, .elapsed])

private def jsonBool (b : Bool) : String := if b then "true" else "false"

def caseJson (c : Case) : String :=
  "{\"name\":" ++ jsonString c.name ++
  ",\"independently_scheduled\":" ++ jsonBool c.independentlyScheduled ++
  ",\"storage_returns\":" ++ jsonBool c.storageReturns ++
  ",\"expected_gate_held\":" ++ jsonBool (held (finalState c)) ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

end Conformance.StorageWriteGateContracts

/-- Standalone narrow fixture; regeneration must not change unrelated contracts. -/
def main : IO Unit := IO.println Conformance.StorageWriteGateContracts.casesJson
