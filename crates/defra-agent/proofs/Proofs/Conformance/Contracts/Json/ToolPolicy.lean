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

def surfaceViewJson (v : SurfaceView) : String :=
  "{"
    ++ "\"file_rank\":" ++ toString v.fileRank ++ ","
    ++ "\"meta\":" ++ (if v.meta then "true" else "false") ++ ","
    ++ "\"defra_query\":" ++ (if v.defraQuery then "true" else "false") ++ ","
    ++ "\"spawn\":" ++ (if v.spawn then "true" else "false") ++ ","
    ++ "\"bash_mode\":" ++ toString v.bashMode ++ ","
    ++ "\"bash_net\":" ++ toString v.bashNet ++ ","
    ++ "\"bash_sandbox\":" ++ (if v.bashSandbox then "true" else "false") ++ ","
    ++ "\"bash_allowed_kind\":" ++ jsonString v.bashAllowedKind ++ ","
    ++ "\"bash_allowed_prefixes\":" ++ jsonStringMatrix v.bashAllowedPrefixes ++ ","
    ++ "\"mcp_probe\":" ++ jsonString v.mcpProbe ++ ","
    ++ "\"mcp_scope_kind\":" ++ jsonString v.mcpScopeKind ++ ","
    ++ "\"mcp_services\":" ++ jsonStringArray v.mcpServices ++ ","
    ++ "\"mcp_permits\":" ++ (if v.mcpPermits then "true" else "false") ++ ","
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
