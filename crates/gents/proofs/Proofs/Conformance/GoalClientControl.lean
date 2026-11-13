import Proofs.Automation.GoalAutomation.ClientControl
import Proofs.Conformance.GoalOperatorResume
import Proofs.Conformance.GoalResetResume
import Proofs.Conformance.Family

namespace Conformance.GoalClientControlContracts

open Lean GoalAutomation.ClientControl Conformance.Contracts

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

deriving instance ToJson for Revision

structure GoalSnapshot where
  revision : Revision
  execution : GoalResetResumeContracts.SnapshotWire
  objective : String
  deriving ToJson

def GoalSnapshot.ofModel (s : Snapshot) : GoalSnapshot :=
  ⟨s.revision, GoalResetResumeContracts.SnapshotWire.ofModel s.execution, s.objective⟩

def GoalSnapshot.toModel (s : GoalSnapshot) : Snapshot :=
  ⟨s.revision, s.execution.toModel, s.objective⟩

structure OperationWire where
  kind : String
  objective : Option String
  budget : Option Int
  deriving ToJson

def OperationWire.ofModel : Operation → OperationWire
  | .pause => ⟨"pause", none, none⟩
  | .resume => ⟨"resume", none, none⟩
  | .stop => ⟨"stop", none, none⟩
  | .edit fields => ⟨"edit", fields.objective, fields.budget⟩

def OperationWire.toModel (o : OperationWire) : Operation :=
  match o.kind with
  | "pause" => .pause
  | "resume" => .resume
  | "stop" => .stop
  | _ => .edit ⟨o.objective, o.budget⟩

structure FactsWire where
  authorized : Bool
  unfinishedRequest : Bool
  resume : Option GoalResetResumeContracts.RequestWire
  deriving ToJson

def FactsWire.ofModel (f : Facts) : FactsWire :=
  ⟨f.authorized, f.unfinishedRequest, f.resume.map GoalResetResumeContracts.RequestWire.ofModel⟩

def FactsWire.toModel (f : FactsWire) : Facts :=
  ⟨f.authorized, f.unfinishedRequest, f.resume.map GoalResetResumeContracts.RequestWire.toModel⟩

structure ControlInput where
  name : String
  before : GoalSnapshot
  expectedRevision : Revision
  nextComposite : String
  operation : OperationWire
  facts : FactsWire
  deriving ToJson

def ControlInput.ofCase (c : Case) : ControlInput :=
  { name := c.name, before := GoalSnapshot.ofModel c.before, expectedRevision := c.expectedRevision
    nextComposite := c.nextComposite, operation := OperationWire.ofModel c.operation
    facts := FactsWire.ofModel c.facts }

def ControlInput.after (i : ControlInput) : Option GoalSnapshot :=
  (apply? i.before.toModel i.expectedRevision i.nextComposite i.operation.toModel
    i.facts.toModel).map GoalSnapshot.ofModel

/-- One signed command persisted directly and answered by the runtime command owner. -/
@[conformance_family]
def control : Family ControlInput (Option GoalSnapshot) where
  inputs := cases.map ControlInput.ofCase
  model := ControlInput.after

/-- The client submission owner publishes a command, waits for the runtime's
receipt and recovers it; each submission fixture is a separately served node,
so only these cases are built: a denied requester needs its own ACP setup and
a foreign document fence needs the recovery scenario. -/
def submissionCases (names : List String) : List ControlInput :=
  (cases.filter fun c => c.name ∈ names).map ControlInput.ofCase

/-- A requester without session update access submits a pause. -/
@[conformance_family]
def deniedSubmission : Family ControlInput (Option GoalSnapshot) where
  inputs := submissionCases ["active_denied_busy_current_pause"]
  model := ControlInput.after

/-- A pause fenced on a foreign goal document is refused durably and the
refusal is recoverable by resubmission. -/
@[conformance_family]
def foreignSubmission : Family ControlInput (Option GoalSnapshot) where
  inputs := submissionCases ["active_authorized_idle_foreign_document_pause"]
  model := ControlInput.after

example : (cases.filter fun c => c.name = "active_denied_busy_current_pause").all fun c =>
    !c.facts.authorized && c.after.isNone := by native_decide

example : (cases.filter fun c => c.name = "active_authorized_idle_foreign_document_pause").all
    fun c => c.facts.authorized && !c.facts.unfinishedRequest &&
      c.expectedRevision == ⟨"foreign-goal", "revision-current"⟩ && c.after.isNone := by
  native_decide

structure TraceStep where
  operation : OperationWire
  facts : FactsWire
  deriving ToJson

structure TraceInput where
  name : String
  before : GoalSnapshot
  steps : List TraceStep
  deriving ToJson

/-- Each step is submitted against the goal's current commit; a refused step
leaves the goal as it was for the next one. -/
def runTrace (s : Snapshot) : List TraceStep → List (Option GoalSnapshot)
  | [] => []
  | step :: rest =>
    match apply? s s.revision "revision-after" step.operation.toModel step.facts.toModel with
    | none => none :: runTrace s rest
    | some next => some (GoalSnapshot.ofModel next) :: runTrace next rest

def pauseThenResume : TraceInput :=
  { name := "pause_then_resume", before := GoalSnapshot.ofModel (initial .active)
    steps := [ ⟨OperationWire.ofModel .pause, FactsWire.ofModel (facts .active true true)⟩
             , ⟨OperationWire.ofModel .resume, FactsWire.ofModel (facts .paused true true)⟩ ] }

/-- An authorized user pauses a goal whose request is still running, then
resumes it, through the client submission owner on one served goal. -/
@[conformance_family]
def submissionTrace : Family TraceInput (List (Option GoalSnapshot)) where
  inputs := [pauseThenResume]
  model := fun i => runTrace i.before.toModel i.steps

example : ((apply? (initial .active) (initial .active).revision "revision-after" .pause
    (facts .active true true)).map (·.execution)) = some (initial .paused).execution := by
  native_decide

end Conformance.GoalClientControlContracts
