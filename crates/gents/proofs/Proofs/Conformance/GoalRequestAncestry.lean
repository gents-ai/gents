import Proofs.GoalAutomation.RequestAncestry
import Proofs.Conformance.GoalRequestHead
import Proofs.Conformance.GoalRequestAssignment
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

open GoalAutomation.RequestAssignment

structure ScopeCase where
  name : String
  rootTagged : Bool := true
  rootOpenings : List Nat := []
  childOpenings : List Nat := []
  childValid : Bool := true
  missingParent : Bool := false

def scopeCases : List ScopeCase :=
  [ {name := "signed_continuation"}
  , {name := "signed_continuation_ignores_conflicting_opening", rootOpenings := [2]}
  , {name := "signed_continuation_ignores_intermediate_opening", childOpenings := [1]}
  , {name := "untagged_continuation_uses_retained_opening", rootTagged := false, rootOpenings := [1]}
  , {name := "untagged_continuation_refuses_conflicting_opening", rootTagged := false, rootOpenings := [2]}
  , {name := "untagged_continuation_uses_intermediate_opening", rootTagged := false, childOpenings := [1]}
  , {name := "signed_continuation_refuses_invalid_receipt", childValid := false}
  , {name := "signed_continuation_refuses_missing_parent", missingParent := true}
  ]

def scopeRows (c : ScopeCase) : List AssignmentRow :=
  (if c.missingParent then [] else
    [{request := root, tagged := c.rootTagged, openingGoals := c.rootOpenings}]) ++
    [{request := {child with receiptValid := c.childValid}, tagged := c.rootTagged,
      openingGoals := c.childOpenings}]

def scopeEntries (c : ScopeCase) : List Entry :=
  [⟨10, 1, 1, if c.rootTagged then some ⟨1, none⟩ else none, true⟩,
   ⟨20, 1, 1, if c.rootTagged then some ⟨1, some 10⟩ else none, c.childValid⟩]

def scopeAssignments (c : ScopeCase) : List Assignment :=
  c.rootOpenings.map (fun goal => ⟨1,1,goal,some 10⟩) ++
    c.childOpenings.map (fun goal => ⟨1,1,goal,some 20⟩)

def scopeResult (c : ScopeCase) : Option QueueScope :=
  queueScopeWithAncestry 1 1 (scopeRows c) (scopeEntries c) (scopeAssignments c) 20

def scopeCaseJson (c : ScopeCase) : Json := Json.mkObj
  [("name", toJson c.name), ("root_tagged", toJson c.rootTagged),
   ("root_openings", toJson c.rootOpenings), ("child_openings", toJson c.childOpenings),
   ("child_valid", toJson c.childValid), ("missing_parent", toJson c.missingParent),
   ("expected", (scopeResult c).map GoalRequestAssignmentContracts.queueScopeJson |>.getD Json.null)]

def scopeCasesJson : String := (toJson (scopeCases.map scopeCaseJson)).compress

example : scopeResult {name := "conflicting", rootOpenings := [2]} =
    some (.referenced 1 10) := by native_decide
example : GoalAutomation.RequestAncestry.assignmentEntry 1 1
    (scopeRows {name := "conflicting", rootOpenings := [2]}) none 20 = none := by native_decide

end Conformance.GoalRequestAncestryContracts
