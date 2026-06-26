import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.ToolPolicy.Cases

/-!
# Tool Policy JSON

Serializer for unified tool-policy composition witnesses.
-/

namespace Conformance.Contracts

open ToolPolicy.ContractCases

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
    ++ "\"mcp_probe\":" ++ jsonString v.mcpProbe ++ ","
    ++ "\"mcp_permits\":" ++ (if v.mcpPermits then "true" else "false") ++ ","
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
