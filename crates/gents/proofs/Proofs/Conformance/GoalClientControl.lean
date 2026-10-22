import Proofs.GoalAutomation.ClientControl
import Proofs.Conformance.GoalOperatorResume

namespace Conformance.GoalClientControlContracts

open GoalAutomation.ClientControl Conformance.Contracts

structure Case where
  name : String
  before : Snapshot
  expectedRevision : Revision
  nextComposite : String
  operation : Operation
  facts : Facts
  deriving DecidableEq, Repr

def initial (status : Goals.Status) : Snapshot :=
  { revision := ⟨"goal-document", "revision-current"⟩
    execution := { GoalOperatorResumeContracts.initial with
      goal := { GoalOperatorResumeContracts.initial.goal with status } }
    objective := "ship" }

def facts (status : Goals.Status) (authorized busy : Bool) : Facts :=
  { authorized, unfinishedRequest := busy
    resume := if busy then none else some
      { GoalOperatorResumeContracts.request with expectedStatus := status } }

def operations : List (String × Operation) :=
  [ ("pause", .pause)
  , ("resume", .resume)
  , ("stop", .stop)
  , ("objective", .edit ⟨some "  verify  ", none⟩)
  , ("budget", .edit ⟨none, some 2000⟩)
  , ("blank_objective", .edit ⟨some "\u3000\u00A0", none⟩)
  , ("empty_edit", .edit ⟨none, none⟩)
  , ("zero_budget", .edit ⟨none, some 0⟩)
  , ("negative_budget", .edit ⟨none, some (-1)⟩)
  ]

def revisions : List (String × Revision) :=
  [ ("current", ⟨"goal-document", "revision-current"⟩)
  , ("stale_commit", ⟨"goal-document", "revision-before"⟩)
  , ("foreign_document", ⟨"foreign-goal", "revision-current"⟩)
  ]

def cases : List Case :=
  [Goals.Status.active, .paused, .blocked, .usageLimited, .budgetLimited, .complete, .stopped].flatMap fun status =>
  [true, false].flatMap fun authorized =>
  [true, false].flatMap fun busy =>
  revisions.flatMap fun (revisionName, revision) =>
  operations.map fun (operationName, operation) => {
    name := status.toDefraDB ++ "_" ++ (if authorized then "authorized" else "denied") ++
      "_" ++ (if busy then "busy" else "idle") ++ "_" ++ revisionName ++ "_" ++ operationName
    before := initial status
    expectedRevision := revision
    nextComposite := "revision-after"
    operation
    facts := facts status authorized busy }

def Case.after (c : Case) : Option Snapshot :=
  apply? c.before c.expectedRevision c.nextComposite c.operation c.facts

example : cases.length = 756 := by native_decide

theorem busy_resume_retains_the_request :
    ((apply? (initial .paused) (initial .paused).revision "revision-after" .resume
      (facts .paused true true)).map fun s =>
        (s.execution.goal.status, s.execution.sequence, s.execution.latestRequest,
          s.execution.children)) = some (.active, 0, 10, []) := by
  native_decide

theorem idle_resume_uses_the_continuation_owner :
    ((apply? (initial .paused) (initial .paused).revision "revision-after" .resume
      (facts .paused true false)).map fun s =>
        (s.execution.goal.status, s.execution.sequence, s.execution.latestRequest)) =
      some (.active, 1, 20) := by
  native_decide

def revisionJson (revision : Revision) : String :=
  "{\"document\":" ++ jsonString revision.document ++
    ",\"composite\":" ++ jsonString revision.composite ++ "}"

def snapshotJson (s : Snapshot) : String :=
  "{\"revision\":" ++ revisionJson s.revision ++
    ",\"execution\":" ++ GoalOperatorResumeContracts.snapshotJson s.execution ++
    ",\"objective\":" ++ jsonString s.objective ++ "}"

def operationJson : Operation → String
  | .pause => "{\"kind\":\"pause\"}"
  | .resume => "{\"kind\":\"resume\"}"
  | .stop => "{\"kind\":\"stop\"}"
  | .edit fields =>
      "{\"kind\":\"edit\",\"objective\":" ++
        (match fields.objective with | none => "null" | some value => jsonString value) ++
        ",\"budget\":" ++
        (match fields.budget with | none => "null" | some value => toString value) ++ "}"

def factsJson (f : Facts) : String :=
  "{\"authorized\":" ++ (if f.authorized then "true" else "false") ++
    ",\"unfinished_request\":" ++ (if f.unfinishedRequest then "true" else "false") ++
    ",\"resume\":" ++ (match f.resume with
      | none => "null" | some value => GoalOperatorResumeContracts.requestJson value) ++ "}"

def caseJson (c : Case) : String :=
  "{\"name\":" ++ jsonString c.name ++
    ",\"before\":" ++ snapshotJson c.before ++
    ",\"expected_revision\":" ++ revisionJson c.expectedRevision ++
    ",\"next_composite\":" ++ jsonString c.nextComposite ++
    ",\"operation\":" ++ operationJson c.operation ++
    ",\"facts\":" ++ factsJson c.facts ++
    ",\"expected\":" ++ (match c.after with
      | none => "null" | some value => snapshotJson value) ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

end Conformance.GoalClientControlContracts
