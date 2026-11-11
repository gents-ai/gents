import Proofs.Conformance.Family
import Proofs.Session.Compaction.Checkpoint
import Proofs.Session.PromptAssembly.ClaudeOutput

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

structure RawField where
  name : String
  value : String
  deriving ToJson

structure FallbackInput where
  name : String
  raw : String
  fields : List RawField
  deriving ToJson

structure FallbackCheckpoint where
  goal : String
  constraintsAndPreferences : List String
  completedWork : List String
  inProgress : List String
  blockers : List String
  currentWork : List String
  keyDecisions : List String
  errorsAndFixes : List String
  verification : List String
  uncertainties : List String
  nextActions : List String
  criticalContext : List String
  deriving ToJson

private def stringsAt (json : Json) (name : String) : List String :=
  match json.getObjValD name with
  | .arr items => items.toList.filterMap fun item => item.getStr?.toOption
  | _ => []

private def checkpointOf (json : Json) : FallbackCheckpoint where
  goal := (json.getObjValAs? String "goal").toOption.getD ""
  constraintsAndPreferences := stringsAt json "constraints_and_preferences"
  completedWork := stringsAt json "completed_work"
  inProgress := stringsAt json "in_progress"
  blockers := stringsAt json "blockers"
  currentWork := stringsAt json "current_work"
  keyDecisions := stringsAt json "key_decisions"
  errorsAndFixes := stringsAt json "errors_and_fixes"
  verification := stringsAt json "verification"
  uncertainties := stringsAt json "uncertainties"
  nextActions := stringsAt json "next_actions"
  criticalContext := stringsAt json "critical_context"

@[conformance_family]
def fallbackCheckpoint : Conformance.Family FallbackInput (Option FallbackCheckpoint) where
  inputs := cases.map fun (name, fields) =>
    { name, raw := rawObject fields
      fields := fields.map fun (field, value) => { name := field, value := value.compress } }
  model := fun i =>
    (Compaction.Checkpoint.decode (i.fields.map fun field =>
      (field.name, (Json.parse field.value).toOption.getD Json.null))).map checkpointOf

def listFieldsJson : Json := toJson Compaction.Checkpoint.listFields

private def schema : Json := Json.mkObj [
  ("type", .str "object"), ("additionalProperties", .bool false),
  ("properties", Json.mkObj [("goal", Json.mkObj [("type", .str "string")])]),
  ("required", toJson ["goal"])]

structure OutputInput where
  schema : Option String
  supported : Option (List String)
  effort : Option String
  deriving ToJson

structure OutputFormat where
  kind : String
  schema : String
  deriving ToJson

structure OutputConfig where
  effort : Option String
  format : Option OutputFormat
  deriving ToJson

@[conformance_family]
def claudeOutputConfig : Conformance.Family OutputInput (Option OutputConfig) where
  inputs :=
    ([none, some schema.compress] : List (Option String)).flatMap fun schema =>
    ([ (none, some "high"), (some ["high"], none), (some ["high"], some "low"),
       (some ["high"], some "high"), (some ["forged"], some "forged") ] :
      List (Option (List String) × Option String)).map fun (supported, effort) =>
      { schema, supported, effort }
  model := fun i =>
    let selection := PromptAssembly.ClaudeOutput.select PromptAssembly.ClaudeBody.sonnet5
      PromptAssembly.ClaudeBody.interactiveTurn i.supported i.effort
      (i.schema.bind fun text => (Json.parse text).toOption)
    selection.toJson.map fun config =>
      ({ effort := (config.getObjValAs? String "effort").toOption
         format := (config.getObjVal? "format").toOption.map fun format =>
           ({ kind := (format.getObjValAs? String "type").toOption.getD ""
              schema := (format.getObjValD "schema").compress } : OutputFormat) } : OutputConfig)

end Conformance.CompactionCheckpoint
