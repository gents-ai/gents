import Proofs.GoalAutomation.RequestHead
import Proofs.GoalAutomation.RequestAncestry
import Proofs.Conformance.ContractTypes

namespace Conformance.GoalRequestHeadContracts

open GoalAutomation.RequestHead Conformance.Contracts

structure Case where
  name : String
  rows : List Row
  expected : Option Row
  assignmentRoot : Option Nat
  deriving DecidableEq, Repr

def scope : Scope := ⟨1, 1, 1⟩
def root : Row := ⟨.normal, 10, 100, 1, 1, none, none, none, 0, true, true, .execution⟩
def child : Row := ⟨.normal, 20, 200, 1, 1, some 1, some 10, some 100, 1, true, true, .execution⟩
def grandchild : Row := ⟨.normal, 30, 300, 1, 1, some 1, some 20, some 200, 2, true, true, .execution⟩
def unrelated : Row := ⟨.normal, 40, 400, 1, 1, none, none, none, 0, true, true, .execution⟩

def retainedChain : List Row :=
  (List.range 128).map fun index =>
    { root with
      doc := 10 + index
      request := 100 + index
      goal := if index == 0 then none else some 1
      parentDoc := if index == 0 then none else some (9 + index)
      parentRequest := if index == 0 then none else some (99 + index)
      sequence := index }

def assignedRows (testCase : Case) : List Row :=
  match testCase.assignmentRoot with
  | none => testCase.rows
  | some opening =>
    let assignments : List GoalAutomation.RequestAncestry.AssignmentRow :=
      testCase.rows.map fun row =>
        {request := row, openingGoals := if row.doc == opening then [scope.goal] else []}
    testCase.rows.filter fun row =>
      GoalAutomation.RequestAncestry.assignmentEntry scope.owner scope.session assignments
        (some scope.goal) row.doc == some opening

def cases : List Case :=
  [ ⟨"same_second_graph_parent", [root, child], some child, none⟩
  , ⟨"legacy_source_omission_keeps_physical_edge", [root, child], some child, none⟩
  , ⟨"same_second_task_parent_chain", [root, grandchild, child], some grandchild, none⟩
  , ⟨"reverse_lexical_order", [child, root], some child, none⟩
  , ⟨"unrelated_latest_keeps_priority", [unrelated, root, child], some unrelated, none⟩
  , ⟨"foreign_edge_cannot_suppress", [root, {child with owner := 2}], some root, none⟩
  , ⟨"arbitrary_signed_parent_link_cannot_suppress", [root, {child with goal := none}], some root, none⟩
  , ⟨"other_goal_link_cannot_suppress", [root, {child with goal := some 2}], some root, none⟩
  , ⟨"missing_parent_does_not_suppress_unrelated_head", [root, {child with parentDoc := some 99}], some root, none⟩
  , ⟨"mismatched_physical_pair_does_not_suppress", [root, {child with parentRequest := some 999}], some root, none⟩
  , ⟨"invalid_signature_does_not_suppress", [root, {child with receiptValid := false}], some root, none⟩
  , ⟨"wrong_deterministic_identity_does_not_suppress", [root, {child with deterministicIdentity := false}], some root, none⟩
  , ⟨"branch_leaves_keep_canonical_order", [root, child, {grandchild with parentDoc := some 10, parentRequest := some 100}], some child, none⟩
  , ⟨"new_goal_epoch_may_reset_sequence", [root, {child with sequence := 5}, {grandchild with sequence := 1}], some {grandchild with sequence := 1}, none⟩
  , ⟨"folded_input_is_not_the_goal_execution_head", [{unrelated with inputDisposition := .folded}, root, child], some child, none⟩
  , ⟨"replaced_input_is_not_the_goal_execution_head", [{unrelated with inputDisposition := .replaced}, root, child], some child, none⟩
  , ⟨"removed_input_is_not_an_execution_interrupt", [{unrelated with inputDisposition := .removed}, root, child], some child, none⟩
  , ⟨"retired_inputs_do_not_invent_execution", [{unrelated with inputDisposition := .folded}, {root with inputDisposition := .replaced}], none, none⟩
  , ⟨"retained_assigned_goal_chain", retainedChain.reverse, retainedChain.getLast?, some 10⟩
  , ⟨"empty_session", [], none, none⟩
  , ⟨"title_cannot_be_public_head", [{unrelated with purpose := .titleAudit}, root], some root, none⟩
  , ⟨"title_cannot_supersede_parent", [root, {child with purpose := .titleAudit}], some root, none⟩
  ]

example : select scope [{child with parentDoc := some 30, parentRequest := some 300}, grandchild] = none := by decide

theorem cases_replay_explicit_expectations : ∀ c ∈ cases, select scope (assignedRows c) = c.expected := by native_decide


private def boolJson (value : Bool) : String := if value then "true" else "false"
private def dispositionJson : InputDisposition → String
  | .execution => jsonString "execution"
  | .folded => jsonString "folded"
  | .replaced => jsonString "replaced"
  | .removed => jsonString "removed"
private def optionalNatJson : Option Nat → String
  | none => "null"
  | some value => toString value

def rowJson (row : Row) : String :=
  "{\"purpose\":" ++ jsonString row.purpose.toWire ++ ",\"doc\":" ++ toString row.doc ++ ",\"request\":" ++ toString row.request ++
  ",\"owner\":" ++ toString row.owner ++ ",\"session\":" ++ toString row.session ++
  ",\"goal\":" ++ optionalNatJson row.goal ++ ",\"parent_doc\":" ++ optionalNatJson row.parentDoc ++
  ",\"parent_request\":" ++ optionalNatJson row.parentRequest ++ ",\"sequence\":" ++ toString row.sequence ++
  ",\"receipt_valid\":" ++ boolJson row.receiptValid ++
  ",\"deterministic_identity\":" ++ boolJson row.deterministicIdentity ++
  ",\"input_disposition\":" ++ dispositionJson row.inputDisposition ++ "}"

def caseJson (testCase : Case) : String :=
  "{\"name\":" ++ jsonString testCase.name ++
  ",\"scope\":{\"owner\":" ++ toString scope.owner ++
  ",\"session\":" ++ toString scope.session ++ ",\"goal\":" ++ toString scope.goal ++ "}" ++
  ",\"assignment_root\":" ++ optionalNatJson testCase.assignmentRoot ++
  ",\"rows\":" ++ jsonArray (testCase.rows.map rowJson) ++
  ",\"expected\":" ++ (match testCase.expected with | none => "null" | some row => rowJson row) ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

theorem cases_count : cases.length = 22 := by decide

end Conformance.GoalRequestHeadContracts
