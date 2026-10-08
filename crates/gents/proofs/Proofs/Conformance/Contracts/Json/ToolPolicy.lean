import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.ToolPolicy.Cases
import Proofs.ToolPolicy.FieldRead
import Proofs.ToolPolicy.WriteInput
import Proofs.ToolPolicy.ApplicationWrite

namespace Conformance.Contracts

open ToolPolicy.ContractCases

def writeGrantViewJson (grant : WriteGrantView) : String :=
  "{"
    ++ "\"tool\":" ++ jsonString grant.tool ++ ","
    ++ "\"collection\":" ++ jsonString grant.collection ++ ","
    ++ "\"fields\":" ++ jsonStringArray grant.fields
  ++ "}"

def boolJson (b : Bool) : String := if b then "true" else "false"

def fieldReadCasesJson : String := Id.run do
  let mut rows := []
  for widths in [[], [1], [1, 2, 3, 4], List.replicate 2000 1 ++ [4], List.replicate 501 4] do
    for offset in [0, 1, 2, 3, 6, 10, 2000, 2004, 2005] do
      for present in [false, true] do
        for hashOk in [false, true] do
          for granted in [false, true] do
            let result := ToolPolicy.FieldRead.page widths offset 2000 granted granted granted present hashOk
            rows := rows ++ ["{\"widths\":" ++ "[" ++ String.intercalate "," (widths.map toString) ++ "]"
              ++ ",\"offset\":" ++ toString offset
              ++ ",\"granted\":" ++ boolJson granted
              ++ ",\"present\":" ++ boolJson present
              ++ ",\"hashOk\":" ++ boolJson hashOk
              ++ ",\"next\":" ++ (match result with | none => "null" | some n => toString n) ++ "}"]
  return "[" ++ String.intercalate "," rows ++ "]"

def writeInputKindName : ToolPolicy.WriteInput.Kind → String
  | .text => "text" | .integer => "integer" | .number => "number"
  | .boolean => "boolean" | .array => "array" | .object => "object" | .null => "null"

def writeInputCasesJson : String := Id.run do
  let mut rows := []
  for expected in [ToolPolicy.WriteInput.Kind.text, .integer, .number, .boolean] do
    for actual in [none, some .text, some .integer, some .number, some .boolean,
        some .array, some .object, some .null] do
      for nullable in [false, true] do
        for required in [false, true] do
          for filled in [false, true] do
            rows := rows ++ ["{\"expected\":" ++ jsonString (writeInputKindName expected)
              ++ ",\"actual\":" ++ (match actual with
                | none => "null" | some kind => jsonString (writeInputKindName kind))
              ++ ",\"nullable\":" ++ boolJson nullable
              ++ ",\"required\":" ++ boolJson required
              ++ ",\"filled\":" ++ boolJson filled
              ++ ",\"accepted\":" ++ boolJson (ToolPolicy.WriteInput.admits expected nullable required filled actual)
              ++ "}"]
  return jsonArray rows

def invocationCorrelationCasesJson : String := Id.run do
  let encode := fun value : Option String => match value with
    | none => "null" | some text => jsonString text
  let mut rows := []
  for request in [none, some "", some " ", some "request-1"] do
    for supplied in [none, some "", some " ", some "event-1"] do
      rows := rows ++ ["{\"request\":" ++ encode request
        ++ ",\"supplied\":" ++ encode supplied
        ++ ",\"expected\":" ++ encode (ToolPolicy.WriteInput.invocationCorrelation request supplied) ++ "}"]
  return jsonArray rows

