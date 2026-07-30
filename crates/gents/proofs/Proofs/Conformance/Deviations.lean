import Proofs.Conformance.Boundaries

namespace Conformance.Contracts

structure Deviation where
  id : String
  domain : String
  subject : String
  statement : String
  acceptedFailureMode : Option String := none
  acceptedFollowUp : Option String := none
  deriving Repr

def deviations : List Deviation :=
  []

def Deviation.toJson (deviation : Deviation) : String :=
  "{"
    ++ "\"id\":" ++ jsonString deviation.id ++ ","
    ++ "\"domain\":" ++ jsonString deviation.domain ++ ","
    ++ "\"subject\":" ++ jsonString deviation.subject ++ ","
    ++ "\"statement\":" ++ jsonString deviation.statement ++ ","
    ++ "\"accepted_failure_mode\":"
      ++ jsonOptionalString deviation.acceptedFailureMode ++ ","
    ++ "\"accepted_follow_up\":"
      ++ jsonOptionalString deviation.acceptedFollowUp
    ++ "}"

def deviationsJson : String :=
  jsonArray (deviations.map Deviation.toJson)

end Conformance.Contracts
