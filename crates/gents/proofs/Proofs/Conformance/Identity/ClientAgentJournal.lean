import Proofs.ToolCalls.Identity.ClientAgentJournal
import Proofs.Conformance.Family

namespace Conformance.ClientAgentJournal

open Lean Identity.ClientAgentJournal

deriving instance ToJson for Stage, Status, Journal, RecoveryDisposition, HostReading

inductive ActionKind where
  | record | dispatch | finish | resume | observe | restart | saveUnrelated
  deriving ToJson

/-- `stage` is the target of record, dispatch and finish and is ignored by the
other actions; `tokenValid` is read only by resume and `reading` only by
observe. -/
structure ActionStep where
  action : ActionKind
  stage : Stage
  durable : Bool
  success : Bool
  confirmedFailure : Bool
  tokenValid : Bool
  reading : HostReading
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

private def blank (action : ActionKind) (stage : Stage) : ActionStep :=
  { action, stage, durable := false, success := false, confirmedFailure := false,
    tokenValid := false, reading := .absent }

private def stepOf : Action → ActionStep
  | .record stage durable => { blank .record stage with durable }
  | .dispatch stage => blank .dispatch stage
  | .finish stage success confirmedFailure =>
    { blank .finish stage with success, confirmedFailure }
  | .resume tokenValid => { blank .resume .host with tokenValid }
  | .observe reading => { blank .observe .host with reading }
  | .restart => blank .restart .commit
  | .saveUnrelated => blank .saveUnrelated .commit

private def actionOf (step : ActionStep) : Action :=
  match step.action with
  | .record => .record step.stage step.durable
  | .dispatch => .dispatch step.stage
  | .finish => .finish step.stage step.success step.confirmedFailure
  | .resume => .resume step.tokenValid
  | .observe => .observe step.reading
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
  host : Status
  tokenValid : Bool
  reading : HostReading
  deriving ToJson

@[conformance_family]
def recovery : Family RecoveryInput RecoveryDisposition where
  inputs := recoveryCases.map fun c =>
    { name := c.name, actorMatches := c.actorMatches, recordPresent := c.recordPresent
      bindingMatches := c.bindingMatches, commit := c.commit, parent := c.parent
      executor := c.executor, host := c.host, tokenValid := c.tokenValid, reading := c.reading }
  model := fun i =>
    recoveryDisposition i.actorMatches i.recordPresent i.bindingMatches i.tokenValid i.reading
      { Identity.ClientAgentJournal.initial with
        commit := i.commit, parent := i.parent, executor := i.executor, host := i.host }

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
