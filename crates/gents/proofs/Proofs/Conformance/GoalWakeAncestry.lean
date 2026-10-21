import Proofs.Conformance.GoalRequestAncestry
import Lean

namespace Conformance.GoalWakeAncestryContracts

open GoalAutomation.RequestAncestry GoalAutomation.RequestHead Lean
open GoalRequestHeadContracts (root child grandchild rowJson)

def entry : AssignmentRow := {request := root}
def wake : AssignmentRow :=
  {request := {child with goal := none}, backgroundWake := true, runtimeWakeReceipt := true}
def continued : AssignmentRow := {request := grandchild}

structure Case where
  name : String
  rows : List AssignmentRow := [entry, wake]
  current : Nat := 20
  expectedGoal : Option Nat := none

def cases : List Case :=
  [ {name := "untagged_wake_keeps_original_parent"}
  , {name := "goal_created_during_wake_starts_here", rows := [entry, {wake with openingGoals := [1]}]}
  , {name := "later_wake_keeps_goal_created_during_first_wake",
      rows := [entry, {wake with openingGoals := [1]},
        {wake with request := {grandchild with goal := none}}], current := 30}
  , {name := "tagged_wake_uses_its_own_signed_reference", rows := [entry, {wake with tagged := true}]}
  , {name := "goal_after_untagged_wake_keeps_opening", rows := [entry, wake, continued], current := 30}
  , {name := "wake_after_goal_keeps_original_opening",
      rows := [entry, {request := child}, {wake with request := {grandchild with goal := none}}], current := 30}
  , {name := "wake_cannot_bridge_two_goal_ids",
      rows := [entry, {request := child},
        {wake with request := {grandchild with goal := none}},
        {request := {grandchild with
          doc := 40
          request := 400
          goal := some 2
          parentDoc := some 30
          parentRequest := some 300}}], current := 40}
  , {name := "unsigned_local_control_is_not_an_assignment_edge", rows := [entry, {wake with runtimeWakeReceipt := false}]}
  , {name := "missing_parent_does_not_invent_unassigned", rows := [wake]}
  , {name := "foreign_parent_does_not_join_assignment", rows := [{entry with request := {root with owner := 2}}, wake]}
  , {name := "changed_parent_pair_does_not_join_assignment",
      rows := [entry, {wake with request := {child with goal := none, parentRequest := some 999}}]}
  , {name := "untagged_wake_cannot_drop_parent_reference", rows := [{entry with tagged := true}, wake]}
  , {name := "forged_wake_signature_does_not_join_assignment",
      rows := [entry, {wake with request := {child with goal := none, receiptValid := false}}]}
  , {name := "ordinary_parent_link_stays_independent", rows := [entry, {wake with backgroundWake := false}]}
  , {name := "goal_path_keeps_its_matching_wake_opening",
      rows := [entry, {wake with openingGoals := [1]}, continued], current := 30}
  , {name := "goal_path_cannot_borrow_another_goals_wake_opening",
      rows := [entry, {wake with openingGoals := [2]}, continued], current := 30}
  ]

def rowValue (r : AssignmentRow) : Json := Json.mkObj
  [("request", (Json.parse (rowJson r.request)).toOption.getD Json.null),
    ("background_wake", toJson r.backgroundWake), ("tagged", toJson r.tagged),
    ("runtime_wake_receipt", toJson r.runtimeWakeReceipt), ("opening_goals", toJson r.openingGoals)]

def caseJson (c : Case) : Json := Json.mkObj
  [("name", toJson c.name), ("rows", toJson (c.rows.map rowValue)),
    ("current", toJson c.current), ("expected_goal", toJson c.expectedGoal),
    ("expected_entry", toJson (assignmentEntry 1 1 c.rows c.expectedGoal c.current)),
    ("expected_invocation_entry", toJson (signedEntry 1 1 (c.rows.map AssignmentRow.request) c.expectedGoal c.current))]

def casesJson : String := (toJson (cases.map caseJson)).compress

example : (cases.map fun c => assignmentEntry 1 1 c.rows c.expectedGoal c.current).take 8 =
    [some 10, some 20, some 20, some 20, some 10, some 10, none, none] := by native_decide

example : (cases.drop 14).map (fun c => assignmentEntry 1 1 c.rows c.expectedGoal c.current) =
    [some 20, none] := by native_decide

end Conformance.GoalWakeAncestryContracts
