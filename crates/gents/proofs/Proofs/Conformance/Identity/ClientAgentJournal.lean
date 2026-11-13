import Proofs.ToolCalls.Identity.ClientAgentJournal
import Proofs.Conformance.Family

namespace Conformance.ClientAgentJournal

open Lean Identity.ClientAgentJournal

deriving instance ToJson for Stage, Status, Journal, RecoveryDisposition

inductive ActionKind where
  | record | dispatch | finish | restart | saveUnrelated
  deriving ToJson

/-- `stage` is the target of record, dispatch and finish and is ignored by the
other actions. -/
structure ActionStep where
  action : ActionKind
  stage : Stage
  durable : Bool
  success : Bool
  confirmedFailure : Bool
  deriving ToJson

structure Input where
  name : String
  initial : Journal
  actions : List ActionStep
  deriving ToJson

structure Step where
  before : Journal
  after : Journal
  deriving ToJson

private def stepOf : Action → ActionStep
  | .record stage durable =>
    { action := .record, stage, durable, success := false, confirmedFailure := false }
  | .dispatch stage =>
    { action := .dispatch, stage, durable := false, success := false, confirmedFailure := false }
  | .finish stage success confirmedFailure =>
    { action := .finish, stage, durable := false, success, confirmedFailure }
  | .restart =>
    { action := .restart, stage := .commit, durable := false, success := false,
      confirmedFailure := false }
  | .saveUnrelated =>
    { action := .saveUnrelated, stage := .commit, durable := false, success := false,
      confirmedFailure := false }

private def actionOf (step : ActionStep) : Action :=
  match step.action with
  | .record => .record step.stage step.durable
  | .dispatch => .dispatch step.stage
  | .finish => .finish step.stage step.success step.confirmedFailure
  | .restart => .restart
  | .saveUnrelated => .saveUnrelated

private def run : Journal → List Action → List Step
  | _, [] => []
  | before, action :: rest =>
    let after := applyAction before action
    { before, after } :: run after rest

@[conformance_family]
def publicationTraces : Family Input (List Step) where
  inputs := Identity.ClientAgentJournal.cases.map fun (name, actions) =>
    { name, initial := Identity.ClientAgentJournal.initial, actions := actions.map stepOf }
  model := fun i => run i.initial (i.actions.map actionOf)

structure RecoveryInput where
  name : String
  actorMatches : Bool
  recordPresent : Bool
  bindingMatches : Bool
  commit : Status
  parent : Status
  executor : Status
  deriving ToJson

@[conformance_family]
def recovery : Family RecoveryInput RecoveryDisposition where
  inputs := recoveryCases.map fun c =>
    { name := c.name, actorMatches := c.actorMatches, recordPresent := c.recordPresent
      bindingMatches := c.bindingMatches, commit := c.commit, parent := c.parent
      executor := c.executor }
  model := fun i =>
    recoveryDisposition i.actorMatches i.recordPresent i.bindingMatches
      { Identity.ClientAgentJournal.initial with
        commit := i.commit, parent := i.parent, executor := i.executor }

end Conformance.ClientAgentJournal
