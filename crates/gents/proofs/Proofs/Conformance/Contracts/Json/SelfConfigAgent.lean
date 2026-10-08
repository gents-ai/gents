import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.SelfConfig.Cases
import Proofs.SelfConfig.Selection
import Proofs.Conformance.Contracts.Json.SelfConfig

namespace Conformance.Contracts

open SelfConfig SelfConfig.ContractCases

private def agentBool (b : Bool) : String :=
  if b then "true" else "false"

private def agentOpName : AgentOp → String
  | .create => "create"
  | .edit => "edit"
  | .disable => "disable"

/-- Create input under the Agent/AgentContext document field names it authors. -/
private def agentCreateInputJson (i : AgentCreateInput) : String :=
  "{"
    ++ "\"display_name\":" ++ jsonString i.name ++ ","
    ++ "\"system_prompt\":" ++ jsonString i.systemPrompt ++ ","
    ++ "\"inference_profile_id\":" ++ jsonString i.profile ++ ","
    ++ "\"clone_from\":" ++ jsonString i.cloneFrom
  ++ "}"

/-- Edit fields as self-config patch entries; an omitted field has no entry. -/
private def agentEditPatchJson (f : AgentEditFields) : String :=
  let entries : List (FieldKey × Option PatchOp) :=
    [("display_name", f.name), ("system_prompt", f.systemPrompt),
     ("inference_profile_id", f.profile)]
  jsonArray (entries.filterMap fun (k, op) => op.map fun op =>
    selfConfigPatchEntryJson (k, op.value))

/-- The decision inputs shared by `agent_decision_cases` and
`agent_materialization_cases`, without a closing brace. -/
private def agentDecisionRowFields (r : AgentDecisionRow) : String :=
    "\"name\":" ++ jsonString r.name ++ ","
    ++ "\"agents\":" ++ jsonArray (r.catalog.agents.map fun a =>
        "{\"agent_id\":" ++ jsonString a.1 ++ ",\"enabled\":" ++ agentBool a.2 ++ "}") ++ ","
    ++ "\"protected_ids\":" ++ jsonStringArray r.catalog.protectedIds ++ ","
    ++ "\"default_id\":" ++ (match r.catalog.defaultId with
        | some id => jsonString id
        | none => "null") ++ ","
    ++ "\"published_profiles\":" ++ jsonStringArray r.profiles ++ ","
    ++ "\"operation\":" ++ jsonString (agentOpName r.operation.op) ++ ","
    ++ "\"target\":" ++ jsonString r.target ++ ","
    ++ "\"make_default\":" ++ agentBool r.makeDefault ++ ","
    ++ "\"create_input\":" ++ (match r.operation with
        | .create i => agentCreateInputJson i
        | _ => "null") ++ ","
    ++ "\"edit_patch\":" ++ (match r.operation with
        | .edit f => agentEditPatchJson f
        | _ => "null")

private def agentDecisionCaseJson (w : AgentDecisionWitness) : String :=
  "{" ++ agentDecisionRowFields w.row ++ ","
    ++ "\"accepted\":" ++ agentBool w.accepted
  ++ "}"

def agentDecisionCasesJson : String :=
  jsonArray (agentDecisionCases.map agentDecisionCaseJson)

private def effortName : Configuration.ReasoningEffort → String
  | .none => "none"
  | .minimal => "minimal"
  | .low => "low"
  | .medium => "medium"
  | .high => "high"
  | .xhigh => "xhigh"
  | .max => "max"
  | .ultra => "ultra"

private def resolvedSessionJson : Option Configuration.ResolvedSessionConfig → String
  | none => "null"
  | some r =>
    "{"
      ++ "\"instructions\":" ++ jsonString r.context.instructions ++ ","
      ++ "\"skill_ids\":" ++ jsonStringArray r.context.skillIds ++ ","
      ++ "\"tool_names\":" ++ jsonStringArray r.context.toolNames ++ ","
      ++ "\"backend_id\":" ++ jsonString r.inference.backendId ++ ","
      ++ "\"model\":" ++ jsonString r.inference.model ++ ","
      ++ "\"effort\":" ++ jsonOptionalString (r.inference.effort.map effortName)
    ++ "}"

/-- Rows are `materializedAgent` over the shared decision inputs; `session` is
the model's resolved configuration or null when nothing is materialized. -/
def agentMaterializationCasesJson : String :=
  jsonArray (agentMaterializationScenarios.map fun r =>
    "{" ++ agentDecisionRowFields { r.decision with name := r.name } ++ ","
      ++ "\"session\":" ++ resolvedSessionJson
        (materializedAgent r.decision.profiles r.decision.catalog r.decision.operation
          r.decision.target r.decision.makeDefault workerCandidate "node")
    ++ "}")

private def networkName : CommandPolicy.NetworkMode → String
  | .inherit => "inherit"
  | .disabled => "disabled"
  | .enabled => "enabled"

private def optionalNetworkJson : Option CommandPolicy.NetworkMode → String
  | none => "null"
  | some mode => jsonString (networkName mode)

/-- Every combination of the operation's guards and requested network over
each stored network mode; verdicts come from `SiblingToolsOperation.admitted`
and `resultNetwork`, never from handwritten expectations. -/
def siblingToolsOperations : List SiblingToolsOperation := Id.run do
  let mut rows : List SiblingToolsOperation := []
  for owner in [true, false] do
    for isProtected in [false, true] do
      for sharedContext in [false, true] do
        for sharedTools in [false, true] do
          for requested in [none, some .inherit, some .disabled, some .enabled] do
            for existing in [CommandPolicy.NetworkMode.inherit, .disabled, .enabled] do
              rows := rows ++ [{ ownerMatches := owner, isProtected := isProtected
                               , sharedContext := sharedContext, sharedTools := sharedTools
                               , requestedNetwork := requested
                               , existingNetwork := existing }]
  return rows

private def siblingToolsCaseJson (op : SiblingToolsOperation) : String :=
  "{"
    ++ "\"owner_matches\":" ++ agentBool op.ownerMatches ++ ","
    ++ "\"protected\":" ++ agentBool op.isProtected ++ ","
    ++ "\"shared_context\":" ++ agentBool op.sharedContext ++ ","
    ++ "\"shared_tools\":" ++ agentBool op.sharedTools ++ ","
    ++ "\"requested_network\":" ++ optionalNetworkJson op.requestedNetwork ++ ","
    ++ "\"existing_network\":" ++ jsonString (networkName op.existingNetwork) ++ ","
    ++ "\"admitted\":" ++ agentBool op.admitted ++ ","
    ++ "\"result_network\":" ++ jsonString (networkName op.resultNetwork)
  ++ "}"

def siblingToolsCasesJson : String :=
  jsonArray (siblingToolsOperations.map siblingToolsCaseJson)

end Conformance.Contracts
