import Proofs.ClientShell.SessionTurn
import Proofs.Conformance.ContractTypes

namespace Conformance.ClientShellContracts.SessionTurnCases

open ClientShell.SessionTurn Conformance.Contracts

def row (doc request : Nat) (state : RowState) (requester : Nat := 1)
    (foldedInto : Option Nat := none) (queuedAfter retryParent : Option Nat := none)
    (retryParentDoc : Option Nat := none) (completed : Bool := false)
    (publicInput : Bool := true) (authoredInputMaterialized : Bool := false)
    (backgroundWake : Bool := false) : Row :=
  { doc, request, requester, state, foldedInto, queuedAfter, retryParent,
    retryParentDoc, completed, publicInput, authoredInputMaterialized, backgroundWake }

def scenarios : List (String × List Row) :=
  [ ("retry_waiting_behind_another_turn_has_no_answer_yet",
      [ row 1 100 .terminal
      , row 2 200 .active
      , row 3 101 .unclaimed (queuedAfter := some 200) (retryParent := some 100) ])
  , ("folded_answer_waits_for_its_queued_retry",
      [ row 1 100 .terminal
      , row 2 200 .active
      , row 3 101 .unclaimed (queuedAfter := some 200) (retryParent := some 100)
      , row 4 102 .terminal (foldedInto := some 1) ])
  , ("queued_behind_running_turn",
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
  ++ [("bounded_read_discards_unrelated_terminal_history",
      (List.range 96).map (fun index =>
        row (100 + index) (1_000 + index) .terminal (completed := true)
          (authoredInputMaterialized := true)) ++
      [ row 1 100 .active
      , row 2 101 .unclaimed (queuedAfter := some 100) ])]
  ++ [ ("cycle_resolution_stops_at_first_repeated_physical_row",
        [row 1 100 .unclaimed (queuedAfter := some 101),
         row 2 101 .unclaimed (queuedAfter := some 100)])
     , ("cycle_resolution_ignores_unrelated_terminal_history",
        (List.range 96).map (fun index =>
          row (100 + index) (1_000 + index) .terminal (completed := true)
            (authoredInputMaterialized := true)) ++
        [row 1 100 .unclaimed (queuedAfter := some 101),
         row 2 101 .unclaimed (queuedAfter := some 100)])
     , ("physical_retry_parent_is_retained_for_prompt_ownership",
        [ row 1 100 .terminal (completed := true) (authoredInputMaterialized := true)
        , row 2 101 .active (retryParent := some 100) (retryParentDoc := some 1) ])
     , ("completed_without_authored_input_is_retained",
        [ row 1 100 .terminal (completed := true)
            (authoredInputMaterialized := false)
        , row 2 101 .active ])
     , ("logical_twins_are_all_retained_in_source_order",
        [ row 1 100 .terminal
        , row 2 100 .active
        , row 3 200 .unclaimed (queuedAfter := some 100) ])]

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
  ",\"retry_parent\":" ++ optNat r.retryParent ++
  ",\"retry_parent_doc\":" ++ optNat r.retryParentDoc ++
  ",\"completed\":" ++ (if r.completed then "true" else "false") ++
  ",\"public_input\":" ++ (if r.publicInput then "true" else "false") ++
  ",\"background_wake\":" ++ (if r.backgroundWake then "true" else "false") ++
  ",\"authored_input_materialized\":" ++
    (if r.authoredInputMaterialized then "true" else "false") ++ "}"

def rowsJson (rows : List Row) : String :=
  "[" ++ String.intercalate "," (rows.map rowJson) ++ "]"

def caseJsonWithSelection (name : String) (rows : List Row) (pageRequestDocs : List Nat)
    (selectedDoc : Option Nat := none) : String :=
  match newest rows with
  | none => "null"
  | some last =>
    let selected := (selectedDoc.bind (fun doc => rows.find? (fun row => row.doc == doc))).getD last
    let turn := turnOf rows selected
    let display := displayTurn rows selected
    let readRows := requestReadRows rows selected.requester selected.doc selected.doc pageRequestDocs
    "{\"name\":" ++ jsonString name ++
    ",\"rows\":" ++ rowsJson rows ++
    ",\"selected_doc\":" ++ toString selected.doc ++
    ",\"page_request_docs\":" ++ natList pageRequestDocs ++
    ",\"seed_docs\":" ++ natList ((requestReadSeeds rows selected.requester selected.doc selected.doc pageRequestDocs).map (·.doc)) ++
    ",\"native_completion_premise\":" ++ (if nativeCompletionPremise rows then "true" else "false") ++
    ",\"expected_display_turn_doc\":" ++ toString display.doc ++
    ",\"expected_read_docs\":" ++ natList (readRows.map (·.doc)) ++
    ",\"expected_turn_doc\":" ++ toString turn.doc ++
    ",\"expected_answer_doc\":" ++ toString (answerOf rows selected).doc ++
    ",\"expected_queued_docs\":" ++ natList ((queuedBehind rows display).map (·.doc)) ++
    ",\"expected_folded_requests\":" ++
      natList ((foldedIn rows selected.requester).map (·.request)) ++ "}"

def caseJsonWithPage (name : String) (rows : List Row) (pageRequestDocs : List Nat) : String :=
  caseJsonWithSelection name rows pageRequestDocs

def caseJson (entry : String × List Row) : String :=
  caseJsonWithPage entry.1 entry.2 []

def casesJson : String :=
  let pageRows :=
    [ row 1 100 .terminal (completed := true) (authoredInputMaterialized := true)
    , row 2 101 .active ]
  "[" ++ String.intercalate "," (scenarios.map caseJson ++
    [caseJsonWithPage "page_request_is_retained" pageRows [1],
     caseJsonWithSelection "stale_terminal_observation_yields_to_active_background_turn"
       [row 1 100 .terminal, row 2 101 .active (backgroundWake := true),
        row 3 102 .terminal (foldedInto := some 2)] [] (some 1),
     caseJsonWithSelection "stale_terminal_observation_keeps_active_background_without_new_input"
       [row 1 100 .terminal, row 2 101 .active (backgroundWake := true)] [] (some 1),
     caseJsonWithSelection "active_turn_does_not_cross_requester_scope"
       [row 1 100 .terminal, row 2 101 .active (requester := 2)] [] (some 1),
     caseJsonWithSelection "active_observed_turn_is_preserved"
       [row 1 100 .active, row 2 101 .active] [] (some 1),
     caseJsonWithSelection "unclaimed_observed_turn_is_preserved"
       [row 1 100 .unclaimed, row 2 101 .active] [] (some 1),
     caseJsonWithSelection "title_audit_does_not_replace_a_terminal_observation"
       [row 1 100 .terminal, row 2 101 .active (publicInput := false)] [] (some 1),
     caseJsonWithSelection "completed_background_does_not_restore_an_older_stopped_notice"
       [row 1 100 .terminal,
        row 2 101 .terminal (completed := true) (backgroundWake := true)
          (authoredInputMaterialized := true)] [] (some 1),
     caseJsonWithSelection "completed_background_keeps_its_folded_input_owner"
       [row 1 100 .terminal,
        row 2 101 .terminal (completed := true) (backgroundWake := true)
          (authoredInputMaterialized := true),
        row 3 102 .terminal (foldedInto := some 2)] [] (some 1),
     caseJsonWithSelection "older_active_row_does_not_revive_a_newer_terminal_turn"
       [row 1 100 .active, row 2 101 .terminal] [] (some 2)]) ++ "]"

def caseCount : Nat := scenarios.length + 10

end Conformance.ClientShellContracts.SessionTurnCases
