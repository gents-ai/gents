import Proofs.Mailbox.Sweep
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.MailboxSweepContracts
open Mailbox.Sweep Conformance.Contracts

private def rowJson (row : Row) : String :=
  "{\"doc_id\":" ++ toString row.docId ++
  ",\"node_did\":" ++ jsonString row.nodeDid ++
  ",\"open\":" ++ (if row.isOpen then "true" else "false") ++
  ",\"due\":" ++ (if row.due then "true" else "false") ++ "}"

private def rowsJson (rows : List Row) : String := jsonArray (rows.map rowJson)

private def stepJson (state : State) (nodeDid : String) (pageSize budget : Nat) : String :=
  let batch := selected state nodeDid pageSize budget
  let post := advance state nodeDid pageSize budget
  "{\"cursor\":" ++ jsonOptionalNat state.cursor ++
    ",\"rows\":" ++ rowsJson state.rows ++
    ",\"selected\":" ++ jsonArray (batch.map (toString ∘ Row.docId)) ++
    ",\"next_cursor\":" ++ jsonOptionalNat post.cursor ++
    ",\"post_rows\":" ++ rowsJson post.rows ++ "}"

private def progressInputs : List Row :=
  (List.range 35).map (fun index =>
    let docId := index + 1
    ⟨docId, "local", true, docId % 4 != 0 && docId != 35⟩) ++
    [⟨36, "foreign", true, true⟩]

private def progressState : State := { rows := progressInputs, cursor := none }
private def firstState : State := advance progressState "local" 16 16
private def secondState : State := advance firstState "local" 16 16
private def wrappedState : State := advance secondState "local" 16 16

private def progressCaseJson : String :=
  "{\"case\":\"close_while_keyset_advances\",\"node_did\":\"local\",\"page_size\":16,\"budget\":16,\"actions\":[" ++
    stepJson progressState "local" 16 16 ++ "," ++
    stepJson firstState "local" 16 16 ++ "," ++
    stepJson secondState "local" 16 16 ++ "," ++
    stepJson wrappedState "local" 16 16 ++ "]}"

def casesJson : String := jsonArray [progressCaseJson]

end Conformance.MailboxSweepContracts
