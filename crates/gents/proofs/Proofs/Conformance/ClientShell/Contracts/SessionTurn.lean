import Proofs.ClientShell.SessionTurn
import Proofs.Conformance.ContractTypes

namespace Conformance.ClientShellContracts.SessionTurnCases

open ClientShell.SessionTurn Conformance.Contracts

def row (doc request : Nat) (state : RowState) (requester : Nat := 1)
    (foldedInto : Option Nat := none) (queuedAfter retryParent : Option Nat := none) : Row :=
  { doc, request, requester, state, foldedInto, queuedAfter, retryParent }

/-- Each scenario lists rows in arrival order; the last row is the newest. -/
def scenarios : List (String × List Row) :=
  [ ("queued_behind_running_turn",
      [ row 1 100 .active
      , row 2 101 .unclaimed (queuedAfter := some 100)
      , row 3 102 .unclaimed (queuedAfter := some 101) ])
  , ("terminal_predecessor_releases_oldest_queued",
      [ row 1 100 .terminal
      , row 2 101 .unclaimed (queuedAfter := some 100)
      , row 3 102 .unclaimed (queuedAfter := some 101) ])
  , ("queued_behind_folded_row_waits_for_its_owner",
      [ row 1 100 .active
      , row 2 101 .terminal (foldedInto := some 1) (queuedAfter := some 99)
      , row 3 102 .unclaimed (queuedAfter := some 101) ])
  , ("multiple_queued_behind_folded_row",
      [ row 1 100 .active
      , row 2 101 .terminal (foldedInto := some 1) (queuedAfter := some 99)
      , row 3 102 .unclaimed (queuedAfter := some 101)
      , row 4 103 .unclaimed (queuedAfter := some 102) ])
  , ("newest_folded_row_resolves_to_its_owner",
      [ row 1 100 .active (queuedAfter := some 99)
      , row 2 101 .terminal (foldedInto := some 1) (queuedAfter := some 100)
      , row 3 102 .terminal (foldedInto := some 1) (queuedAfter := some 101) ])
  , ("folded_owner_finished_releases_queued",
      [ row 1 100 .terminal
      , row 2 101 .terminal (foldedInto := some 1) (queuedAfter := some 99)
      , row 3 102 .unclaimed (queuedAfter := some 101) ])
  , ("queued_behind_retried_head_waits_for_retry",
      [ row 1 100 .terminal
      , row 2 101 .unclaimed (queuedAfter := some 100)
      , row 3 102 .active (retryParent := some 100) ])
  , ("queued_behind_finished_retry_is_the_turn",
      [ row 1 100 .terminal
      , row 2 101 .unclaimed (queuedAfter := some 100)
      , row 3 102 .terminal (retryParent := some 100)
      , row 4 103 .unclaimed (queuedAfter := some 101) ])
  , ("selected_unpublished_message_waits_behind_running_head",
      [ row 1 100 .terminal
      , row 2 101 .active (queuedAfter := some 100)
      , row 3 102 .unclaimed (queuedAfter := some 101) ])
  , ("selected_unpublished_message_returns_when_head_fails",
      [ row 1 100 .terminal
      , row 2 101 .terminal (queuedAfter := some 100)
      , row 3 102 .unclaimed (queuedAfter := some 101)
      , row 4 103 .unclaimed (queuedAfter := some 102) ])
  , ("folded_into_retried_head_resolves_to_its_retry",
      [ row 1 100 .terminal
      , row 2 102 .active (retryParent := some 100)
      , row 3 101 .terminal (foldedInto := some 1) (queuedAfter := some 100) ])
  , ("other_requester_predecessor_is_not_followed",
      [ row 1 100 .active (requester := 2)
      , row 2 101 .unclaimed (queuedAfter := some 100) ])
  , ("other_requester_fold_owner_is_not_followed",
      [ row 1 100 .active (requester := 2)
      , row 2 101 .terminal (foldedInto := some 1) ]) ]

def newest (rows : List Row) : Option Row := rows.getLast?

def stateName : RowState → String
  | .unclaimed => "unclaimed"
  | .active    => "active"
  | .terminal  => "terminal"

def optNat : Option Nat → String
  | some n => toString n
  | none   => "null"

def natList (values : List Nat) : String :=
  "[" ++ String.intercalate "," (values.map toString) ++ "]"

def rowJson (r : Row) : String :=
  "{\"doc\":" ++ toString r.doc ++
  ",\"request\":" ++ toString r.request ++
  ",\"requester\":" ++ toString r.requester ++
  ",\"state\":" ++ jsonString (stateName r.state) ++
  ",\"folded_into\":" ++ optNat r.foldedInto ++
  ",\"queued_after\":" ++ optNat r.queuedAfter ++
  ",\"retry_parent\":" ++ optNat r.retryParent ++ "}"

def rowsJson (rows : List Row) : String :=
  "[" ++ String.intercalate "," (rows.map rowJson) ++ "]"

def caseJson (entry : String × List Row) : String :=
  match newest entry.2 with
  | none => "null"
  | some last =>
    let turn := turnOf entry.2 last
    "{\"name\":" ++ jsonString entry.1 ++
    ",\"rows\":" ++ rowsJson entry.2 ++
    ",\"expected_turn_doc\":" ++ toString turn.doc ++
    ",\"expected_queued_docs\":" ++ natList ((queuedBehind entry.2 turn).map (·.doc)) ++
    ",\"expected_folded_requests\":" ++
      natList ((foldedIn entry.2 last.requester).map (·.request)) ++ "}"

def casesJson : String :=
  "[" ++ String.intercalate "," (scenarios.map caseJson) ++ "]"

def caseCount : Nat := scenarios.length

end Conformance.ClientShellContracts.SessionTurnCases
