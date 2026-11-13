import Proofs.Automation.GoalAutomation.FirstRequestBinding
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Family

namespace Conformance.GoalFirstRequestBinding
open GoalAutomation.FirstRequestBinding Conformance.Contracts Lean

deriving instance ToJson for Enrollment.GoalAssignmentInput

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

structure BindingInput where
  name : String
  goal : String
  current : Option String
  requestDoc : String
  reference : Option Enrollment.GoalAssignmentInput
  commit : Bool
  deriving ToJson

structure BindingOutput where
  accepted : Bool
  root : Option String
  deriving ToJson

/-- Whether `bind_goal_first_request_in_txn` accepts the first request and the
assignment root the Goal holds after its transaction commits or is discarded. -/
@[conformance_family]
def binding : Family BindingInput BindingOutput where
  inputs := cases.map fun c =>
    { name := c.name, goal := c.goal, current := c.current, requestDoc := c.requestDoc
      reference := c.reference, commit := c.commit }
  model := fun i =>
    let selected := root? i.goal i.current i.requestDoc i.reference
    { accepted := selected.isSome
      root := if i.commit && selected.isSome then selected else i.current }

structure TaskInput where
  name : String
  currentTaskGoal : Option String
  initialTaskGoal : String
  observedTaskGoal : Option String
  deriving ToJson

structure TaskOutput where
  taskGoal : String
  taskBindingValid : Bool
  deriving ToJson

def taskInputs : List TaskInput :=
  cases.map fun c =>
    { name := c.name, currentTaskGoal := c.currentTaskGoal, initialTaskGoal := "initial-goal"
      observedTaskGoal := if c.taskChanged then c.taskNow else c.currentTaskGoal }

def taskOutput (i : TaskInput) : TaskOutput :=
  let selected := taskGoalId i.currentTaskGoal i.initialTaskGoal
  { taskGoal := selected
    taskBindingValid := taskBindingValid selected i.observedTaskGoal i.initialTaskGoal }

/-- The Task Goal `task_assignment_goal_id` selects, and whether
`validate_task_goal_identity_in_txn` and `write_task_delivery` admit it once
the observed Goal is in place. -/
@[conformance_family]
def task : Family TaskInput TaskOutput where
  inputs := taskInputs
  model := taskOutput

/-- The Task Goal `prepare_task_run_goal_id` freezes for a manual Task run, and
whether `write_task_delivery` admits it once the observed Goal is in place. -/
@[conformance_family]
def manualTask : Family TaskInput TaskOutput where
  inputs := taskInputs
  model := taskOutput

structure ContinuationCase where
  name : String
  goal : String := "goal"
  current : Option String := none
  parentDoc : String := "parent"
  parent : Option Enrollment.GoalAssignmentInput := none
  receiptDoc : Option String := none
  deriving ToJson

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

/-- The continuation assignment `continuation_reference` derives, over every
continuation case. -/
@[conformance_family]
def continuation : Family ContinuationCase (Option Enrollment.GoalAssignmentInput) where
  inputs := continuationCases
  model := fun c => continuationReference? c.goal c.current c.parentDoc c.parent c.receiptDoc

structure EdgeInput where
  name : String
  goal : String
  parentDoc : String
  parent : Option Enrollment.GoalAssignmentInput
  child : Option Enrollment.GoalAssignmentInput
  deriving ToJson

structure EdgeOutput where
  accepted : Bool
  deriving ToJson

/-- Whether `continuation_reference_matches` accepts each candidate child
reference after each continuation case's parent. -/
@[conformance_family]
def edge : Family EdgeInput EdgeOutput where
  inputs := continuationCases.flatMap fun c =>
    ([none, some ⟨"goal", none⟩, some ⟨"goal", some "root"⟩,
      some ⟨"goal", some "parent"⟩, some ⟨"other", none⟩, some ⟨"goal", some "other"⟩] :
        List (Option Enrollment.GoalAssignmentInput)).map fun child =>
      { name := c.name, goal := c.goal, parentDoc := c.parentDoc, parent := c.parent, child }
  model := fun i => { accepted := continuationEdge i.goal i.parentDoc i.parent i.child }

example : (continuationCases.take 4).map (fun value => continuationReference? value.goal
    value.current value.parentDoc value.parent value.receiptDoc) =
    [some ⟨"goal", none⟩, none, none, some ⟨"goal", none⟩] := by native_decide
end Conformance.GoalFirstRequestBinding
