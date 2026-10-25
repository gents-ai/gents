import Proofs.StorageFailure
import Proofs.Conformance.ContractTypes

namespace Conformance.StorageFailureContracts
open Conformance.Contracts

/-- Unknown codes and misleading messages must not acquire storage authority. -/
def inputs : List (String × String) :=
  [ ("STORAGE_FULL", "localized storage failure")
  , ("STORAGE_FULL", "please retry")
  , ("", "No space left on device (os error 28)")
  , ("OTHER", "STORAGE_FULL") ]

def caseJson (input : String × String) : String :=
  let before : StorageFailure.Submission := ⟨17, [3, 1, 4, 1, 5], false⟩
  let after := StorageFailure.observe input.1 before
  "{\"code\":" ++ jsonString input.1 ++
  ",\"message\":" ++ jsonString input.2 ++
  ",\"full\":" ++ toString (StorageFailure.isFull input.1) ++
  ",\"fatal_readiness\":" ++ toString (StorageFailure.fatalReadiness input.1) ++
  ",\"stop_automatic_retry\":" ++ toString (StorageFailure.stopAutomaticRetry input.1) ++
  ",\"outcome_unknown\":" ++ toString after.outcomeUnknown ++
  ",\"same_request\":" ++ toString (after.requestId == before.requestId) ++
  ",\"same_bytes\":" ++ toString (after.signedBytes == before.signedBytes) ++ "}"

def casesJson : String := jsonArray (inputs.map caseJson)
end Conformance.StorageFailureContracts
