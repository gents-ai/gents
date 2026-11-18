import Proofs.Conformance.Identity.Conformance
import Proofs.Conformance.Identity.HouseholdStore
import Proofs.Conformance.Family

namespace Conformance.Household

open Lean Identity Identity.Household Identity.Conformance

structure TraceObject where
  resource : String
  id : String
  owner : String
  deriving ToJson

structure TraceSubject where
  actor : Option String
  wildcard : Bool
  resource : Option String
  id : Option String
  targetRelation : Option String
  deriving ToJson

structure TraceRelation where
  resource : String
  id : String
  relation : String
  subject : TraceSubject
  deriving ToJson

structure TraceStep where
  action : String
  actor : Option String
  relation : Option TraceRelation
  resource : Option String
  id : Option String
  permission : Option String
  deriving ToJson

structure TraceActorRole where
  actor : String
  role : String
  deriving ToJson

structure PolicyTraceInput where
  name : String
  objects : List TraceObject
  relations : List TraceRelation
  enrolled : List String
  roles : List TraceActorRole
  steps : List TraceStep
  deriving ToJson

def TraceObject.ofModel (object : Object) : TraceObject := ⟨object.resource, object.id, object.owner⟩

def TraceObject.toModel (object : TraceObject) : Object := ⟨object.resource, object.id, object.owner⟩

def TraceSubject.ofModel : Subject → TraceSubject
  | .wildcard => ⟨none, true, none, none, none⟩
  | .actor did => ⟨some did, false, none, none, none⟩
  | .object resource id => ⟨none, false, some resource, some id, none⟩
  | .entitySet resource id relation => ⟨none, false, some resource, some id, some relation⟩

def TraceSubject.toModel (subject : TraceSubject) : Subject :=
  match subject.actor, subject.resource, subject.id, subject.targetRelation with
  | some did, _, _, _ => .actor did
  | none, some resource, some id, some relation => .entitySet resource id relation
  | none, some resource, some id, none => .object resource id
  | _, _, _, _ => .wildcard

def TraceRelation.ofModel (relation : Relation) : TraceRelation :=
  ⟨relation.resource, relation.id, relation.relation, TraceSubject.ofModel relation.subject⟩

def TraceRelation.toModel (relation : TraceRelation) : Relation :=
  ⟨relation.resource, relation.id, relation.relation, relation.subject.toModel⟩

def TraceStep.ofModel : Action → TraceStep
  | .grant relation => ⟨"grant", none, some (TraceRelation.ofModel relation), none, none, none⟩
  | .revoke relation => ⟨"revoke", none, some (TraceRelation.ofModel relation), none, none, none⟩
  | .grantAs actor relation => ⟨"grant_as", some actor, some (TraceRelation.ofModel relation), none, none, none⟩
  | .revokeAs actor relation => ⟨"revoke_as", some actor, some (TraceRelation.ofModel relation), none, none, none⟩
  | .check actor resource id permission =>
      ⟨"check", some actor, none, some resource, some id, some permission⟩

def TraceStep.toModel (step : TraceStep) : Option Action :=
  match step.action, step.actor, step.relation, step.resource, step.id, step.permission with
  | "grant", _, some relation, _, _, _ => some (.grant relation.toModel)
  | "revoke", _, some relation, _, _, _ => some (.revoke relation.toModel)
  | "grant_as", some actor, some relation, _, _, _ => some (.grantAs actor relation.toModel)
  | "revoke_as", some actor, some relation, _, _, _ => some (.revokeAs actor relation.toModel)
  | "check", some actor, _, some resource, some id, some permission =>
      some (.check actor resource id permission)
  | _, _, _, _, _, _ => none

def traceRole (actor : String) : String :=
  if actor == otherAgentDid then "other-agent" else if actor == readerDid then "reader"
  else if actor == strangerDid then "stranger" else if actor == otherRuntimeDid then "other-runtime"
  else roleOf actor

