import Proofs.Conformance.ClientShell.Contracts.SessionTurn
import Proofs.Conformance.Family

namespace Conformance.SessionQueueOrder

open Lean ClientShell.SessionTurn Conformance.ClientShellContracts.SessionTurnCases

deriving instance ToJson for RowState, Row

def scenarios : List (String × List Nat) :=
  [ ("replacement_keeps_original_slot", [4, 3])
  , ("replacement_taken_before_pending_read", [4, 3])
  , ("whole_queue_taken_before_pending_read", [4, 3])
  , ("reordered_queue", [3, 4])
  , ("unrelated_arrival", [99, 4, 3])
  , ("missing_arrival_is_not_invented", [3]) ]

def queueRows : List Row :=
  [ row 1 100 .active
  , row 3 102 .unclaimed (queuedAfter := some 100)
  , row 4 103 .unclaimed (queuedAfter := some 100) ]

structure ArrivalInput where
  name : String
  rows : List Row
  arrivals : List Nat
  deriving ToJson

@[conformance_family]
def arrival : Family ArrivalInput (List Nat) where
  inputs := scenarios.map fun (name, arrivals) => { name, rows := queueRows, arrivals }
  model := fun i =>
    (orderedQueued (queuedBehind i.rows (row 1 100 .active)) i.arrivals).map (·.doc)

end Conformance.SessionQueueOrder
