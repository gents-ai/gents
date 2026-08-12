import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Lsp.Cases

namespace Conformance.Contracts

open Lsp.ContractCases

def lspActionCaseJson (c : Case) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"lsp\":" ++ (if c.lsp then "true" else "false") ++ ","
    ++ "\"file_rank\":" ++ toString c.fileRank ++ ","
    ++ "\"action\":" ++ jsonString c.action ++ ","
    ++ "\"mutates\":" ++ (if c.mutates then "true" else "false") ++ ","
    ++ "\"source\":" ++ jsonString c.source ++ ","
    ++ "\"advertised\":" ++ (if c.advertised then "true" else "false") ++ ","
    ++ "\"action_authorized\":" ++ (if c.actionAuthorized then "true" else "false") ++ ","
    ++ "\"apply_authorized\":" ++ (if c.applyAuthorized then "true" else "false")
  ++ "}"

def lspActionCasesJson : String :=
  jsonArray (Lsp.ContractCases.cases.map lspActionCaseJson)

end Conformance.Contracts
