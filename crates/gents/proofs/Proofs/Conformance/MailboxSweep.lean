import Proofs.Automation.Mailbox.Sweep
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Family

namespace Conformance.MailboxSweepContracts
open Mailbox.Sweep Conformance.Contracts

structure PoisonRowInput where
  due : Bool
  undecodable : Bool
  hostile : Bool
  deriving Lean.ToJson

structure PoisonInput where
  rows : List PoisonRowInput
  deriving Lean.ToJson

structure PoisonOutput where
  skippedErrors : Nat
  expired : Nat
  openRows : List Bool
  expiredRows : List Bool
  deriving Lean.ToJson

def poisonState (rows : List PoisonRowInput) : Mailbox.Sweep.State :=
  { rows := ((List.range rows.length).zip rows).map fun (index, row) =>
      (⟨index + 1, "local", true, row.due, row.undecodable || row.hostile⟩ : Mailbox.Sweep.Row),
    cursor := none }

def poisonOutcome (input : PoisonInput) : PoisonOutput :=
  let state := poisonState input.rows
  { skippedErrors := (skipped state "local" 16 16).length
    expired := (expiredIn state "local" 16 16).length
    openRows := (advance state "local" 16 16).rows.map Row.isOpen
    expiredRows := state.rows.map fun row =>
      (expiredIn state "local" 16 16).any fun expiredRow => expiredRow.docId == row.docId }

@[conformance_family]
def poisonedRows : Family PoisonInput PoisonOutput where
  inputs :=
    [ { rows := [{ due := true, undecodable := false, hostile := false }] },
      { rows := [{ due := false, undecodable := true, hostile := false }] },
      { rows := [{ due := false, undecodable := false, hostile := true }] },
      { rows := [{ due := false, undecodable := false, hostile := true },
        { due := false, undecodable := true, hostile := false },
        { due := true, undecodable := false, hostile := false }] },
      { rows := [{ due := true, undecodable := false, hostile := false },
        { due := false, undecodable := false, hostile := true },
        { due := true, undecodable := false, hostile := false }] },
      { rows := [{ due := false, undecodable := false, hostile := false },
        { due := true, undecodable := false, hostile := false }] } ]
  model := poisonOutcome

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
    ⟨docId, "local", true, docId % 4 != 0 && docId != 35, false⟩) ++
    [⟨36, "foreign", true, true, false⟩]

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
