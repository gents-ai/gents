import Proofs.Conformance.ClientShell.Contracts.SessionTurn

namespace Conformance.SessionQueueOrder

open ClientShell.SessionTurn Conformance.ClientShellContracts.SessionTurnCases

def scenarios : List (String × List Nat × List Nat) :=
  [ ("replacement_keeps_original_slot", [4, 3], [4, 3])
  , ("replacement_taken_before_pending_read", [4, 3], [3])
  , ("whole_queue_taken_before_pending_read", [4, 3], [])
  , ("reordered_queue", [3, 4], [3, 4])
  , ("unrelated_arrival", [99, 4, 3], [4, 3])
  , ("missing_arrival_is_not_invented", [3], [3]) ]

def queueRows : List Row :=
  [ row 1 100 .active
  , row 3 102 .unclaimed (queuedAfter := some 100)
  , row 4 103 .unclaimed (queuedAfter := some 100) ]

def caseJson (entry : String × List Nat × List Nat) : String :=
  let queued := queuedBehind queueRows (row 1 100 .active)
  "{\"name\":" ++ Conformance.Contracts.jsonString entry.1 ++
  ",\"rows\":" ++ rowsJson queueRows ++
  ",\"arrivals\":" ++ natList entry.2.1 ++
  ",\"pending\":" ++ natList entry.2.2 ++
  ",\"expected\":" ++ natList ((orderedQueued queued entry.2.1).map (·.doc)) ++ "}"

def casesJson : String :=
  "[" ++ String.intercalate "," (scenarios.map caseJson) ++ "]"

end Conformance.SessionQueueOrder
