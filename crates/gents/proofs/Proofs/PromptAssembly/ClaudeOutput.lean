import Proofs.PromptAssembly.ClaudeMap
import Lean

namespace PromptAssembly.ClaudeOutput
open Lean

/-- Messages transports the requested schema in output_config.format, alongside
advertised reasoning effort. These independent fields must not replace one
another. Additional provider parameters cannot supply the authoritative schema.
See https://platform.claude.com/docs/en/build-with-claude/structured-outputs. -/
structure Selection where
  effort : Option String
  schema : Option Json

def select (supported : Option (List String)) (effort : Option String)
    (schema : Option Json) : Selection :=
  ⟨ClaudeMap.selectedEffort supported effort, schema⟩

def Selection.toJson (selection : Selection) : Option Json :=
  let fields := (selection.effort.toList.map fun value => ("effort", Json.str value)) ++
    (selection.schema.toList.map fun schema =>
      ("format", Json.mkObj [("type", .str "json_schema"), ("schema", schema)]))
  if fields.isEmpty then none else some (Json.mkObj fields)

theorem schema_survives_effort_selection (supported : Option (List String))
    (effort : Option String) (schema : Option Json) :
    (select supported effort schema).schema = schema := rfl

theorem effort_uses_its_existing_owner (supported : Option (List String))
    (effort : Option String) (schema : Option Json) :
    (select supported effort schema).effort = ClaudeMap.selectedEffort supported effort := rfl

end PromptAssembly.ClaudeOutput
