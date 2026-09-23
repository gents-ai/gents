import Proofs.Eval
import Proofs.Conformance.ContractTypes

/-! Generated witnesses for the eval outcome projection. Rust must classify every
(kind, provider reason) combination exactly as the model does. -/
namespace Conformance.Eval

open Conformance.Contracts
open _root_.Eval

private def boolJson (b : Bool) : String := if b then "true" else "false"

private def reasonJson : Option ProviderReason → String
  | none => "null"
  | some r => jsonString r.toDefraDB

private def row (k : OutcomeKind) (r : Option ProviderReason) : String :=
  "{\"kind\":" ++ jsonString k.toDefraDB
    ++ ",\"provider_reason\":" ++ reasonJson r
    ++ ",\"class\":" ++ jsonString (classify k r).toDefraDB
    ++ ",\"subject_causable\":" ++ boolJson (subjectCausable k r) ++ "}"

def evalOutcomeCasesJson : String :=
  jsonArray (allKinds.flatMap fun k =>
    ([none] ++ allReasons.map some).map fun r => row k r)

end Conformance.Eval
