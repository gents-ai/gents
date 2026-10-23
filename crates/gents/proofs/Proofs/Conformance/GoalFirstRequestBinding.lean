import Proofs.GoalAutomation.FirstRequestBinding
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.GoalFirstRequestBinding
open GoalAutomation.FirstRequestBinding Conformance.Contracts

structure Case where
  name : String
  current : Option String := none
  requestDoc : String := "request"
  goal : String := "goal"
  reference : Option Enrollment.GoalAssignmentInput := none
  currentTaskGoal : Option String := none
  taskChanged : Bool := false
  taskNow : Option String := none
  commit : Bool := true

def cases : List Case :=
  [ { name := "shared-native-first-request" }
  , { name := "shared-signed-first-request", reference := some ⟨"goal", none⟩ }
  , { name := "same-physical-request-idempotent", current := some "request" }
  , { name := "signed-physical-request-idempotent", current := some "request", reference := some ⟨"goal", none⟩ }
  , { name := "second-first-request-refused", current := some "old-request" }
  , { name := "foreign-signed-goal-refused", reference := some ⟨"foreign-goal", none⟩ }
  , { name := "borrowed-root-refused", reference := some ⟨"goal", some "old-request"⟩ }
  , { name := "missing-goal-identity-refused", goal := "" }
  , { name := "missing-physical-request-refused", requestDoc := "" }
  , { name := "task-after-client-replacement", currentTaskGoal := some "replacement-goal" }
  , { name := "task-existing-initial-goal", currentTaskGoal := some "initial-goal" }
  , { name := "root-and-request-transaction-discarded", reference := some ⟨"goal", none⟩, commit := false }
  , { name := "task-selection-preempted-by-replacement", taskChanged := true, taskNow := some "replacement-goal" }
  , { name := "task-selected-replacement-removed", currentTaskGoal := some "replacement-goal", taskChanged := true }
  ]

private def optional (value : Option String) : String :=
  value.map jsonString |>.getD "null"

private def encode (value : Case) : String :=
  let selected := root? value.goal value.current value.requestDoc value.reference
  let taskSelected := taskGoalId value.currentTaskGoal "initial-goal"
  let taskObserved := if value.taskChanged then value.taskNow else value.currentTaskGoal
  "{\"name\":" ++ jsonString value.name ++
  ",\"goal\":" ++ jsonString value.goal ++
  ",\"current\":" ++ optional value.current ++
  ",\"request_doc\":" ++ jsonString value.requestDoc ++
  ",\"reference\":" ++ (match value.reference with
    | none => "null"
    | some reference => "{\"goal_id\":" ++ jsonString reference.goalId ++
        ",\"root_request_doc_id\":" ++ optional reference.rootRequestDocId ++ "}") ++
  ",\"accepted\":" ++ toString selected.isSome ++
  ",\"commit\":" ++ toString value.commit ++
  ",\"expected_root\":" ++ optional (if value.commit && selected.isSome then selected else value.current) ++
  ",\"current_task_goal\":" ++ optional value.currentTaskGoal ++
  ",\"initial_task_goal\":\"initial-goal\",\"expected_task_goal\":" ++
    jsonString taskSelected ++ ",\"observed_task_goal\":" ++ optional taskObserved ++
    ",\"task_binding_valid\":" ++ toString (taskBindingValid taskSelected taskObserved "initial-goal") ++ "}"

def json : String := jsonArray (cases.map encode)

structure ContinuationCase where
  name : String
  goal : String := "goal"
  current : Option String := none
  parentDoc : String := "parent"
  parent : Option Enrollment.GoalAssignmentInput := none
  receiptDoc : Option String := none

def continuationCases : List ContinuationCase :=
  [ { name := "fresh-continuation-opens-assignment" }
  , { name := "unbound-signed-predecessor-refused", parent := some ⟨"goal", none⟩ }
  , { name := "receipt-cannot-repair-unbound-goal", receiptDoc := some "child" }
  , { name := "opening-receipt-keeps-signed-self", current := some "child", receiptDoc := some "child" }
  , { name := "signed-opening-parent-keeps-physical-root", current := some "parent", parent := some ⟨"goal", none⟩ }
  , { name := "untagged-opening-parent-keeps-physical-root", current := some "parent" }
  , { name := "later-parent-keeps-physical-root", current := some "root", parent := some ⟨"goal", some "root"⟩ }
  , { name := "later-receipt-keeps-physical-root", current := some "root", parent := some ⟨"goal", some "root"⟩, receiptDoc := some "child" }
  , { name := "foreign-signed-goal-refused", current := some "root", parent := some ⟨"other", some "root"⟩ }
  , { name := "foreign-signed-root-refused", current := some "root", parent := some ⟨"goal", some "other"⟩ }
  , { name := "unrelated-untagged-parent-refused", current := some "root" }
  , { name := "opening-receipt-cannot-borrow-parent-reference", current := some "child", parent := some ⟨"goal", some "parent"⟩, receiptDoc := some "child" }
  , { name := "empty-goal-refused", goal := "" }
  , { name := "empty-parent-refused", parentDoc := "" }
  , { name := "empty-root-refused", current := some "" }
  , { name := "empty-receipt-refused", receiptDoc := some "" }
  , { name := "parent-is-not-its-own-child", current := some "parent", receiptDoc := some "parent" }
  ]

private def referenceJson (reference : Option Enrollment.GoalAssignmentInput) : String :=
  match reference with
  | none => "null"
  | some reference => "{\"goal_id\":" ++ jsonString reference.goalId ++
      ",\"root_request_doc_id\":" ++ optional reference.rootRequestDocId ++ "}"

private def encodeContinuation (value : ContinuationCase) : String :=
  let edges : List (Option Enrollment.GoalAssignmentInput) :=
    [none, some ⟨"goal", none⟩, some ⟨"goal", some "root"⟩,
      some ⟨"goal", some "parent"⟩, some ⟨"other", none⟩, some ⟨"goal", some "other"⟩]
  "{\"name\":" ++ jsonString value.name ++ ",\"goal\":" ++ jsonString value.goal ++
    ",\"current\":" ++ optional value.current ++ ",\"parent_doc\":" ++ jsonString value.parentDoc ++
    ",\"parent\":" ++ referenceJson value.parent ++ ",\"receipt_doc\":" ++ optional value.receiptDoc ++
    ",\"expected\":" ++ referenceJson (continuationReference? value.goal value.current
      value.parentDoc value.parent value.receiptDoc) ++
    ",\"edges\":" ++ jsonArray (edges.map fun child =>
      "{\"child\":" ++ referenceJson child ++ ",\"accepted\":" ++
        toString (continuationEdge value.goal value.parentDoc value.parent child) ++ "}") ++ "}"

def continuationsJson : String := jsonArray (continuationCases.map encodeContinuation)

example : (continuationCases.take 4).map (fun value => continuationReference? value.goal
    value.current value.parentDoc value.parent value.receiptDoc) =
    [some ⟨"goal", none⟩, none, none, some ⟨"goal", none⟩] := by native_decide
end Conformance.GoalFirstRequestBinding
