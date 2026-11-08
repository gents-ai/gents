import Proofs.Client.Types
import Proofs.Conformance.ContractTypes

namespace Conformance.ClientSessionDelivery

open Conformance.Contracts

def sessionRows : List SessionRowObservation :=
  [.absent, .canonical, .duplicate, .outOfScope]

def sessionRowJson : SessionRowObservation → String
  | .absent => "absent"
  | .canonical => "canonical"
  | .duplicate => "duplicate"
  | .outOfScope => "out_of_scope"

def directReadJson : DirectSessionRead → String
  | .empty => "empty"
  | .visible => "visible"
  | .integrityFault => "integrity_fault"

def directCaseJson (row : SessionRowObservation) : String :=
  "{\"session_row\":" ++ jsonString (sessionRowJson row) ++
  ",\"expected\":" ++ jsonString (directReadJson (directSessionRead row)) ++ "}"

def directCasesJson : String := jsonArray (sessionRows.map directCaseJson)

end Conformance.ClientSessionDelivery
