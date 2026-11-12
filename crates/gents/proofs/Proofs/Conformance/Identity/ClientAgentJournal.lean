import Proofs.ToolCalls.Identity.ClientAgentJournal
import Proofs.Conformance.Family

namespace Conformance.ClientAgentJournal

open Lean Identity.ClientAgentJournal

deriving instance ToJson for Stage, Status, Journal

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

structure PrivateAccessInput where
  role : String
  action : String
  deriving ToJson

structure PrivateAccess where
  allowed : Bool
  policyYaml : String
  deriving ToJson

@[conformance_family]
def privateAccess : Family PrivateAccessInput PrivateAccess where
  inputs := privateAccessCases.map fun (role, action, _) => { role, action }
  model := fun i =>
    let actor := match i.role with
      | "owner" => "user" | "anonymous" => "" | role => role
    { allowed := Identity.Household.allowedIn privatePolicy 5 privateFixture actor "journals" "record" i.action
      policyYaml := Identity.Household.policyYamlOf privatePolicy }

structure SignerInput where
  role : String
  deriving ToJson

@[conformance_family]
def localSigner : Family SignerInput Bool where
  inputs := ["client", "other", "empty"].map fun role => { role }
  model := fun i =>
    let actor := if i.role == "client" then "user" else if i.role == "other" then "other" else ""
    localSignerMatches actor "user"

end Conformance.ClientAgentJournal
