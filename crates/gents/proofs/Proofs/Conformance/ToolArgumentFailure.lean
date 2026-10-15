import Proofs.PromptAssembly.ToolArgumentFailure
import Lean

namespace Conformance.ToolArgumentFailure
open Lean PromptAssembly.ToolArgumentFailure

def terminations : List Termination := [.unreported, .completed, .tokenLimit]
def failures : List ParseFailure := [.syntax, .unknownField, .missingField, .wrongType, .schema]

def terminationName : Termination → String
  | .unreported => "unreported"
  | .completed => "completed"
  | .tokenLimit => "token_limit"

def kindName : FailureKind → String
  | .malformed => "malformed"
  | .truncated => "truncated"
  | .unknownField => "unknown field"
  | .missingField => "missing field"
  | .wrongType => "wrong type"
  | .schema => "schema mismatch"

private def arguments : ParseFailure → List String
  | .syntax => ["{\"text\":\"unfinished", "{\"text\":?}"]
  | .unknownField => ["{\"text\":\"ok\",\"extra\":true}"]
  | .missingField => ["{}"]
  | .wrongType => ["{\"text\":42}"]
  | .schema => ["{\"text\":\"ok\",\"mode\":\"unknown\"}"]

private def caseJson (termination : Termination) (failure : ParseFailure) (args : String) : Json :=
  Json.mkObj [("termination", toJson (terminationName termination)),
    ("arguments", toJson args), ("kind", toJson (kindName (classify termination failure)))]

def casesJson : String :=
  (toJson (terminations.flatMap fun termination =>
    failures.flatMap fun failure =>
      (arguments failure).map (caseJson termination failure))).compress

end Conformance.ToolArgumentFailure

def main : IO Unit := IO.println Conformance.ToolArgumentFailure.casesJson
