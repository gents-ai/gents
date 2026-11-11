import Proofs.Enrollment.State
import Proofs.Conformance.Contracts.Json.Core

namespace Conformance.EnrollmentCandidateName

open Conformance.Contracts

private def optionalString : Option String → String
  | none => "null"
  | some value => jsonString value

private def caseJson (name : Option String) : String :=
  "{\"name\":" ++ optionalString name ++ ",\"fields\":" ++
    jsonStringArray (Enrollment.candidateNameFields name) ++ "}"

def casesJson : String := jsonArray ([none, some "Ivan", some "顾问"] |>.map caseJson)

end Conformance.EnrollmentCandidateName
