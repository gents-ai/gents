import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.SelfConfig.Cases
import Proofs.SelfConfig.Selection
import Proofs.Conformance.Contracts.Json.SelfConfig

namespace Conformance.Contracts

open SelfConfig SelfConfig.ContractCases

private def agentBool (b : Bool) : String :=
  if b then "true" else "false"

private def agentOpName : AgentOperation → String
  | .create _ => "create"
  | .edit _ => "edit"
  | .disable => "disable"

private def agentCreateInputJson (i : AgentCreateInput) : String :=
  "{"
    ++ "\"display_name\":" ++ jsonString i.name ++ ","
    ++ "\"system_prompt\":" ++ jsonString i.systemPrompt ++ ","
    ++ "\"inference_profile_id\":" ++ jsonString i.profile ++ ","
    ++ "\"clone_from\":" ++ jsonString i.cloneFrom
  ++ "}"

/-- The edit as patch entries in `agentEditKeys` order. `enabled` is the JSON
text `true` or `false`, and `tags` a JSON array text, as in `self_config_cases`. -/
private def agentEditPatchJson (f : AgentEditFields) : String :=
  let entries : List (FieldKey × Option PatchOp) :=
    [("display_name", f.name), ("description", f.description),
     ("system_prompt", f.systemPrompt), ("inference_profile_id", f.profile),
     ("context_id", f.contextId),
     ("enabled", f.enabled.map fun b => .set (if b then "true" else "false")),
     ("tags", f.tags)]
  jsonArray (entries.filterMap fun (k, op) => op.map fun op =>
    selfConfigPatchEntryJson (k, op.value))

private def actorKindName : ActorKind → String
  | .person => "person"
  | .agent => "agent"

private def frontEndName : FrontEnd → String
  | .tool => "tool"
  | .cli => "cli"
  | .desktop => "desktop"

def agentRefusalName : AgentRefusal → String
  | .catalogNotHeld => "catalog_not_held"
  | .agentNotHeld => "agent_not_held"
  | .targetExists => "target_exists"
  | .targetMissing => "target_missing"
  | .invalidInput => "invalid_input"
  | .defaultDisabled => "default_disabled"
  | .selfLockout => "self_lockout"

/-- The acting actor and the grants native ACP gives it
(`SelfConfig.actorGrantsOf`): `catalog` is `nodes manage_agents` on the Node and
`configures` the Agents whose document it may update. -/
private def actorJson (a : Actor) : String :=
  "{"
    ++ "\"did\":" ++ jsonString a.did ++ ","
    ++ "\"kind\":" ++ jsonString (actorKindName a.kind) ++ ","
    ++ "\"catalog\":" ++ agentBool a.grants.catalog ++ ","
    ++ "\"configures\":" ++ jsonStringArray a.grants.configures
  ++ "}"

private def agentDecisionRowFields (r : AgentDecisionRow) : String :=
    "\"name\":" ++ jsonString r.name ++ ","
    ++ "\"agents\":" ++ jsonArray (r.catalog.agents.map fun a =>
        "{\"agent_did\":" ++ jsonString a.id ++ ",\"enabled\":" ++ agentBool a.enabled
          ++ ",\"tags\":" ++ jsonStringArray a.tags ++ "}") ++ ","
    ++ "\"default_id\":" ++ (match r.catalog.defaultId with
        | some id => jsonString id
        | none => "null") ++ ","
    ++ "\"published_profiles\":" ++ jsonStringArray r.profiles ++ ","
    ++ "\"actor\":" ++ actorJson r.actor ++ ","
    ++ "\"front_ends\":" ++ jsonStringArray ([FrontEnd.tool, .cli, .desktop].map frontEndName) ++ ","
    ++ "\"operation\":" ++ jsonString (agentOpName r.request.operation) ++ ","
    ++ "\"target\":" ++ jsonString r.request.target ++ ","
    ++ "\"make_default\":" ++ agentBool r.request.makeDefault ++ ","
    ++ "\"create_input\":" ++ (match r.request.operation with
        | .create i => agentCreateInputJson i
        | _ => "null") ++ ","
    ++ "\"edit_patch\":" ++ (match r.request.operation with
        | .edit f => agentEditPatchJson f
        | _ => "null")

/-- `accepted` and `refusal` hold for every front end in `front_ends`
(`SelfConfig.admission_ignores_front_end`); a refused preview reports the same
`refusal` as a successful answer (`SelfConfig.preview_refusal_matches_commit`). -/
private def agentDecisionCaseJson (w : AgentDecisionWitness) : String :=
  "{" ++ agentDecisionRowFields w.row ++ ","
    ++ "\"accepted\":" ++ agentBool w.accepted ++ ","
    ++ "\"refusal\":" ++ (match w.admission with
        | .error e => jsonString (agentRefusalName e)
        | .ok _ => "null")
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