def surfaceViewJson (v : SurfaceView) : String :=
  "{"
    ++ "\"file_rank\":" ++ toString v.fileRank ++ ","
    ++ "\"goal_tools\":" ++ boolJson v.goalTools ++ ","
    ++ "\"goal_create\":" ++ boolJson v.goalCreate ++ ","
    ++ "\"defra_query\":" ++ boolJson v.defraQuery ++ ","
    ++ "\"self_config\":" ++ boolJson v.selfConfig ++ ","
    ++ "\"memory\":" ++ boolJson v.memory ++ ","
    ++ "\"schema_management\":" ++ boolJson v.schemaManagement ++ ","
    ++ "\"p2p_read\":" ++ boolJson v.p2pRead ++ ","
    ++ "\"p2p_mutate\":" ++ boolJson v.p2pMutate ++ ","
    ++ "\"p2p_mutation_allowed\":" ++ boolJson v.p2pMutationAllowed ++ ","
    ++ "\"p2p_overlay_before\":" ++ jsonStringArray v.p2pOverlayBefore ++ ","
    ++ "\"p2p_overlay_after\":" ++ jsonStringArray v.p2pOverlayAfter ++ ","
    ++ "\"p2p_overlay_allowed\":" ++ boolJson v.p2pOverlayAllowed ++ ","
    ++ "\"session_history\":" ++ boolJson v.sessionHistory ++ ","
    ++ "\"context_budget\":" ++ boolJson v.contextBudget ++ ","
    ++ "\"session_messages\":" ++ boolJson v.sessionMessages ++ ","
    ++ "\"skills\":" ++ boolJson v.skills ++ ","
    ++ "\"lsp\":" ++ boolJson v.lsp ++ ","
    ++ "\"bash_mode\":" ++ toString v.bashMode ++ ","
    ++ "\"bash_net\":" ++ toString v.bashNet ++ ","
    ++ "\"bash_sandbox\":" ++ boolJson v.bashSandbox ++ ","
    ++ "\"bash_allowed_kind\":" ++ jsonString v.bashAllowedKind ++ ","
    ++ "\"bash_allowed_prefixes\":" ++ jsonStringMatrix v.bashAllowedPrefixes ++ ","
    ++ "\"bash_forbidden\":" ++ jsonStringMatrix v.bashForbidden ++ ","
    ++ "\"bash_read_only_kind\":" ++ jsonString v.bashReadOnlyKind ++ ","
    ++ "\"bash_read_only_keys\":" ++ jsonStringArray v.bashReadOnlyKeys ++ ","
    ++ "\"cli_scope_kind\":" ++ jsonString v.cliScopeKind ++ ","
    ++ "\"cli_keys\":" ++ jsonStringArray v.cliKeys ++ ","
    ++ "\"mcp_probe\":" ++ jsonString v.mcpProbe ++ ","
    ++ "\"mcp_scope_kind\":" ++ jsonString v.mcpScopeKind ++ ","
    ++ "\"mcp_services\":" ++ jsonStringArray v.mcpServices ++ ","
    ++ "\"mcp_permits\":" ++ boolJson v.mcpPermits ++ ","
    ++ "\"defra_collections_scope_kind\":" ++ jsonString v.defraCollectionsScopeKind ++ ","
    ++ "\"defra_collections_keys\":" ++ jsonStringArray v.defraCollectionsKeys ++ ","
    ++ "\"p2p_collections_scope_kind\":" ++ jsonString v.p2pCollectionsScopeKind ++ ","
    ++ "\"p2p_collections_keys\":" ++ jsonStringArray v.p2pCollectionsKeys ++ ","
    ++ "\"self_config_categories_scope_kind\":"
      ++ jsonString v.selfConfigCategoriesScopeKind ++ ","
    ++ "\"self_config_categories_keys\":"
      ++ jsonStringArray v.selfConfigCategoriesKeys ++ ","
    ++ "\"agent_targets_scope_kind\":" ++ jsonString v.agentTargetsScopeKind ++ ","
    ++ "\"agent_targets_keys\":" ++ jsonStringArray v.agentTargetsKeys ++ ","
    ++ "\"background_tools_scope_kind\":" ++ jsonString v.backgroundToolsScopeKind ++ ","
    ++ "\"background_tools_keys\":" ++ jsonStringArray v.backgroundToolsKeys ++ ","
    ++ "\"write_probe_tool\":" ++ jsonString v.writeProbe.1 ++ ","
    ++ "\"write_probe_collection\":" ++ jsonString v.writeProbe.2 ++ ","
    ++ "\"write_scope_kind\":" ++ jsonString v.writeScopeKind ++ ","
    ++ "\"write_grants\":"
      ++ jsonArray (v.writeGrants.map writeGrantViewJson) ++ ","
    ++ "\"write_fields\":" ++ jsonArray (v.writeFields.map jsonString) ++ ","
    ++ "\"query_probe_tool\":" ++ jsonString v.queryProbe.1 ++ ","
    ++ "\"query_probe_collection\":" ++ jsonString v.queryProbe.2 ++ ","
    ++ "\"query_scope_kind\":" ++ jsonString v.queryScopeKind ++ ","
    ++ "\"query_grants\":"
      ++ jsonArray (v.queryGrants.map writeGrantViewJson) ++ ","
    ++ "\"query_fields\":" ++ jsonArray (v.queryFields.map jsonString) ++ ","
    ++ "\"eth_query_methods_kind\":" ++ jsonString v.ethQueryMethodsKind ++ ","
    ++ "\"eth_query_methods_keys\":" ++ jsonStringArray v.ethQueryMethodsKeys ++ ","
    ++ "\"eth_call_tools_kind\":" ++ jsonString v.ethCallToolsKind ++ ","
    ++ "\"eth_call_tools_keys\":" ++ jsonStringArray v.ethCallToolsKeys ++ ","
    ++ "\"plugin_tools_kind\":" ++ jsonString v.pluginToolsKind ++ ","
    ++ "\"plugin_tools_keys\":" ++ jsonStringArray v.pluginToolsKeys
  ++ "}"

