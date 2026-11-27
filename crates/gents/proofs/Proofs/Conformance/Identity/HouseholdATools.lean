import Proofs.Conformance.Identity.Conformance
import Proofs.Conformance.Family
import Proofs.Store.Identity.GrantSets

namespace Conformance.HouseholdA

open Lean Identity Identity.Household Identity.Conformance

structure ToolOperationInput where
  name : String
  schema : Bool
  history : Bool
  goal : Bool
  create : Bool
  selfConfig : Bool
  category : String
  pack : Bool
  query : Bool
  write : Bool
  memory : Bool
  deriving ToJson, Inhabited

structure OperationSource where
  purpose : String
  permission : String
  deriving ToJson

def toolOperationSurface (i : ToolOperationInput) : ToolPolicy.Surface :=
  if i.query || i.write then
    let scope : ToolPolicy.EndpointScope (String × String) (Finset String) :=
      .only {("tool", "collection")} (fun _ => ∅)
    { ToolPolicy.ContractCases.secureMinimal with
      queryTools := if i.query then scope else .none
      writeTools := if i.write then scope else .none }
  else
    { ToolPolicy.ContractCases.secureMinimal with
      schemaManagement := i.schema
      sessionHistory := i.history
      memory := i.memory
      goalTools := i.goal
      goalCreate := i.create
      selfConfig := i.selfConfig
      queryTools := .none
      writeTools := .none
      selfConfigCategories := if i.category == "all" then .all else if i.category == "none" then .none
        else .only {i.category} (fun _ => ()) }

def toolOperationSources (i : ToolOperationInput) : List OperationSource :=
  (Configuration.toolOperationSources (toolOperationSurface i) i.pack).map
    fun (purpose, permission) => { purpose, permission }

def operation (name : String) (schema history goal create selfConfig pack : Bool)
    (category : String) : ToolOperationInput :=
  { name, schema, history, goal, create, selfConfig, category, pack, query := false, write := false,
    memory := false }

def memoryOperation (name : String) : ToolOperationInput :=
  { name, schema := false, history := false, goal := false, create := false,
    selfConfig := false, category := "none", pack := false, query := false, write := false,
    memory := true }

def datastoreOperation (name : String) (query write : Bool) : ToolOperationInput :=
  { name, schema := false, history := false, goal := false, create := false,
    selfConfig := false, category := "none", pack := false, query, write, memory := false }

def toolOperationInputs : List ToolOperationInput :=
  [operation "none" false false false false false false "none",
   operation "schema" true false false false false false "none",
   operation "session-history" false true false false false false "none",
   memoryOperation "memory",
   operation "goal" false false true false false false "none",
   operation "goal-create-without-goal" false false false true false false "none",
   operation "goal-and-create" false false true true false false "none",
   operation "self-config-without-categories" false false false false true true "none",
   operation "self-config-all" false false false false true false "all",
   operation "self-config-agent" false false false false true false "agent",
   operation "self-config-node" false false false false true false "node",
   operation "self-config-tools" false false false false true false "tools",
   operation "self-config-profile" false false false false true false "profile",
   operation "self-config-backend" false false false false true false "backend",
   operation "self-config-mcp-service" false false false false true false "mcp_service",
   operation "self-config-automation" false false false false true false "automation",
   operation "pack-independent" false false false false true true "backend",
   operation "schema-and-pack-overlap" true false false false true true "all",
   operation "all" true true true true true true "all",
   datastoreOperation "datastore-query" true false,
   datastoreOperation "datastore-write" false true,
   datastoreOperation "datastore-overlap" true true]

structure OperationStep where
  step : String
  operation : ToolOperationInput
  deriving ToJson

def grantsPermission (permission : String) (i : OperationStep) : Bool :=
  (toolOperationSources i.operation).any fun source => source.permission == permission

/-- Whether the live Agent holds `permission` at this step: its baseline and
the step's tool sources (`GrantSets.agentEffective`). -/
def liveAgentHolds (permission : String) (i : OperationStep) : Bool :=
  GrantSets.agentEffective ⟨true, true, true⟩
    ((toolOperationSources i.operation).map (·.permission)) [] permission

