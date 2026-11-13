import Proofs.Clients.ClientShell.SessionTurn
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Family

namespace Conformance.ClientShellContracts.SessionTurnCases

open ClientShell.SessionTurn Conformance.Contracts

def row (doc request : Nat) (state : RowState) (requester : Nat := 1)
    (foldedInto : Option Nat := none) (queuedAfter retryParent : Option Nat := none)
    (retryParentDoc : Option Nat := none) (completed : Bool := false)
    (publicInput : Bool := true) (authoredInputMaterialized : Bool := false)
    (backgroundWake : Bool := false) (replacedBy : Option Nat := none) : Row :=
  { doc, request, requester, state, foldedInto, queuedAfter, retryParent,
    retryParentDoc, completed, publicInput, authoredInputMaterialized, backgroundWake, replacedBy }

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

def stateName : RowState → String
  | .unclaimed => "unclaimed"
  | .active    => "active"
  | .terminal  => "terminal"

def optNat : Option Nat → String
  | some n => toString n
  | none   => "null"

def rowJson (r : Row) : String :=
  "{\"doc\":" ++ toString r.doc ++
  ",\"request\":" ++ toString r.request ++
  ",\"requester\":" ++ toString r.requester ++
  ",\"state\":" ++ jsonString (stateName r.state) ++
  ",\"folded_into\":" ++ optNat r.foldedInto ++
  ",\"replaced_by\":" ++ optNat r.replacedBy ++
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

deriving instance Lean.ToJson for RowState, Row

instance : Inhabited Row := ⟨row 0 0 .terminal⟩

structure CaseInput where
  name : String
  rows : List Row
  selectedDoc : Nat
  pageRequestDocs : List Nat
  deriving Lean.ToJson

def pageCase (name : String) (rows : List Row) (pageRequestDocs : List Nat) : CaseInput :=
  { name, rows, selectedDoc := ((rows.getLast?).map (·.doc)).getD 0, pageRequestDocs }

def selectedCase (name : String) (rows : List Row) (selectedDoc : Nat) : CaseInput :=
  { name, rows, selectedDoc, pageRequestDocs := [] }

def sessionCases : List CaseInput :=
  let pageRows :=
    [ row 1 100 .terminal (completed := true) (authoredInputMaterialized := true)
    , row 2 101 .active ]
  scenarios.map (fun (name, rows) => pageCase name rows []) ++
  [ pageCase "page_request_is_retained" pageRows [1],
    selectedCase "stale_terminal_observation_yields_to_active_background_turn"
      [row 1 100 .terminal, row 2 101 .active (backgroundWake := true),
       row 3 102 .terminal (foldedInto := some 2)] 1,
    selectedCase "stale_terminal_observation_keeps_active_background_without_new_input"
      [row 1 100 .terminal, row 2 101 .active (backgroundWake := true)] 1,
    selectedCase "active_background_keeps_queued_user_messages_interruptible"
      [row 1 100 .terminal (completed := true) (authoredInputMaterialized := true),
       row 2 101 .active (backgroundWake := true),
       row 3 102 .unclaimed (queuedAfter := some 101)] 1,
    selectedCase "non_normal_selected_row_cannot_anchor_a_later_active_turn"
      [row 1 100 .terminal (publicInput := false), row 2 101 .active] 1,
    selectedCase "active_turn_does_not_cross_requester_scope"
      [row 1 100 .terminal, row 2 101 .active (requester := 2)] 1,
    selectedCase "active_observed_turn_is_preserved"
      [row 1 100 .active, row 2 101 .active] 1,
    selectedCase "unclaimed_observed_turn_is_preserved"
      [row 1 100 .unclaimed, row 2 101 .active] 1,
    selectedCase "title_audit_does_not_replace_a_terminal_observation"
      [row 1 100 .terminal, row 2 101 .active (publicInput := false)] 1,
    selectedCase "completed_background_does_not_restore_an_older_stopped_notice"
      [row 1 100 .terminal,
       row 2 101 .terminal (completed := true) (backgroundWake := true)
         (authoredInputMaterialized := true)] 1,
    selectedCase "completed_background_keeps_its_folded_input_owner"
      [row 1 100 .terminal,
       row 2 101 .terminal (completed := true) (backgroundWake := true)
         (authoredInputMaterialized := true),
       row 3 102 .terminal (foldedInto := some 2)] 1,
    selectedCase "replaced_queue_input_resolves_to_completed_fold_owner"
      [row 1 100 .terminal (completed := true) (authoredInputMaterialized := true),
       row 2 101 .terminal (replacedBy := some 3),
       row 3 102 .terminal (foldedInto := some 1)] 2,
    selectedCase "replaced_queue_input_resolves_to_active_fold_owner"
      [row 1 100 .active, row 2 101 .terminal (replacedBy := some 3),
       row 3 102 .terminal (foldedInto := some 1)] 2,
    selectedCase "edited_input_waits_without_becoming_folded"
      [row 1 100 .active, row 2 101 .terminal (replacedBy := some 3),
       row 3 102 .unclaimed (queuedAfter := some 100)] 2,
    selectedCase "replacement_missing_successor_does_not_guess"
      [row 1 100 .terminal (replacedBy := some 9)] 1,
    selectedCase "replacement_cannot_follow_foreign_requester"
      [row 1 100 .terminal (replacedBy := some 2),
       row 2 101 .active (requester := 2)] 1,
    selectedCase "replacement_cycle_is_bounded"
      [row 1 100 .terminal (replacedBy := some 2),
       row 2 101 .terminal (replacedBy := some 1)] 1,
    selectedCase "replacement_keeps_physical_successor_among_logical_twins"
      [row 1 100 .terminal (replacedBy := some 3),
       row 2 101 .active (requester := 2), row 3 101 .unclaimed] 1,
    selectedCase "replacement_sorted_after_successor_preserves_completed_turn"
      [row 1 100 .terminal (completed := true) (authoredInputMaterialized := true),
       row 3 102 .terminal (foldedInto := some 1),
       row 2 101 .terminal (replacedBy := some 3)] 2,
    selectedCase "replacement_retains_omittable_completed_successor"
      [row 1 100 .terminal (completed := true) (authoredInputMaterialized := true),
       row 2 101 .terminal (replacedBy := some 1)] 2,
    selectedCase "replacement_chain_preserves_completed_turn"
      [row 1 100 .terminal (completed := true) (authoredInputMaterialized := true),
       row 2 101 .terminal (replacedBy := some 3),
       row 3 102 .terminal (replacedBy := some 4),
       row 4 103 .terminal (foldedInto := some 1)] 2,
    selectedCase "older_active_row_does_not_revive_a_newer_terminal_turn"
      [row 1 100 .active, row 2 101 .terminal] 2]

