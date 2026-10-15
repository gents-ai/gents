import Proofs.Conformance.ClientShell.Contracts.SessionTurn

namespace Conformance.SessionRequestRefresh

open ClientShell.SessionTurn Conformance.ClientShellContracts.SessionTurnCases

private def history (count : Nat) : List Row :=
  (List.range count).map (fun index => row (10000 + index) (10000 + index) .terminal)

private def caseJson (entry : String × List Row) (historyCount : Nat) : String :=
  let allRows := history historyCount ++ entry.2
  match entry.2.getLast? with
  | none => "null"
  | some newest =>
    let turn := turnOf allRows newest
    "{\"name\":" ++ Conformance.Contracts.jsonString entry.1 ++
    ",\"rows\":" ++ rowsJson entry.2 ++
    ",\"history\":{\"count\":" ++ toString historyCount ++
      ",\"doc_start\":10000,\"request_start\":10000,\"requester\":1,\"state\":\"terminal\"}" ++
    ",\"expected_turn_doc\":" ++ toString turn.doc ++
    ",\"expected_queued_docs\":" ++ natList ((queuedBehind allRows turn).map (·.doc)) ++
    ",\"expected_folded_requests\":" ++ natList ((foldedIn allRows newest.requester).map (·.request)) ++ "}"

def casesJson : String :=
  "[" ++ String.intercalate "," (scenarios.zipIdx.map (fun (entry, index) =>
    caseJson entry (if index == 0 then 5000 else 0))) ++ "]"

end Conformance.SessionRequestRefresh

def main : IO Unit := IO.println Conformance.SessionRequestRefresh.casesJson