structure SourceChangeInput where
  step : String
  present : Bool
  delete : Bool
  authorized : Bool
  deriving ToJson

structure GrantorInput where
  step : String
  grantor : String
  committed : Bool
  deriving ToJson

/-- Whether the grantor's Tools edit leaves the Agent holding a schema tool
source. The admin is the native administrator; the writer holds only a Tools
writer relationship. -/
def grantorGrants (i : GrantorInput) : Bool :=
  let initial : Configuration.ToolGrantProjection := { admins := [userDid] }
  let grantor := if i.grantor == "admin" then userDid else agentDid
  !(Configuration.configureToolGrantGroup initial ConfigurationFixture.toolGrantorHousehold
    ConfigurationFixture.schemaToolSources
    grantor true i.committed).grants.isEmpty

structure ManagedIdentityInput where
  claimedKey : Option String
  derivedNodeKey : Option String
  derivedCreation : Option String
  nodeKey : String
  creationNonce : Option String
  /-- The native permissions the creating User holds at each of its create
  attempts; the attempts stop at the first that commits. -/
  creatorHeldSteps : List (List String)
  deriving ToJson

/-- The creating User's direct grants in the served scenario. -/
def creatorBaseHeld : List String := ["get-collection", "read-document", "update-document"]

/-- The User first creates with its direct grants, then again once a legacy
tool source and a direct grant also give it `patch-collection`. -/
def servedCreatorHeldSteps : List (List String) :=
  [creatorBaseHeld, creatorBaseHeld ++ ["patch-collection"]]

def ManagedIdentityInput.ofCase : Option ClaimedDid × String × Option String → ManagedIdentityInput
  | (claimed, nodeKey, creationNonce) =>
    match claimed with
    | some (.key name) => ⟨some name, none, none, nodeKey, creationNonce, servedCreatorHeldSteps⟩
    | some (.derived derivedNodeKey creation) =>
      ⟨none, some derivedNodeKey, some creation, nodeKey, creationNonce, servedCreatorHeldSteps⟩
    | none => ⟨none, none, none, nodeKey, creationNonce, servedCreatorHeldSteps⟩

def ManagedIdentityInput.toCase (i : ManagedIdentityInput) :
    Option ClaimedDid × String × Option String :=
  let claimed : Option ClaimedDid := match i.claimedKey, i.derivedNodeKey, i.derivedCreation with
    | some name, _, _ => some (.key name)
    | none, some nodeKey, some creation => some (.derived nodeKey creation)
    | _, _, _ => none
  (claimed, i.nodeKey, i.creationNonce)

def managedIdentityAllowed : Option ClaimedDid × String × Option String → Bool
  | (claimed, nodeKey, creation) =>
    Configuration.managedToolIdentityAllowed renderedDerivation
      ((claimed.map ClaimedDid.render).getD "") nodeKey creation

structure ManagedIdentityOutcome where
  granted : Bool
  userCreatesRefused : List Bool
  deriving ToJson, DecidableEq

/-- The served Tools enable the schema tool, so a managed row is owed its
permissions. -/
def managedIdentityOwed (i : ManagedIdentityInput) : List String :=
  match i.toCase with
  | (claimed, nodeKey, creation) =>
    Configuration.owedToolSourcePermissions renderedDerivation
      ((claimed.map ClaimedDid.render).getD "") nodeKey creation Configuration.schemaToolPermissions

/-- The User's create attempts, one per held set, stopping at the first that
commits (`Configuration.agentCreateCommits`). -/
def userCreatesRefused (owed : List String) : List (List String) → List Bool
  | [] => []
  | held :: rest =>
    if Configuration.agentCreateCommits { admin := false, held } owed then [false]
    else true :: userCreatesRefused owed rest

/-- When every User attempt is refused the node owner, an administrator,
creates the row; `granted` is whether the row's identity then holds the schema
tool's `patch-collection` through the projection. -/
def managedIdentityOutcome (i : ManagedIdentityInput) : ManagedIdentityOutcome :=
  let owed := managedIdentityOwed i
  let refused := userCreatesRefused owed i.creatorHeldSteps
  let creator : GrantSets.Holder :=
    if refused.all id then { admin := true, held := [] }
    else { admin := false, held := (i.creatorHeldSteps.drop (refused.length - 1)).headD [] }
  { granted := (Configuration.agentCreateGrants creator owed).contains "patch-collection"
    userCreatesRefused := refused }

