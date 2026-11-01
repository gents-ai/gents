import Proofs.Compaction.Checkpoint
import Proofs.PromptAssembly.ClaudeOutput

namespace Conformance.CompactionCheckpoint
open Lean

private def rawObject (fields : List (String × Json)) : String :=
  "{" ++ String.intercalate "," (fields.map fun (key, value) =>
    (Json.str key).compress ++ ":" ++ value.compress) ++ "}"

private def base : List (String × Json) :=
  [("goal", .str "Finish the user's task"), ("next_actions", toJson ["Run the pending check"])]

private def fieldCase (name : String) (value : Json) : List (String × Json) :=
  base.filter (fun field => field.1 != name) ++ [(name, value)]

private def whitespace : String := String.mk <|
  ([9, 10, 11, 12, 13, 0x20, 0x85, 0xA0, 0x1680, 0x2003, 0x2028, 0x2029,
    0x202F, 0x205F, 0x3000] : List Nat).map Char.ofNat

private def cases : List (String × List (String × Json)) :=
  (Compaction.Checkpoint.listFields.map fun name =>
    ("scalar_" ++ name, fieldCase name (.str "The last action and its exact result: café ✓"))) ++
  [ ("canonical_arrays", fieldCase "current_work" (toJson ["first", "second"]))
  , ("blank_optional_item", fieldCase "current_work" (.str ""))
  , ("mixed_array_rejected", fieldCase "current_work" (.arr #[.str "first", .bool true]))
  , ("null_rejected", fieldCase "current_work" .null)
  , ("number_rejected", fieldCase "current_work" (toJson (12 : Nat)))
  , ("object_rejected", fieldCase "current_work" (Json.mkObj [("text", .str "task")]))
  , ("unknown_field_rejected", fieldCase "unknown_work" (.str "task"))
  , ("missing_goal_rejected", base.filter (fun field => field.1 != "goal"))
  , ("blank_goal_rejected", fieldCase "goal" (.str whitespace))
  , ("missing_next_rejected", base.filter (fun field => field.1 != "next_actions"))
  , ("blank_next_rejected", fieldCase "next_actions" (.str whitespace))
  , ("duplicate_goal_rejected", base ++ [("goal", .str "second")])
  , ("duplicate_list_rejected", base ++ [("current_work", .str "first"), ("current_work", .str "second")]) ]

def checkpointCasesJson : Json := toJson (cases.map fun (name, fields) => Json.mkObj [
  ("name", toJson name), ("raw", toJson (rawObject fields)),
  ("expected", (Compaction.Checkpoint.decode fields).getD .null)])

def listFieldsJson : Json := toJson Compaction.Checkpoint.listFields

private def schema : Json := Json.mkObj [
  ("type", .str "object"), ("additionalProperties", .bool false),
  ("properties", Json.mkObj [("goal", Json.mkObj [("type", .str "string")])]),
  ("required", toJson ["goal"])]

def outputCasesJson : Json := toJson <|
  ([none, some schema] : List (Option Json)).flatMap fun schema =>
  ([ (none, some "high"), (some ["high"], none), (some ["high"], some "low"),
     (some ["high"], some "high"), (some ["forged"], some "forged") ] :
    List (Option (List String) × Option String)).map fun (supported, effort) => Json.mkObj [
      ("schema", schema.getD .null), ("supported", toJson supported),
      ("effort", toJson effort),
      ("expected", (PromptAssembly.ClaudeOutput.select PromptAssembly.ClaudeBody.sonnet5
        PromptAssembly.ClaudeBody.interactiveTurn supported effort schema).toJson.getD .null)]

end Conformance.CompactionCheckpoint
