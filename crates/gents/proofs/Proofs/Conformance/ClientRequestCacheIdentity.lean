import Proofs.ClientShell.RequestCacheIdentity

namespace Conformance.ClientRequestCacheIdentity

open ClientShell.RequestCacheIdentity
open ClientShell.SessionTurn

def stateName : RowState → String
  | .unclaimed => "unclaimed"
  | .active => "active"
  | .terminal => "terminal"

def optionNatJson : Option Nat → String
  | none => "null"
  | some value => toString value

def rowJson (row : ClientShell.RequestCacheIdentity.Row) : String :=
  "{\"doc\":" ++ optionNatJson row.doc ++
  ",\"node\":" ++ optionNatJson row.node ++
  ",\"requester\":" ++ optionNatJson row.requester ++
  ",\"request\":" ++ toString row.request ++
  ",\"state\":\"" ++ stateName row.state ++ "\"" ++
  ",\"folded_into\":" ++ optionNatJson row.foldedInto ++
  ",\"queued_after\":" ++ optionNatJson row.queuedAfter ++
  ",\"retry_parent\":" ++ optionNatJson row.retryParent ++ "}"

def rowsJson (rows : List ClientShell.RequestCacheIdentity.Row) : String :=
  "[" ++ String.intercalate "," (rows.map rowJson) ++ "]"

def caseJson (entry : String × List ClientShell.RequestCacheIdentity.Row ×
    List ClientShell.RequestCacheIdentity.Row × Option Nat) : String :=
  let (name, current, incoming, selectedDoc) := entry
  let expected := mergeRows current incoming
  let turnDoc := selectedDoc.bind (turnDocument expected)
  "{\"name\":\"" ++ name ++ "\",\"current\":" ++ rowsJson current ++
  ",\"incoming\":" ++ rowsJson incoming ++
  ",\"expected_rows\":" ++ rowsJson expected ++
  ",\"selected_doc\":" ++ optionNatJson selectedDoc ++
  ",\"expected_turn_doc\":" ++ optionNatJson turnDoc ++ "}"

def casesJson : String :=
  "[" ++ String.intercalate "," (identityCases.map caseJson) ++ "]"

end Conformance.ClientRequestCacheIdentity

def main : IO Unit := IO.println Conformance.ClientRequestCacheIdentity.casesJson