def selectedRow (rows : List Row) (selectedDoc : Nat) : Row :=
  (rows.find? (fun row => row.doc == selectedDoc)).getD default

def readRows (rows : List Row) (selectedDoc : Nat) (pageRequestDocs : List Nat) : List Row :=
  let selected := selectedRow rows selectedDoc
  requestReadRows rows selected.requester selected.doc selected.doc pageRequestDocs

def readSeeds (rows : List Row) (selectedDoc : Nat) (pageRequestDocs : List Nat) : List Row :=
  let selected := selectedRow rows selectedDoc
  requestReadSeeds rows selected.requester selected.doc selected.doc pageRequestDocs

structure SelectionOutput where
  displayTurnDoc : Nat
  turnDoc : Nat
  answerDoc : Nat
  deriving Lean.ToJson

@[conformance_family]
def selection : Family CaseInput SelectionOutput where
  inputs := sessionCases
  model := fun i =>
    let selected := selectedRow i.rows i.selectedDoc
    { displayTurnDoc := (displayTurn i.rows selected).doc
      turnDoc := (turnOf i.rows selected).doc
      answerDoc := (answerOf i.rows selected).doc }

/-- The native seed query reads completed public rows only through the
publication evidence `nativeCompletionPremise` names; a case without it binds
the general read-closure owner from the modeled seeds instead
(`Conformance.ClientRequestClosure.closed`). -/
def nativeReadCases : List CaseInput :=
  sessionCases.filter (fun i => nativeCompletionPremise i.rows)

def seededReadCases : List CaseInput :=
  sessionCases.filter (fun i => !nativeCompletionPremise i.rows)

example : seededReadCases.map (·.name) = ["completed_without_authored_input_is_retained"] := by
  native_decide

structure ReadOutput where
  readDocs : List Nat
  deriving Lean.ToJson

@[conformance_family]
def read : Family CaseInput ReadOutput where
  inputs := nativeReadCases
  model := fun i => { readDocs := (readRows i.rows i.selectedDoc i.pageRequestDocs).map (·.doc) }

structure ProjectionInput where
  name : String
  rows : List Row
  selectedDoc : Nat
  pageRequestDocs : List Nat
  /-- The modeled read closure (`requestReadRows`): the session projection built
  from only these rows must equal the projection built from every row. -/
  readDocs : List Nat
  deriving Lean.ToJson

structure ProjectionOutput where
  displayTurnDoc : Nat
  queuedDocs : List Nat
  foldedRequests : List Nat
  deriving Lean.ToJson

/-- A session snapshot is built for its observed latest request, and that
observation names a normal-purpose request: a title-audit row never anchors a
session observation, so the snapshot projection replays the cases whose
selected row is public input. -/
def projectionCases : List ProjectionInput :=
  (sessionCases.filter (fun i => i.rows.any (fun row => row.doc == i.selectedDoc && row.publicInput))).map
    (fun i => { name := i.name, rows := i.rows, selectedDoc := i.selectedDoc,
                pageRequestDocs := i.pageRequestDocs,
                readDocs := (readRows i.rows i.selectedDoc i.pageRequestDocs).map (·.doc) })

example : projectionCases.all (fun c =>
    let requester := (selectedRow c.rows c.selectedDoc).requester
    c.rows.all (fun row => row.requester != requester || !row.publicInput ||
      c.readDocs.contains row.doc || mayOmitCompletedPublicInput row)) := by
  native_decide

example : projectionCases.all (fun c =>
    (c.rows.filter (fun row => c.readDocs.contains row.doc)).length == c.readDocs.length) := by
  native_decide

@[conformance_family]
def projection : Family ProjectionInput ProjectionOutput where
  inputs := projectionCases
  model := fun i =>
    let selected := selectedRow i.rows i.selectedDoc
    let display := displayTurn i.rows selected
    { displayTurnDoc := display.doc
      queuedDocs := (queuedBehind i.rows display).map (·.doc)
      foldedRequests := (foldedIn i.rows selected.requester).map (·.request) }

end Conformance.ClientShellContracts.SessionTurnCases
