import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.ToolPolicy.Cases

/-!
# Tool Policy JSON

Serializer for unified tool-policy composition witnesses.
-/

namespace Conformance.Contracts

open ToolPolicy.ContractCases

def writeGrantViewJson (grant : WriteGrantView) : String :=
  "{"
    ++ "\"tool\":" ++ jsonString grant.tool ++ ","
    ++ "\"collection\":" ++ jsonString grant.collection ++ ","
    ++ "\"fields\":" ++ jsonStringArray grant.fields
  ++ "}"

def boolJson (b : Bool) : String := if b then "true" else "false"

def surfaceViewJson (v : SurfaceView) : String :=
  "{"
    ++ "\"file_rank\":" ++ toString v.fileRank ++ ","
    ++ "\"meta\":" ++ boolJson v.meta ++ ","
    ++ "\"defra_query\":" ++ boolJson v.defraQuery ++ ","
    ++ "\"memory\":" ++ boolJson v.memory ++ ","
    ++ "\"session_history\":" ++ boolJson v.sessionHistory ++ ","
    ++ "\"context_budget\":" ++ boolJson v.contextBudget ++ ","
    ++ "\"spawn\":" ++ boolJson v.spawn ++ ","
    ++ "\"steering\":" ++ boolJson v.steering ++ ","
    ++ "\"background\":" ++ boolJson v.background ++ ","
    ++ "\"orchestration\":" ++ boolJson v.orchestration ++ ","
    ++ "\"cross_deployment\":" ++ boolJson v.crossDeployment ++ ","
    ++ "\"skills\":" ++ boolJson v.skills ++ ","
    ++ "\"bash_mode\":" ++ toString v.bashMode ++ ","
    ++ "\"bash_net\":" ++ toString v.bashNet ++ ","
    ++ "\"bash_sandbox\":" ++ boolJson v.bashSandbox ++ ","
    ++ "\"bash_allowed_kind\":" ++ jsonString v.bashAllowedKind ++ ","
    ++ "\"bash_allowed_prefixes\":" ++ jsonStringMatrix v.bashAllowedPrefixes ++ ","
    ++ "\"cli_scope_kind\":" ++ jsonString v.cliScopeKind ++ ","
    ++ "\"cli_keys\":" ++ jsonStringArray v.cliKeys ++ ","
    ++ "\"mcp_probe\":" ++ jsonString v.mcpProbe ++ ","
    ++ "\"mcp_scope_kind\":" ++ jsonString v.mcpScopeKind ++ ","
    ++ "\"mcp_services\":" ++ jsonStringArray v.mcpServices ++ ","
    ++ "\"mcp_permits\":" ++ boolJson v.mcpPermits ++ ","
    ++ "\"defra_collections_scope_kind\":" ++ jsonString v.defraCollectionsScopeKind ++ ","
    ++ "\"defra_collections_keys\":" ++ jsonStringArray v.defraCollectionsKeys ++ ","
    ++ "\"subagent_targets_scope_kind\":" ++ jsonString v.subagentTargetsScopeKind ++ ","
    ++ "\"subagent_targets_keys\":" ++ jsonStringArray v.subagentTargetsKeys ++ ","
    ++ "\"background_tools_scope_kind\":" ++ jsonString v.backgroundToolsScopeKind ++ ","
    ++ "\"background_tools_keys\":" ++ jsonStringArray v.backgroundToolsKeys ++ ","
    ++ "\"write_probe_tool\":" ++ jsonString v.writeProbe.1 ++ ","
    ++ "\"write_probe_collection\":" ++ jsonString v.writeProbe.2 ++ ","
    ++ "\"write_scope_kind\":" ++ jsonString v.writeScopeKind ++ ","
    ++ "\"write_grants\":"
      ++ jsonArray (v.writeGrants.map writeGrantViewJson) ++ ","
    ++ "\"write_fields\":" ++ jsonArray (v.writeFields.map jsonString)
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

end Conformance.Contracts
