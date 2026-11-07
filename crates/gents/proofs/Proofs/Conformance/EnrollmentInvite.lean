import Proofs.Enrollment.Invite
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.EnrollmentInvite

open Conformance.Contracts

def lengths : List Nat := [0, 1, 1365, 1899, 1900, 1901, 32768]

private def caseJson (length : Nat) : String :=
  "{\"length\":" ++ toString length ++ ",\"linkable\":" ++
    (if Enrollment.Invite.linkable length then "true" else "false") ++ "}"

def casesJson : String := jsonArray (lengths.map caseJson)

private def windowCases : List (Nat × Nat × Nat) :=
  [(1000000, 1003600, 1000000), (1000030, 1000100, 1000000),
   (1000031, 1000100, 1000000), (999900, 1000000, 1000000),
   (1000000, 1086400, 1000000), (1000000, 1086401, 1000000),
   (1000001, 1000000, 999999)]

private def windowCaseJson (v : Nat × Nat × Nat) : String :=
  let (issued, expires, now) := v
  "{\"issued\":" ++ toString issued ++ ",\"expires\":" ++ toString expires ++
    ",\"now\":" ++ toString now ++ ",\"admissible\":" ++
    (if Enrollment.Invite.admissible 1 issued expires now then "true" else "false") ++ "}"

def windowCasesJson : String := jsonArray (windowCases.map windowCaseJson)

private def schemaCases : List (Option String × Option String) :=
  [(none, none), (none, some "a"), (some "a", none),
   (some "", some ""), (some "a", some "a"), (some "a", some "b")]

private def optionalString : Option String → String
  | none => "null"
  | some s => "\"" ++ s ++ "\""

private def schemaCaseJson (pair : Option String × Option String) : String :=
  "{\"signed\":" ++ optionalString pair.1 ++ ",\"local\":" ++ optionalString pair.2 ++
    ",\"matches\":" ++
    (if Enrollment.Invite.schemaEvidenceMatches pair.1 pair.2 then "true" else "false") ++ "}"

def schemaCasesJson : String := jsonArray (schemaCases.map schemaCaseJson)

end Conformance.EnrollmentInvite