private def candidateFixtureJson (f : CandidateFixture) : String :=
  "{"
    ++ "\"node_did\":" ++ jsonString f.nodeDid ++ ","
    ++ "\"agents\":" ++ jsonArray (f.agents.map fun (id, a) =>
        "{\"agent_did\":" ++ jsonString id ++ ","
          ++ "\"creation_nonce\":" ++ jsonString id ++ ","
          ++ "\"context_id\":" ++ jsonOptionalString a.contextId ++ ","
          ++ "\"inference_profile_id\":" ++ jsonString a.profileId ++ ","
          ++ "\"enabled\":" ++ agentBool a.enabled ++ "}") ++ ","
    ++ "\"contexts\":" ++ jsonArray (f.contexts.map fun (id, c) =>
        "{\"context_id\":" ++ jsonString id ++ ","
          ++ "\"system_prompt\":" ++ jsonString c.instructions ++ ","
          ++ "\"skill_ids\":" ++ jsonStringArray c.skillIds ++ ","
          ++ "\"tool_names\":" ++ jsonStringArray c.toolNames ++ "}") ++ ","
    ++ "\"inference_profiles\":" ++ jsonArray (f.profiles.map fun (id, m) =>
        "{\"profile_id\":" ++ jsonString id ++ ","
          ++ "\"backend_id\":" ++ jsonString m.backendId ++ ","
          ++ "\"model_name\":" ++ jsonString m.model ++ ","
          ++ "\"reasoning_effort\":" ++ jsonOptionalString (m.effort.map effortName) ++ "}") ++ ","
    ++ "\"inference_backends\":" ++ jsonArray (f.backends.map fun (id, b) =>
        "{\"backend_id\":" ++ jsonString id ++ ","
          ++ "\"enabled\":" ++ agentBool b.enabled ++ "}")
  ++ "}"

def agentMaterializationCasesJson : String :=
  jsonArray (agentMaterializationScenarios.map fun r =>
    "{" ++ agentDecisionRowFields { r.decision with name := r.name } ++ ","
      ++ "\"candidate\":" ++ candidateFixtureJson workerFixture ++ ","
      ++ "\"session\":" ++ resolvedSessionJson
        (materializedAgent r.decision.profiles r.decision.catalog r.decision.actor
          r.decision.request workerFixture.registry workerFixture.nodeDid)
    ++ "}")

private def networkName : CommandPolicy.NetworkMode → String
  | .inherit => "inherit"
  | .disabled => "disabled"
  | .enabled => "enabled"

private def optionalNetworkJson : Option CommandPolicy.NetworkMode → String
  | none => "null"
  | some mode => jsonString (networkName mode)

def siblingToolsOperations : List SiblingToolsOperation := Id.run do
  let mut rows : List SiblingToolsOperation := []
  for owner in [true, false] do
    for sharedContext in [false, true] do
      for sharedTools in [false, true] do
        for requested in [none, some .inherit, some .disabled, some .enabled] do
          for existing in [CommandPolicy.NetworkMode.inherit, .disabled, .enabled] do
            rows := rows ++ [{ ownerMatches := owner
                             , sharedContext := sharedContext, sharedTools := sharedTools
                             , requestedNetwork := requested
                             , existingNetwork := existing }]
  return rows

private def siblingToolsCaseJson (op : SiblingToolsOperation) : String :=
  "{"
    ++ "\"owner_matches\":" ++ agentBool op.ownerMatches ++ ","
    ++ "\"shared_context\":" ++ agentBool op.sharedContext ++ ","
    ++ "\"shared_tools\":" ++ agentBool op.sharedTools ++ ","
    ++ "\"requested_network\":" ++ optionalNetworkJson op.requestedNetwork ++ ","
    ++ "\"existing_network\":" ++ jsonString (networkName op.existingNetwork) ++ ","
    ++ "\"admitted\":" ++ agentBool op.admitted ++ ","
    ++ "\"result_network\":" ++ jsonString (networkName op.resultNetwork)
  ++ "}"

def siblingToolsCasesJson : String :=
  jsonArray (siblingToolsOperations.map siblingToolsCaseJson)

private def optionalBoolJson : Option Bool → String
  | none => "null"
  | some b => agentBool b

private def jobGuardCaseJson (r : JobGuardRow) : String :=
  "{"
    ++ "\"name\":" ++ jsonString r.name ++ ","
    ++ "\"writer\":" ++ jsonString (if r.operator then "operator" else "model") ++ ","
    ++ "\"selected_before\":" ++ agentBool r.selectedBefore ++ ","
    ++ "\"native_authorized\":" ++ agentBool r.nativeAuthorized ++ ","
    ++ "\"native_source_authorized\":" ++ agentBool r.nativeSourceAuthorized ++ ","
    ++ "\"add_schema_tool\":" ++ agentBool r.addSchemaTool ++ ","
    ++ "\"stored_integrations\":" ++ r.stored.getD "null" ++ ","
    ++ "\"candidate_integrations\":" ++ r.candidate ++ ","
    ++ "\"accepted\":" ++ agentBool r.accepted
  ++ "}"

def selfConfigSelectionCasesJson : String :=
  let bools := [false, true]
  let toolFlags := Id.run do
    let mut rows : List String := []
    for existing in bools do
      for requested in [none, some false, some true] do
        rows := rows ++ ["{\"existing\":" ++ agentBool existing ++ ","
          ++ "\"requested\":" ++ optionalBoolJson requested ++ ","
          ++ "\"selected\":" ++ agentBool (selectedToolFlag requested existing) ++ "}"]
    return rows
  "{\"tool_flag_cases\":" ++ jsonArray toolFlags
    ++ ",\"job_guard_cases\":" ++ jsonArray (jobGuardRows.map jobGuardCaseJson) ++ "}"

end Conformance.Contracts
