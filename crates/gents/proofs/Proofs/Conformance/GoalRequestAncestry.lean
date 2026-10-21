import Proofs.GoalAutomation.RequestAncestry
import Proofs.Conformance.GoalRequestHead
import Lean

namespace Conformance.GoalRequestAncestryContracts

open GoalAutomation.RequestAncestry GoalAutomation.RequestHead Lean
open GoalRequestHeadContracts (root child grandchild rowJson)

structure Case where
  name : String
  rows : List Row := [root, child, grandchild]
  current : Nat := 30
  expectedGoal : Option Nat := none

def cases : List Case :=
  [ {name := "untagged_entry_is_its_own_root", rows := [root], current := 10}
  , {name := "untagged_task_continuation_keeps_goal"}
  , {name := "known_goal_accepts_matching_ancestry", expectedGoal := some 1}
  , {name := "later_child_cannot_change_untagged_goal",
      rows := [root, child, {grandchild with goal := some 2}]}
  , {name := "older_ancestor_cannot_change_untagged_goal",
      rows := [root, {child with goal := some 2}, grandchild]}
  , {name := "known_goal_rejects_other_goal", expectedGoal := some 2}
  , {name := "single_other_goal_still_resolves_its_entry",
      rows := [root, {child with goal := some 2}], current := 20}
  , {name := "ordinary_parent_link_does_not_join_a_goal",
      rows := [root, {child with goal := none}], current := 20}
  , {name := "foreign_root_cannot_join_ancestry",
      rows := [{root with owner := 2}, child, grandchild]}
  , {name := "invalid_entry_signature_cannot_join_ancestry",
      rows := [{root with receiptValid := false}, child, grandchild]}
  , {name := "missing_physical_parent_is_unknown", rows := [child, grandchild]}
  , {name := "invalid_continuation_receipt_is_unknown",
      rows := [root, child, {grandchild with receiptValid := false}]}
  ]

def caseJson (c : Case) : String :=
  "{\"name\":" ++ Conformance.Contracts.jsonString c.name ++
    ",\"scope\":{\"owner\":1,\"session\":1,\"goal\":1}" ++
    ",\"rows\":" ++ Conformance.Contracts.jsonArray (c.rows.map rowJson) ++
    ",\"current\":" ++ toString c.current ++
    ",\"expected_goal\":" ++ (toJson c.expectedGoal).compress ++
    ",\"expected_entry\":" ++ (toJson (signedEntry 1 1 c.rows c.expectedGoal c.current)).compress ++ "}"

def casesJson : String := Conformance.Contracts.jsonArray (cases.map caseJson)

example : cases.length = 12 := by decide
example : (cases.map fun c => signedEntry 1 1 c.rows c.expectedGoal c.current).take 6 =
    [some 10, some 10, some 10, none, none, none] := by native_decide

end Conformance.GoalRequestAncestryContracts
