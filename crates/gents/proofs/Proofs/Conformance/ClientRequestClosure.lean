import Proofs.Conformance.ClientShell.Contracts.SessionTurn

namespace Conformance.ClientRequestClosure

open ClientShell.SessionTurn
open Conformance.ClientShellContracts.SessionTurnCases
open Conformance.Contracts

def caseJson (entry : String × List Row) : String :=
  let (name, rows) := entry
  let seeds := rows.getLast?.toList
  let closed := closeReadRows rows (rows.length + 1) seeds
  "{\"name\":" ++ jsonString name ++ ",\"rows\":" ++ rowsJson rows ++
  ",\"seed_docs\":" ++ natList (seeds.map (·.doc)) ++
  ",\"expected_docs\":" ++ natList (closed.map (·.doc)) ++ "}"

def casesJson : String :=
  "[" ++ String.intercalate "," (scenarios.map caseJson) ++ "]"

end Conformance.ClientRequestClosure

def main : IO Unit := IO.println Conformance.ClientRequestClosure.casesJson
