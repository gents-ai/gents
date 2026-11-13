import Proofs.Automation.Triggers.Durable
import Proofs.Automation.Triggers.Groups
import Proofs.Automation.GoalAutomation
import Proofs.Conformance.Family

namespace Conformance.TriggerMaterializer

open Lean

inductive ReplayKind where
  | event
  | schedule
  deriving ToJson, DecidableEq

structure ReplayInput where
  kind : ReplayKind
  admitted : Bool
  renders : Bool
  deriving ToJson

structure ReplayOutput where
  outcome : String
  priorWorkKept : Bool
  deriving ToJson

def outcomeWire : Triggers.Durable.DispatchOutcome → String
  | .duplicate => "duplicate"
  | .fired => "fired"
  | .rejected => "rejected"

def replayIdentity (kind : ReplayKind) : Triggers.Durable.Identity :=
  match kind with
  | .event => ⟨"node", "replay-trigger", "ReplayWork", "physical-source"⟩
  | .schedule => ⟨"node", "replay-trigger", "Schedule", "persisted-due-occurrence"⟩

/-- A not-admitted fire whose template renders would fire and run latest-only
supersession against the fixture's live request; that path is owned by the
latest-admission cases. A not-admitted schedule fire needs a configured
schedule before it reaches rendering, which the event-trigger fixture lacks, so
the render boundary is an event case. -/
@[conformance_family]
def dispatch : Family ReplayInput ReplayOutput where
  inputs := [ReplayKind.event, .schedule].flatMap (fun kind =>
    [(true, true), (true, false)].map fun (admitted, renders) => { kind, admitted, renders }) ++
    [{ kind := .event, admitted := false, renders := false }]
  model := fun i =>
    let id := replayIdentity i.kind
    let state : Triggers.Durable.State := { receipts := if i.admitted then [id] else [] }
    let outcome := Triggers.Durable.replayDispatch state id i.renders
    { outcome := outcomeWire outcome, priorWorkKept := Triggers.Durable.priorWorkKept outcome }

inductive RecoveryRow where
  | materialized
  | foreignNode
  deriving ToJson, DecidableEq

structure RecoveryInput where
  row : Option RecoveryRow
  deriving ToJson

structure RecoveryOutput where
  disposition : String
  recoversMaterialized : Bool
  otherNodeRequestDiffers : Bool
  deriving ToJson

def dispositionWire : GoalAutomation.TaskFireRecoveryDisposition → String
  | .absent => "absent"
  | .recovered => "recovered"
  | .conflict => "conflict"

/-- Recovery of a goal-backed Task fire on node "node", given which row holds
the fire's deterministic request id. The materialized row is the binding
publication wrote; a foreign-node row carries the same ids under another node.
The same Task and fire key on another node derive another request. -/
@[conformance_family]
def taskFireRecovery : Family RecoveryInput RecoveryOutput where
  inputs := [none, some .materialized, some .foreignNode].map fun row => { row }
  model := fun i =>
    let expected := GoalAutomation.expectedTaskGoalRequestBinding "node" "agent" "task" "fire"
    let observed := i.row.map fun
      | .materialized => expected
      | .foreignNode => { expected with nodeDid := "other-node" }
    let decision := GoalAutomation.decideTaskFireRecovery expected ⟨observed, true, true⟩
    let published := GoalAutomation.decideTaskPublication ⟨some "objective", none⟩ "node" "task" "fire"
    { disposition := dispositionWire decision.disposition
      recoversMaterialized := decision.recoveredRequestId.isSome &&
        decision.recoveredRequestId == published.requestId
      otherNodeRequestDiffers := (GoalAutomation.taskFireIdentity "node" "task" "fire").requestId !=
        (GoalAutomation.taskFireIdentity "other-node" "task" "fire").requestId }

structure MarkerRow where
  owner : String
  trigger : String
  key : String
  goalTask : Option String
  deriving ToJson

structure MarkerQuery where
  owner : String
  trigger : String
  key : String
  deriving ToJson

structure MarkerInput where
  rows : List MarkerRow
  queries : List MarkerQuery
  deriving ToJson

/-- Owners, triggers and generation keys are placeholders the consumer maps to
real DIDs, trigger ids and group fire keys. -/
@[conformance_family]
def groupMarker : Family MarkerInput (List Bool) where
  inputs :=
    let old : MarkerRow := ⟨"owner", "trigger", "old", none⟩
    let goal : MarkerRow := ⟨"owner", "trigger", "new", some "old-task-name"⟩
    let queries : List MarkerQuery :=
      [⟨"owner", "trigger", "old"⟩, ⟨"owner", "trigger", "new"⟩, ⟨"other-owner", "trigger", "old"⟩,
       ⟨"owner", "other-trigger", "old"⟩, ⟨"owner", "trigger", "later"⟩,
       ⟨"other-owner", "trigger", "new"⟩]
    [⟨[old], queries⟩, ⟨[old, goal], queries⟩]
  model := fun i =>
    let requests := i.rows.map fun row =>
      ({ owner := row.owner, trigger := row.trigger, key := row.key, goalTask := row.goalTask } :
        Triggers.Groups.MarkerRequest)
    i.queries.map fun q => Triggers.Groups.hasMaterializedGroupRequest requests q.owner q.trigger q.key