/-- The served scenario's decisions: only the derived identity is owed a grant;
its User creates are refused even once the User holds `patch-collection`,
because adding a native relation is an administrator operation it lacks; the
owner's create grants it. Every foreign or underived row commits on the User's
first attempt and grants nothing. -/
theorem managed_identity_outcomes :
    (managedToolIdentityCases.map fun c => managedIdentityOutcome (ManagedIdentityInput.ofCase c)) =
      [⟨true, [true, true]⟩, ⟨false, [false]⟩, ⟨false, [false]⟩, ⟨false, [false]⟩,
       ⟨false, [false]⟩, ⟨false, [false]⟩, ⟨false, [false]⟩] := by
  native_decide

structure HealthCheckInput where
  role : String
  permission : String
  parented : Bool
  deriving ToJson

def healthActor (role : String) : String :=
  if role == "user" then userDid else if role == "runtime" then runtimeDid
  else if role == "agent" then agentDid else if role == "sibling" then siblingDid
  else otherAgentDid

def healthAllowed (i : HealthCheckInput) : Bool :=
  let parent : Relation := ⟨"operations", "readiness", "parent", .object "nodes" "home"⟩
  let attached := Household.apply OperationsFixture.initial (.grantAs runtimeDid parent)
  let state := if i.parented then attached else Household.apply attached (.revokeAs runtimeDid parent)
  Household.allowed 5 state (healthActor i.role) "operations" "readiness" i.permission

structure DatastoreWriteInput where
  name : String
  sourceValid : Bool
  responseConfirmed : Bool
  commit : Bool
  deriving ToJson

def datastoreWriteInputs : List DatastoreWriteInput :=
  [⟨"committed", true, true, true⟩, ⟨"mutation-failed", true, false, false⟩,
   ⟨"unconfirmed", true, false, true⟩, ⟨"source-revoked", false, true, true⟩]

def datastoreWriteCommitted (i : DatastoreWriteInput) : Bool :=
  Configuration.cachedDatastoreWriteCommitted i.sourceValid i.responseConfirmed i.commit

end Conformance.HouseholdA

namespace Conformance.Household

open Lean Identity Identity.Household Identity.Conformance Conformance.HouseholdA

@[conformance_family]
def toolOperationSourceProjection : Family ToolOperationInput (List OperationSource) where
  inputs := toolOperationInputs
  model := toolOperationSources

/-- The projection scenario: the Agent with a selected datastore query, a
sibling with no Tools, the Agent with only the schema tool (its surface
disabled), then neither. -/
@[conformance_family]
def toolOperationCollectionGet : Family OperationStep Bool where
  inputs := [⟨"agent-query", datastoreOperation "datastore-query" true false⟩,
    ⟨"sibling", operation "none" false false false false false false "none"⟩,
    ⟨"agent-schema", operation "schema" true false false false false false "none"⟩,
    ⟨"agent-disabled", operation "none" false false false false false false "none"⟩]
  model := liveAgentHolds "get-collection"

@[conformance_family]
def toolOperationCollectionPatch : Family OperationStep Bool where
  inputs := toolOperationCollectionGet.inputs
  model := liveAgentHolds "patch-collection"

/-- The delete-source scenario edits one Agent's self-config categories and
pack installation in this order; the last step follows the revocation of an
independent direct grant. -/
@[conformance_family]
def toolOperationDocumentDelete : Family OperationStep Bool where
  inputs := [⟨"agent", operation "self-config-agent" false false false false true false "agent"⟩,
    ⟨"tools-pack", operation "self-config-tools" false false false false true true "tools"⟩,
    ⟨"tools", operation "self-config-tools" false false false false true false "tools"⟩,
    ⟨"agent-pack", operation "pack-independent" false false false false true true "agent"⟩,
    ⟨"agent-only", operation "self-config-agent" false false false false true false "agent"⟩,
    ⟨"direct-revoked", operation "self-config-agent" false false false false true false "agent"⟩]
  model := grantsPermission "delete-document"