def traceInput (name : String) (state : State) (actions : List Action) : PolicyTraceInput :=
  { name, objects := state.objects.map TraceObject.ofModel,
    relations := state.relations.map TraceRelation.ofModel, enrolled := state.enrolled,
    roles := [userDid, runtimeDid, otherRuntimeDid, agentDid, siblingDid, otherAgentDid, readerDid,
      strangerDid].map fun actor => { actor, role := traceRole actor },
    steps := actions.map TraceStep.ofModel }

def stepOutcome (state : State) : Action → Option Bool
  | .grant _ | .revoke _ => none
  | .grantAs actor relation | .revokeAs actor relation => some (mayManage state actor relation)
  | .check actor resource id permission => some (allowed 5 state actor resource id permission)

def traceOutcomes (state : State) : List Action → List (Option Bool)
  | [] => []
  | action :: rest => stepOutcome state action :: traceOutcomes (apply state action) rest

/-- A step the wire cannot name is replayed as a check nobody may pass, so a
malformed step is visible as a mismatch rather than skipped. -/
def PolicyTraceInput.outcomes (i : PolicyTraceInput) : List (Option Bool) :=
  let state : State := ⟨i.objects.map TraceObject.toModel, i.relations.map TraceRelation.toModel, i.enrolled⟩
  traceOutcomes state (i.steps.map fun step => step.toModel.getD (.check "" "" "" ""))

def householdActions : List Action :=
  HouseholdFixture.nodeAdministratorChanges ++ HouseholdFixture.runtimeReaderChanges ++
    HouseholdFixture.invokeChanges ++ HouseholdFixture.invokeOnlySessionActions ++
    HouseholdFixture.requestSessionChanges ++ HouseholdFixture.receiptAuthorChanges ++
    HouseholdFixture.matrix ++ HouseholdFixture.managementChanges ++ HouseholdFixture.selfConfigChanges ++
    HouseholdFixture.changes

def configurationChecks : List Action :=
  [userDid, agentDid, siblingDid, runtimeDid].flatMap fun actor =>
    ["selected", "agent-created"].flatMap fun id =>
      ["read", "update", "delete"].map fun permission => .check actor "configuration" id permission

def configurationActions : List Action :=
  configurationChecks ++ [.grant ConfigurationFixture.reader] ++ configurationChecks ++
    [.grant ConfigurationFixture.writer] ++ configurationChecks ++ [.revoke ConfigurationFixture.writer] ++
    configurationChecks ++ [.revoke ConfigurationFixture.reader] ++ configurationChecks

def configurationTrace : PolicyTraceInput :=
  traceInput "configuration-explicit-grants" ConfigurationFixture.initial configurationActions

/-- Every household fixture's trace, replayed against native document ACP
with the production policy installed. -/
@[conformance_family]
def policyTrace : Family PolicyTraceInput (List (Option Bool)) where
  inputs := [traceInput "user-node-agents-mixed-session-ownership-policy-followup"
      HouseholdFixture.initial householdActions,
    traceInput "operations-parent-publication" OperationsFixture.initial OperationsFixture.actions,
    configurationTrace,
    traceInput "secret-configuration-publication" SecretConfigurationFixture.initial
      SecretConfigurationFixture.actions]
  model := PolicyTraceInput.outcomes

/-- The explicit configuration grants, replayed through authenticated native
relationship writes on a served node. -/
@[conformance_family]
def configurationGrantTrace : Family PolicyTraceInput (List (Option Bool)) where
  inputs := [configurationTrace]
  model := PolicyTraceInput.outcomes

/-- Self-configuration grants on the household fixture, replayed through
authenticated native relationship writes on a served node. -/
@[conformance_family]
def selfConfigTrace : Family PolicyTraceInput (List (Option Bool)) where
  inputs := [traceInput "self-config" HouseholdFixture.initial HouseholdFixture.selfConfigChanges]
  model := PolicyTraceInput.outcomes

structure PolicyDefinitionInput where
  name : String
  deriving ToJson