def toolPolicyCaseJson (c : Case) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"behavior\":" ++ surfaceViewJson c.behavior ++ ","
    ++ "\"ceiling\":" ++ surfaceViewJson c.ceiling ++ ","
    ++ "\"runtime\":" ++ surfaceViewJson c.runtime ++ ","
    ++ "\"expected\":" ++ surfaceViewJson c.expected
  ++ "}"

def toolPolicyCasesJson : String :=
  jsonArray (ToolPolicy.ContractCases.cases.map toolPolicyCaseJson)

structure GoalCapabilityResolutionCase where
  name : String
  explicitGoalTools : Option Bool
  explicitGoalCreate : Option Bool

def goalCapabilityResolutionCases : List GoalCapabilityResolutionCase :=
  [ ⟨"missing_goal_tools_is_off", none, none⟩
  , ⟨"explicit_goal_on", some true, none⟩
  , ⟨"explicit_goal_off", some false, none⟩
  , ⟨"creation_explicit_on", some true, some true⟩ ]

def optionalBoolJson : Option Bool → String
  | none => "null"
  | some value => boolJson value

def goalCapabilityResolutionCaseJson (c : GoalCapabilityResolutionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"explicit_goal_tools\":" ++ optionalBoolJson c.explicitGoalTools ++ ","
    ++ "\"explicit_goal_create\":" ++ optionalBoolJson c.explicitGoalCreate ++ ","
    ++ "\"expected_goal_tools\":"
      ++ boolJson (ToolPolicy.resolveGoalTools c.explicitGoalTools) ++ ","
    ++ "\"expected_goal_create\":"
      ++ boolJson (ToolPolicy.resolveGoalCreate c.explicitGoalCreate)
  ++ "}"

def goalCapabilityResolutionCasesJson : String :=
  jsonArray (goalCapabilityResolutionCases.map goalCapabilityResolutionCaseJson)

end Conformance.Contracts

namespace Conformance.Contracts
def applicationWriteCasesJson : String := Id.run do
  let mut rows := []
  for op in [ToolPolicy.ApplicationWrite.Operation.create, .update, .delete] do
    for granted in [false, true] do
      for application in [false, true] do
        for blocked in [false, true] do
          for credential in [false, true] do
            for bounded in [false, true] do
              for targets in [0, 1, 3, 101] do
                for limit in [0, 2, 100, 101] do
                  for preview in [false, true] do
                    for digestMatches in [false, true] do
                      let o : ToolPolicy.ApplicationWrite.Observation :=
                        ⟨granted, application, blocked, credential, bounded, targets, limit, preview, digestMatches⟩
                      let name := match op with | .create => "create" | .update => "update" | .delete => "delete"
                      rows := rows ++ ["{\"operation\":" ++ jsonString name
                        ++ ",\"granted\":" ++ boolJson granted ++ ",\"application\":" ++ boolJson application
                        ++ ",\"protected\":" ++ boolJson blocked ++ ",\"credential\":" ++ boolJson credential
                        ++ ",\"bounded\":" ++ boolJson bounded ++ ",\"targets\":" ++ toString targets
                        ++ ",\"limit\":" ++ toString limit ++ ",\"preview\":" ++ boolJson preview
                        ++ ",\"digest_matches\":" ++ boolJson digestMatches
                        ++ ",\"admitted\":" ++ boolJson (ToolPolicy.ApplicationWrite.admitted op o)
                        ++ ",\"may_apply\":" ++ boolJson (ToolPolicy.ApplicationWrite.mayApply op o) ++ "}"]
  return jsonArray rows
end Conformance.Contracts