/-- The schema-tool scenario: a non-admin Tools writer and the owner edit the
schema tool in this order. -/
@[conformance_family]
def toolSourceChangeSchema : Family SourceChangeInput Bool where
  inputs := [⟨"writer-control", false, true, false⟩, ⟨"writer-elevated", false, false, false⟩,
    ⟨"owner-enable", false, false, true⟩, ⟨"writer-noop", true, false, false⟩,
    ⟨"writer-disable", true, true, false⟩, ⟨"owner-disable", true, true, true⟩,
    ⟨"writer-dormant", false, false, false⟩]
  model := fun i => Configuration.sourceChangeAllowed i.present i.delete i.authorized

/-- The reviewed plugin scenario: a non-admin restages its present roots, then
the root and its membership go missing and only the owner restores them. -/
@[conformance_family]
def toolSourceChangePluginDelete : Family SourceChangeInput Bool where
  inputs := [⟨"repeat", true, false, false⟩, ⟨"missing-root", false, false, false⟩,
    ⟨"repair-root", false, false, true⟩, ⟨"missing-member", false, false, false⟩,
    ⟨"repair-member", false, false, true⟩]
  model := fun i => Configuration.sourceChangeAllowed i.present i.delete i.authorized

/-- A self-config preview on Tools whose sources are already selected projects
no new source. -/
@[conformance_family]
def toolSourceChangePreview : Family SourceChangeInput Bool where
  inputs := [⟨"preview", true, false, false⟩]
  model := fun i => Configuration.sourceChangeAllowed i.present i.delete i.authorized

/-- The embedded scenario: the non-admin Tools writer enables the schema tool,
the owner's enable is rolled back, then the owner enables it. -/
@[conformance_family]
def toolGrantorEmbedded : Family GrantorInput Bool where
  inputs := [⟨"writer", "writer", true⟩, ⟨"owner-rolled-back", "admin", false⟩,
    ⟨"owner", "admin", true⟩]
  model := grantorGrants

/-- The HTTP scenario drives only the non-admin Tools writer. -/
@[conformance_family]
def toolGrantorHttp : Family GrantorInput Bool where
  inputs := [⟨"writer", "writer", true⟩]
  model := grantorGrants

/-- The user-signed scenario installs the User's legacy source at the first
attempt whose modeled holdings include `patch-collection`, so the rows whose
first User attempt commits come first. -/
@[conformance_family]
def managedToolIdentity : Family ManagedIdentityInput ManagedIdentityOutcome where
  inputs := (managedToolIdentityCases.filter (!managedIdentityAllowed ·) ++
    managedToolIdentityCases.filter managedIdentityAllowed).map ManagedIdentityInput.ofCase
  model := managedIdentityOutcome

/-- The health scenario publishes under the runtime's parent grant, then the
runtime revokes the parent and republishes. -/
@[conformance_family]
def operationsHealthAccess : Family HealthCheckInput Bool where
  inputs := [⟨"user", "read", true⟩, ⟨"user", "update", true⟩, ⟨"runtime", "update", true⟩,
    ⟨"agent", "read", true⟩, ⟨"sibling", "read", true⟩, ⟨"outsider", "update", true⟩,
    ⟨"runtime", "update", false⟩, ⟨"user", "read", false⟩]
  model := healthAllowed

/-- The pure confirmation check reads only the mutation response. -/
@[conformance_family]
def datastoreWriteConfirmation : Family DatastoreWriteInput Bool where
  inputs := datastoreWriteInputs.filter fun i => i.sourceValid && i.commit
  model := datastoreWriteCommitted

/-- The cached-write scenario commits, fails its mutation, and writes after
the source is deselected and its reader revoked; a confirmed commit without
rows is only reachable through the pure confirmation check. -/
@[conformance_family]
def datastoreWriteCached : Family DatastoreWriteInput Bool where
  inputs := datastoreWriteInputs.filter fun i => i.name != "unconfirmed"
  model := datastoreWriteCommitted

end Conformance.Household
