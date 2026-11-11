import Proofs.PromptAssembly.ClaudeBody
import Proofs.PromptAssembly.ClaudeMap
import Lean

namespace PromptAssembly.ClaudeOutput
open Lean

/-- Messages transports the requested schema in output_config.format, alongside
the effort the body owner decides from the advertised reasoning efforts. These independent fields must not replace one
another. Additional provider parameters cannot supply the authoritative schema.
See https://platform.claude.com/docs/en/build-with-claude/structured-outputs. -/
structure Selection where
  effort : Option String
  schema : Option Json

def select (facts : ClaudeBody.ModelFacts) (turn : Option String → ClaudeBody.Request)
    (supported : Option (List String)) (effort : Option String) (schema : Option Json) :
    Selection :=
  ⟨ClaudeBody.effort facts (turn (ClaudeMap.selectedEffort supported effort)), schema⟩

def Selection.toJson (selection : Selection) : Option Json :=
  let fields := (selection.effort.toList.map fun value => ("effort", Json.str value)) ++
    (selection.schema.toList.map fun schema =>
      ("format", Json.mkObj [("type", .str "json_schema"), ("schema", schema)]))
  if fields.isEmpty then none else some (Json.mkObj fields)

theorem schema_survives_effort_selection (facts : ClaudeBody.ModelFacts)
    (turn : Option String → ClaudeBody.Request) (supported : Option (List String))
    (effort : Option String) (schema : Option Json) :
    (select facts turn supported effort schema).schema = schema := rfl

theorem effort_uses_its_existing_owner (facts : ClaudeBody.ModelFacts)
    (turn : Option String → ClaudeBody.Request) (supported : Option (List String))
    (effort : Option String) (schema : Option Json) :
    (select facts turn supported effort schema).effort =
      ClaudeBody.effort facts (turn (ClaudeMap.selectedEffort supported effort)) := rfl

end PromptAssembly.ClaudeOutput