inductive GateAction where
  | create
  | gate
  | supersede
  deriving ToJson, DecidableEq

inductive RequestKind where
  | schedule
  | event
  deriving ToJson, DecidableEq

/-- `request` is the created request id for `create` and the excluded request
id for `gate` and `supersede`; `kind` and `active` describe a created request. -/
structure GateStep where
  action : GateAction
  owner : String
  trigger : String
  request : Option String
  kind : RequestKind
  active : Bool
  deriving ToJson

structure GateObservation where
  gated : Option Bool
  superseded : Option Nat
  deriving ToJson

def gateFold : List Triggers.Groups.GateRequest → List GateStep → List GateObservation
  | _, [] => []
  | requests, step :: rest =>
    match step.action with
    | .create =>
      ⟨none, none⟩ :: gateFold (requests ++
        [{ id := step.request.getD "", owner := step.owner, trigger := step.trigger,
           active := step.active }]) rest
    | .gate =>
      ⟨some (Triggers.Groups.triggerWideGated requests step.owner step.trigger step.request), none⟩ ::
        gateFold requests rest
    | .supersede =>
      let (next, count) :=
        Triggers.Groups.supersedeTriggerWide requests step.owner step.trigger step.request
      ⟨none, some count⟩ :: gateFold next rest

def create (id owner trigger : String) (kind : RequestKind) (active : Bool) : GateStep :=
  ⟨.create, owner, trigger, some id, kind, active⟩

def query (action : GateAction) (owner trigger : String) (excluded : Option String) : GateStep :=
  ⟨action, owner, trigger, excluded, .event, false⟩

@[conformance_family]
def triggerWideGate : Family (List GateStep) (List GateObservation) where
  inputs :=
    [[create "old-schedule" "owner" "trigger" .schedule true,
      query .gate "owner" "trigger" none,
      create "new-event" "owner" "trigger" .event true,
      create "terminal" "owner" "trigger" .event false,
      create "foreign-owner" "other-owner" "trigger" .event true,
      create "foreign-trigger" "owner" "other-trigger" .event true,
      query .gate "owner" "trigger" (some "old-schedule"),
      query .supersede "owner" "trigger" (some "old-schedule"),
      query .gate "owner" "trigger" (some "old-schedule"),
      query .gate "owner" "trigger" none,
      query .supersede "owner" "trigger" none,
      query .gate "owner" "trigger" none,
      query .gate "other-owner" "trigger" none,
      query .gate "owner" "other-trigger" none]]
  model := gateFold []

inductive GoalTaskAction where
  | publish
  | replaceGoal
  deriving ToJson, DecidableEq

/-- `goal` names the Goal a `replaceGoal` step installs. -/
structure GoalTaskStep where
  action : GoalTaskAction
  goal : Option String
  deriving ToJson

structure GoalTaskTrace where
  targetExisting : Bool
  currentGoal : String
  steps : List GoalTaskStep
  deriving ToJson

structure GoalTaskObservation where
  outcome : Option String
  boundGoal : Option String
  rowsChanged : Option Bool
  taskOpened : Option Bool
  assignmentRoot : Option Bool
  currentGoal : String
  deriving ToJson

def goalTaskFold (s : Triggers.Durable.GoalTaskSession) :
    List GoalTaskStep → List GoalTaskObservation
  | [] => []
  | step :: rest =>
    match step.action with
    | .publish =>
      let (next, outcome) := s.publish
      let opened := Triggers.Durable.taskOpensSession next.targetExisting
      ⟨some (outcomeWire outcome), next.admittedGoal, some (next != s), some opened,
        some opened, next.currentGoal⟩ :: goalTaskFold next rest
    | .replaceGoal =>
      let next := s.replaceGoal (step.goal.getD s.currentGoal)
      ⟨none, none, none, none, none, next.currentGoal⟩ :: goalTaskFold next rest

/-- Goal names are placeholders the consumer maps to replacement Goal
identities of the fixture session; the fire prepared the session's initial
deterministic Goal, which the session no longer holds. -/
@[conformance_family]
def goalTaskReplay : Family GoalTaskTrace (List GoalTaskObservation) where
  inputs :=
    [⟨true, "first-replacement",
      [⟨.publish, none⟩, ⟨.replaceGoal, some "later-replacement"⟩, ⟨.publish, none⟩]⟩]
  model := fun i => goalTaskFold ⟨i.targetExisting, i.currentGoal, none⟩ i.steps

end Conformance.TriggerMaterializer