@[conformance_family]
def policyDefinition : Family PolicyDefinitionInput String where
  inputs := [{ name := "agent-ownership" }]
  model := fun _ => policyYaml

structure PolicyRegistrationInput where
  name : String
  candidateYaml : String
  deriving ToJson

structure PolicyRegistration where
  sameId : Bool
  accepted : Bool
  deriving ToJson

def policyCandidates : List (String × List ResourceDefinition) :=
  [("identical-policy", policy), ("management-metadata-collision", policyWithDifferentManagement),
    ("relation-type-collision", policyWithDifferentTypes)]

@[conformance_family]
def policyRegistration : Family PolicyRegistrationInput PolicyRegistration where
  inputs := policyCandidates.map fun (name, candidate) => { name, candidateYaml := policyYamlOf candidate }
  model := fun i =>
    let candidate := (policyCandidates.find? (·.1 == i.name)).map (·.2)
    { sameId := candidate.any fun candidate => policyIdInput policy == policyIdInput candidate
      accepted := (registerPolicy (some policyYaml) i.candidateYaml).isSome }

structure TraceCheckInput where
  actor : String
  role : String
  resource : String
  id : String
  permission : String
  occurrence : Nat
  deriving ToJson

def TraceCheckInput.hits (i : TraceCheckInput) (actor resource id permission : String) : Bool :=
  actor == i.actor && resource == i.resource && id == i.id && permission == i.permission

/-- The modeled answer to the `occurrence`-th matching check along `actions`
from `state`: the answer at the point of the trace where that check is made. -/
def nthCheck (state : State) (i : TraceCheckInput) (occurrence : Nat) : List Action → Option Bool
  | [] => none
  | action :: rest =>
    match action with
    | .check actor resource id permission =>
      if i.hits actor resource id permission then
        if occurrence == 0 then some (allowed 5 state actor resource id permission)
        else nthCheck (apply state action) i (occurrence - 1) rest
      else nthCheck (apply state action) i occurrence rest
    | _ => nthCheck (apply state action) i occurrence rest

def traceCheck (actor resource id permission : String) (occurrence : Nat := 0) : TraceCheckInput :=
  { actor, role := traceRole actor, resource, id, permission, occurrence }

/-- An autonomous session is read by its working Agent, the User and the
sibling Agent at the household trace's first such check. -/
@[conformance_family]
def automatedSessionRead : Family TraceCheckInput (Option Bool) where
  inputs := [agentDid, userDid, siblingDid].map fun actor =>
    traceCheck actor "sessions" "automated-session" "read"
  model := fun i => nthCheck HouseholdFixture.initial i i.occurrence householdActions

/-- The invoke-only caller's observations along the invoke-only session trace:
its invoke of the sibling Agent, its reads of a foreign and of a nested session
(the foreign read before, during and after a direct reader grant), the sibling's
read of its own session, and the caller's foreign update. -/
@[conformance_family]
def invokeSessionCheck : Family TraceCheckInput (Option Bool) where
  inputs := [traceCheck agentDid "agents" "sibling" "invoke",
    traceCheck agentDid "sessions" "sibling-session" "read",
    traceCheck agentDid "sessions" "nested-session" "read",
    traceCheck siblingDid "sessions" "sibling-session" "read",
    traceCheck agentDid "sessions" "sibling-session" "read" 1,
    traceCheck agentDid "sessions" "sibling-session" "read" 2,
    traceCheck agentDid "sessions" "sibling-session" "update"]
  model := fun i => nthCheck HouseholdFixture.initial i i.occurrence HouseholdFixture.invokeOnlySessionActions

/-- The first Node administrator granted along the household trace. -/
def firstAdministrator : List Action → Option String
  | [] => none
  | .grant ⟨"nodes", _, "administrator", .actor actor⟩ :: _ => some actor
  | _ :: rest => firstAdministrator rest

