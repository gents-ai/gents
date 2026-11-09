import Proofs.Configuration.Affected
import Lean

namespace Conformance.ConfigurationAffected
open Lean

structure InputDocument where
  key : String
  required : List String
  localValid : Bool
  deriving FromJson

structure Input where
  documents : List InputDocument
  changed : List String
  deriving FromJson

def execute (input : Input) : Bool :=
  let documents := input.documents.map fun document =>
    (_root_.Configuration.Affected.Document.mk document.key document.required document.localValid)
  _root_.Configuration.Affected.accepted documents input.changed

end Conformance.ConfigurationAffected

def main : IO Unit := do
  let line ← (← IO.getStdin).getLine
  match Lean.Json.parse line >>= Lean.fromJson? with
  | .error reason => throw (IO.userError reason)
  | .ok input => IO.println (Lean.toJson (Conformance.ConfigurationAffected.execute input)).compress
