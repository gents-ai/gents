import Proofs.ClientShell.SessionTurn

/-!
The desktop cache updates persisted AgentRequest documents by physical
document identity. `request_id` names the logical predecessor/retry relation;
it does not identify a physical row. Rows without `_docID` are optimistic
placeholders and use their known logical scope until a persisted row arrives.
-/
namespace ClientShell.RequestCacheIdentity

open ClientShell.SessionTurn

structure Row where
  doc         : Option Nat
  node        : Option Nat
  requester   : Option Nat
  request     : Nat
  state       : RowState
  foldedInto  : Option Nat
  queuedAfter : Option Nat
  retryParent : Option Nat
  deriving DecidableEq, Repr

def incomingRefinesScope (current incoming : Option Nat) : Bool :=
  current.isNone || current == incoming

def sameLogicalScope (left right : Row) : Bool :=
  left.request == right.request
    && incomingRefinesScope left.node right.node
    && incomingRefinesScope left.requester right.requester

def sameIdentity (left right : Row) : Bool :=
  match left.doc, right.doc with
  | some leftDoc, some rightDoc => leftDoc == rightDoc
  | none, none =>
      left.request == right.request
        && left.node == right.node
        && left.requester == right.requester
  | _, _ => false

def upsert (rows : List Row) (incoming : Row) : List Row :=
  match rows with
  | [] => [incoming]
  | row :: rest =>
      if sameIdentity row incoming then
        incoming :: rest
      else
        row :: upsert rest incoming

def retirePlaceholder (row : Row) (incoming : List Row) : Bool :=
  row.doc.isNone && incoming.any fun persisted =>
    (persisted.doc.isSome || persisted.node.isSome) && sameLogicalScope row persisted

def mergeRows (current incoming : List Row) : List Row :=
  let retained := current.filter fun row => !(retirePlaceholder row incoming)
  incoming.foldl upsert retained

def turnRows (rows : List Row) : List ClientShell.SessionTurn.Row :=
  rows.filterMap fun row => row.doc.map fun doc =>
    { doc := doc
      request := row.request
      requester := row.requester.getD 0
      state := row.state
      foldedInto := row.foldedInto
      queuedAfter := row.queuedAfter
      retryParent := row.retryParent
      retryParentDoc := none }

def turnDocument (rows : List Row) (selectedDoc : Nat) : Option Nat :=
  match rows.find? (fun row => row.doc == some selectedDoc) with
  | none => none
  | some selected =>
      let requests := turnRows (rows.filter fun row => row.node == selected.node)
      match requests.find? (fun row => row.doc == selectedDoc) with
      | none => none
      | some selected => some (ClientShell.SessionTurn.turnOf requests selected).doc

def physicalTwinFoldRows : List Row :=
  [ { doc := some 1, node := some 7, requester := some 9, request := 100
      state := .terminal, foldedInto := some 3, queuedAfter := none, retryParent := none }
  , { doc := some 3, node := some 7, requester := some 9, request := 200
      state := .active, foldedInto := none, queuedAfter := none, retryParent := none }
  ]

def physicalTwinFoldIncoming : List Row :=
  [ { doc := some 2, node := some 7, requester := some 9, request := 200
      state := .active, foldedInto := none, queuedAfter := none, retryParent := none }
  ]

def identityCases : List (String × List Row × List Row × Option Nat) :=
  [ ("physical_twins_preserve_exact_fold_owner",
      physicalTwinFoldRows, physicalTwinFoldIncoming, some 1)
  , ("foreign_node_is_not_a_fold_owner",
      [{ doc := some 1, node := some 7, requester := some 9, request := 100
         state := .terminal, foldedInto := some 3, queuedAfter := none, retryParent := none }],
      [{ doc := some 3, node := some 8, requester := some 9, request := 200
         state := .active, foldedInto := none, queuedAfter := none, retryParent := none }], some 1)
  , ("same_physical_document_updates_in_place",
      [ { doc := some 4, node := some 7, requester := some 9, request := 300
          state := .unclaimed, foldedInto := none, queuedAfter := none, retryParent := none } ],
      [ { doc := some 4, node := some 7, requester := some 9, request := 300
          state := .active, foldedInto := none, queuedAfter := none, retryParent := none } ], none)
  , ("persisted_row_retires_scoped_optimistic_placeholder",
      [ { doc := none, node := some 7, requester := some 9, request := 200
          state := .unclaimed, foldedInto := none, queuedAfter := none, retryParent := none } ],
      physicalTwinFoldIncoming, none)
  , ("scoped_row_retires_compatible_unscoped_placeholder",
      [ { doc := none, node := none, requester := none, request := 200
          state := .unclaimed, foldedInto := none, queuedAfter := none, retryParent := none } ],
      [ { doc := none, node := some 7, requester := some 9, request := 200
          state := .active, foldedInto := none, queuedAfter := none, retryParent := none } ], none)
  , ("different_requester_placeholder_is_preserved",
      [ { doc := none, node := some 7, requester := some 8, request := 200
          state := .unclaimed, foldedInto := none, queuedAfter := none, retryParent := none } ],
      physicalTwinFoldIncoming, none)
  , ("unknown_incoming_scope_does_not_erase_known_placeholder",
      [ { doc := none, node := some 7, requester := some 9, request := 200
          state := .unclaimed, foldedInto := none, queuedAfter := none, retryParent := none } ],
      [ { doc := some 2, node := none, requester := none, request := 200
          state := .active, foldedInto := none, queuedAfter := none, retryParent := none } ], none)
  ]

theorem merge_keeps_distinct_physical_twins :
    ({ doc := some 3, node := some 7, requester := some 9, request := 200,
       state := RowState.active, foldedInto := none, queuedAfter := none, retryParent := none } : Row)
      ∈ mergeRows physicalTwinFoldRows physicalTwinFoldIncoming
    ∧ ({ doc := some 2, node := some 7, requester := some 9, request := 200,
         state := RowState.active, foldedInto := none, queuedAfter := none, retryParent := none } : Row)
      ∈ mergeRows physicalTwinFoldRows physicalTwinFoldIncoming := by
  decide

theorem physical_fold_owner_remains_resolvable :
    turnDocument (mergeRows physicalTwinFoldRows physicalTwinFoldIncoming) 1 = some 3 := by
  decide

theorem persisted_row_retires_matching_placeholder :
    mergeRows
      [{ doc := none, node := some 7, requester := some 9, request := 200,
         state := .unclaimed, foldedInto := none, queuedAfter := none, retryParent := none }]
      physicalTwinFoldIncoming = physicalTwinFoldIncoming := by
  decide

theorem different_known_requester_keeps_placeholder :
    ({ doc := none, node := some 7, requester := some 8, request := 200,
       state := RowState.unclaimed, foldedInto := none, queuedAfter := none, retryParent := none } : Row)
      ∈ mergeRows
        [{ doc := none, node := some 7, requester := some 8, request := 200,
           state := .unclaimed, foldedInto := none, queuedAfter := none, retryParent := none }]
        physicalTwinFoldIncoming := by
  decide

end ClientShell.RequestCacheIdentity