/-- The modeled answer to a check by the Node administrator after its grant. -/
def afterGrant (state : State) (i : TraceCheckInput) : Bool → List Action → Option Bool
  | _, [] => none
  | granted, action :: rest =>
    match action with
    | .grant ⟨"nodes", _, "administrator", .actor actor⟩ =>
      afterGrant (apply state action) i (granted || actor == i.actor) rest
    | .check actor resource id permission =>
      if granted && i.hits actor resource id permission then
        some (allowed 5 state actor resource id permission)
      else afterGrant (apply state action) i granted rest
    | _ => afterGrant (apply state action) i granted rest

/-- A live two-Agent node: the working Agent updating its own session, an
invoke-only caller updating a foreign session, and the Engineer (the Node
administrator) reading sessions and delivered requests. -/
@[conformance_family]
def liveSessionUpdate : Family TraceCheckInput (Option Bool) where
  inputs := [traceCheck agentDid "sessions" "general-session" "update",
    traceCheck agentDid "sessions" "sibling-session" "update"]
  model := fun i =>
    if i.id == "general-session" then nthCheck HouseholdFixture.initial i i.occurrence householdActions
    else nthCheck HouseholdFixture.initial i i.occurrence HouseholdFixture.invokeOnlySessionActions

@[conformance_family]
def liveAdministratorRead : Family TraceCheckInput (Option Bool) where
  inputs := match firstAdministrator householdActions with
    | some administrator => [traceCheck administrator "sessions" "sibling-session" "read",
        traceCheck administrator "delivered_requests" "remote-request" "read"]
    | none => []
  model := fun i => afterGrant HouseholdFixture.initial i false householdActions

/-- A live delegated child: the caller's invoke after its session_writer
relationship is revoked. The granted phase is observed natively by
`agentManagerInvokeNative`. -/
@[conformance_family]
def agentManagerInvokeLive : Family InvokePhaseInput Bool where
  inputs := [⟨"after"⟩]
  model := agentInvokeAllowed

structure SelfConfigLookupInput where
  name : String
  present : Bool
  visible : Bool
  sameGeneration : Bool
  nativeAllows : Bool
  deriving ToJson

def selfConfigLookupInputs : List SelfConfigLookupInput :=
  HouseholdFixture.selfConfigLookupCases.map fun (name, attempt) =>
    { name, present := attempt.present, visible := attempt.visible,
      sameGeneration := attempt.sameGeneration, nativeAllows := attempt.nativeAllows }

def selfConfigLookupOutcome (i : SelfConfigLookupInput) : String :=
  selfConfigOutcome ⟨i.present, i.visible, i.sameGeneration, i.nativeAllows⟩

@[conformance_family]
def selfConfigLookup : Family SelfConfigLookupInput String where
  inputs := selfConfigLookupInputs.filter fun i => i.sameGeneration || !i.present
  model := selfConfigLookupOutcome

/-- model-production-gap: native document ACP cannot fence a document
replacement between the self-config lookup and its relationship grant, so a
lookup whose Agent document changed generation in between cannot be driven. -/
@[conformance_family]
def selfConfigLookupUnreached : Family SelfConfigLookupInput String where
  inputs := selfConfigLookupInputs.filter fun i => !i.sameGeneration && i.present
  model := selfConfigLookupOutcome

structure AtomicCommitInput where
  commit : Bool
  deriving ToJson

structure AtomicReads where
  ownerRead : Bool
  agentRead : Bool
  siblingRead : Bool
  deriving ToJson

@[conformance_family]
def configurationAtomic : Family AtomicCommitInput AtomicReads where
  inputs := [⟨false⟩, ⟨true⟩]
  model := fun i =>
    let state := Configuration.publishSelectedAtomic ConfigurationFixture.atomicInitial userDid "atomic"
      "home" agentDid i.commit
    { ownerRead := Household.allowed 5 state userDid "configuration" "atomic" "read"
      agentRead := Household.allowed 5 state agentDid "configuration" "atomic" "read"
      siblingRead := Household.allowed 5 state siblingDid "configuration" "atomic" "read" }

end Conformance.Household
