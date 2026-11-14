import Proofs.Session.PromptAssembly.ToolArgs
import Proofs.Conformance.Family

namespace Conformance.PromptAssemblyRepair

open Lean

private inductive Payload where
  | empty
  | raw
  | sanitized
  deriving DecidableEq, Repr

private def sanitizePayload : Payload → Payload
  | .empty => .empty
  | .raw => .sanitized
  | .sanitized => .sanitized

private instance : PromptAssembly.LeafSanitizer Payload where
  sanitize := sanitizePayload
  idempotent := by intro p; cases p <;> rfl

private def argsName : PromptAssembly.ToolArgs Payload → String
  | .object .empty => "object:empty"
  | .object .raw => "object:raw"
  | .object .sanitized => "object:sanitized"
  | .str none => "str:unparsed"
  | .str (some .empty) => "str:object:empty"
  | .str (some .raw) => "str:object:raw"
  | .str (some .sanitized) => "str:object:sanitized"
  | .array => "array"
  | .scalar => "scalar"
  | .null => "null"

private def repairVectors : List (String × PromptAssembly.ToolArgs Payload) :=
  [ ("object-passes-through", .object .raw)
  , ("empty-object-passes-through", .object .empty)
  , ("stringified-object-salvages", .str (some .raw))
  , ("unparsable-string-collapses", .str none)
  , ("array-collapses", .array)
  , ("scalar-collapses", .scalar)
  , ("null-collapses", .null)
  ]

private def isPayloadOnly : PromptAssembly.ToolArgs Payload → Bool
  | .object _ => true
  | _ => false

structure ArgsInput where
  name : String
  input : String
  payloadOnly : Bool
  deriving ToJson

structure ArgsOutput where
  expected : String
  expectedTwice : String
  deriving ToJson

@[conformance_family]
def args : Family ArgsInput ArgsOutput where
  inputs := repairVectors.map fun vector =>
    { name := vector.1, input := argsName vector.2, payloadOnly := isPayloadOnly vector.2 }
  model := fun i =>
    match (repairVectors.find? (argsName ·.2 == i.input)) with
    | some vector =>
      let once := PromptAssembly.repairArgs Payload.empty vector.2
      { expected := argsName once
        expectedTwice := argsName (PromptAssembly.repairArgs Payload.empty once) }
    | none => { expected := "", expectedTwice := "" }

end Conformance.PromptAssemblyRepair
