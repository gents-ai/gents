import Proofs.Store.Identity.SessionChild
import Proofs.Conformance.Family

namespace Conformance.GoalLinks

open Lean Identity.Household Identity.SessionChild

structure Step where
  parentOk : Bool
  workerOk : Bool
  markOk : Bool
  deriving ToJson

structure LinkInput where
  collection : String
  actor : String
  mode : String
  commit : Bool
  steps : List Step
  deriving ToJson

def LinkInput.of (collection actor mode : String) (commit : Bool) : LinkInput :=
  { collection, actor, mode, commit
    steps := if mode == "recovery" || mode == "partial" then
      [⟨true, false, true⟩, ⟨true, true, false⟩, ⟨true, true, true⟩] else [] }

def linkInputs : List LinkInput :=
  ["Goal", "GoalCreationClaim"].flatMap fun collection =>
    (["user", "agent"].flatMap fun actor =>
      [LinkInput.of collection actor "fresh" false,
       LinkInput.of collection actor "fresh" true,
       LinkInput.of collection actor "recovery" true,
       LinkInput.of collection actor "partial" true]) ++
    [LinkInput.of collection "user" "foreign" true]

def LinkInput.author (i : LinkInput) : String :=
  if i.mode == "foreign" then "agent" else i.actor

def LinkInput.owner (i : LinkInput) : Option String :=
  let empty : State := { objects := [], relations := [], enrolled := [] }
  let committed := if i.mode != "fresh" || i.commit then
    directCreate empty i.author "children" "goal" true true else empty
  (committed.objects.find? (fun object => object.id == "goal")).map (·.owner)

def LinkInput.initial (i : LinkInput) : GoalLinks :=
  if i.mode == "fresh" then publishGoalLinks i.commit
  else if i.mode == "partial" then ⟨true, false, false⟩ else {}

def afterSteps : GoalLinks → List Step → List GoalLinks
  | _, [] => []
  | state, step :: rest =>
    let next := repairGoalLinks state step.parentOk step.workerOk step.markOk
    next :: afterSteps next rest

def revokedAfterCompletion : GoalLinks := repairGoalLinks ⟨false, true, true⟩ true true true

/-- The Agent reads a User-authored row through its `working_agent`
relationship or through the session parent, so it observes only their union;
the session reader observes the parent alone. -/
structure UserObservation where
  linked : Bool
  parent : Bool
  agentVisible : Bool
  deriving ToJson

def UserObservation.of (state : GoalLinks) : UserObservation :=
  { linked := state.linked, parent := state.parent, agentVisible := state.worker || state.parent }

/-- An Agent-authored row is the Agent's own, so its `working_agent`
relationship has no observation of its own. -/
structure AgentObservation where
  linked : Bool
  parent : Bool
  deriving ToJson

def AgentObservation.of (state : GoalLinks) : AgentObservation :=
  { linked := state.linked, parent := state.parent }

structure UserOutput where
  owner : Option String
  initial : UserObservation
  after : List UserObservation
  afterRevocation : Option UserObservation
  deriving ToJson

structure AgentOutput where
  owner : Option String
  initial : AgentObservation
  after : List AgentObservation
  afterRevocation : Option AgentObservation
  deriving ToJson

@[conformance_family]
def userOwned : Family LinkInput UserOutput where
  inputs := linkInputs.filter fun i => i.actor == "user" && i.mode != "foreign"
  model := fun i =>
    { owner := i.owner, initial := .of i.initial
      after := (afterSteps i.initial i.steps).map .of
      afterRevocation := i.owner.map fun _ => .of revokedAfterCompletion }

@[conformance_family]
def agentOwned : Family LinkInput AgentOutput where
  inputs := linkInputs.filter fun i => i.actor == "agent"
  model := fun i =>
    { owner := i.owner, initial := .of i.initial
      after := (afterSteps i.initial i.steps).map .of
      afterRevocation := i.owner.map fun _ => .of revokedAfterCompletion }

structure ForeignOutput where
  owner : Option String
  repairAllowed : Bool
  linked : Bool
  parent : Bool
  deriving ToJson

/-- A row the Agent authored and only shared with the User as a reader: the
User may not repair it. -/
@[conformance_family]
def foreign : Family LinkInput ForeignOutput where
  inputs := linkInputs.filter fun i => i.mode == "foreign"
  model := fun i =>
    { owner := i.owner, repairAllowed := goalRepairAllowed i.actor i.author i.initial
      linked := i.initial.linked, parent := i.initial.parent }

def freshCommitted (actor : String) (collections : List String) : List LinkInput :=
  linkInputs.filter fun i =>
    i.actor == actor && i.mode == "fresh" && i.commit && collections.contains i.collection

/-- A session Goal the User creates through `create_goal_for_session` commits
the Goal and its creation claim with their links; only the committed fresh
publication is reachable there. -/
@[conformance_family]
def sessionGoal : Family LinkInput Bool where
  inputs := freshCommitted "user" ["Goal", "GoalCreationClaim"]
  model := fun i => i.initial.linked

/-- The same User session Goal created over the client HTTP transport. -/
@[conformance_family]
def clientSessionGoal : Family LinkInput Bool where
  inputs := freshCommitted "user" ["Goal", "GoalCreationClaim"]
  model := fun i => i.initial.linked

/-- A User Goal set through `seed_goal_from_access`; the scenario reads only
the Goal. -/
@[conformance_family]
def seededGoal : Family LinkInput Bool where
  inputs := freshCommitted "user" ["Goal"]
  model := fun i => i.initial.linked

/-- A goal-backed submission by the selected Agent commits the Goal, its claim
and the opening request together. -/
@[conformance_family]
def goalBackedSubmission : Family LinkInput Bool where
  inputs := freshCommitted "agent" ["Goal", "GoalCreationClaim"]
  model := fun i => i.initial.linked

/-- A goal Task fire materializes the Agent's Goal and claim with its first
request. -/
@[conformance_family]
def taskFire : Family LinkInput Bool where
  inputs := freshCommitted "agent" ["Goal", "GoalCreationClaim"]
  model := fun i => i.initial.linked

/-- A scheduled goal Task fire publishes the Agent's Goal and claim
atomically with its request. -/
@[conformance_family]
def scheduledTaskFire : Family LinkInput Bool where
  inputs := freshCommitted "agent" ["Goal", "GoalCreationClaim"]
  model := fun i => i.initial.linked

/-- A workspace-bound goal Task fire; the scenario reads only the Goal. -/
@[conformance_family]
def workspaceTaskFire : Family LinkInput Bool where
  inputs := freshCommitted "agent" ["Goal"]
  model := fun i => i.initial.linked

/-- A nested Agent target creates a Goal and claim for the target session. -/
@[conformance_family]
def agentTarget : Family LinkInput Bool where
  inputs := freshCommitted "agent" ["Goal", "GoalCreationClaim"]
  model := fun i => i.initial.linked

/-- A session-message closeout binds a Goal to its caused request; the scenario
reads only the Goal. -/
@[conformance_family]
def closeoutGoal : Family LinkInput Bool where
  inputs := freshCommitted "agent" ["Goal"]
  model := fun i => i.initial.linked

end Conformance.GoalLinks
