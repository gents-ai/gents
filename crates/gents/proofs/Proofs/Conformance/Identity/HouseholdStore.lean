import Proofs.Conformance.Identity.Conformance
import Proofs.Conformance.Family

namespace Conformance.Household

open Lean Identity Identity.Household Identity.Conformance

structure DestructiveInput where
  nacEnabled : Bool
  admin : Bool
  completeInventory : Bool
  deriving ToJson

def DestructiveInput.authority (i : DestructiveInput) : Configuration.DestructiveAuthority :=
  ⟨i.nacEnabled, i.admin, if i.completeInventory then .complete else .acpFiltered⟩

def destructiveInputs : List DestructiveInput := Id.run do
  let mut cases := []
  for nacEnabled in [true, false] do
    for admin in [true, false] do
      for completeInventory in [false, true] do
        cases := cases ++ [⟨nacEnabled, admin, completeInventory⟩]
  return cases

def destructiveRefusal (i : DestructiveInput) : Option String :=
  (Configuration.destructiveAdminDecision i.authority).map Configuration.DestructiveRefusal.name

@[conformance_family]
def destructiveDecision : Family DestructiveInput (Option String) where
  inputs := destructiveInputs.filter fun i => !i.completeInventory
  model := destructiveRefusal

/-- model-production-gap: production has no complete-inventory read; every
Gents read is filtered by document ACP for its actor, so
`gents_store::config_client::InventoryRead` has only `AcpFiltered` and a
destructive decision over a complete inventory cannot be driven. -/
@[conformance_family]
def destructiveDecisionUnreached : Family DestructiveInput (Option String) where
  inputs := destructiveInputs.filter fun i => i.completeInventory
  model := destructiveRefusal

structure ReopenInput where
  marker : Bool
  store : Bool
  binding : Bool
  deriving ToJson

@[conformance_family]
def freshBackendReopen : Family ReopenInput Bool where
  inputs := [⟨true, true, true⟩, ⟨false, true, true⟩, ⟨false, true, false⟩,
    ⟨true, false, true⟩, ⟨true, true, false⟩]
  model := fun i => Configuration.freshBackendReopenAllowed i.marker i.store i.binding

structure CollectionInput where
  collection : String
  deriving ToJson

structure SignedCreate where
  supported : Bool
  updateSupported : Bool
  deriving ToJson

@[conformance_family]
def signedCreateCollections : Family CollectionInput SignedCreate where
  inputs := (["Node", "Agent", "AgentContext", "CompactionConfig", "Skill", "DatastoreToolSurface",
    "Tools", "AgentTarget", "InferenceBackend", "InferenceBackendMetadata", "InferenceProfile",
    "InferenceSampling", "InferenceExecution", "InferenceRetryPolicy", "ToolServiceRegistry",
    "ProjectionAcpBinding", "Task", "Schedule", "EventSource", "Trigger", "Callback",
    "CallbackBinding", "CallbackModule", "RepositoryPlacement", "EvalDefinition", "Page",
    "PluginSource", "NotBundled"] : List String).map fun collection => { collection }
  model := fun i =>
    { supported := Configuration.clientSignedCreateCollection i.collection
      updateSupported := Configuration.clientSignedUpdateSupported i.collection }

@[conformance_family]
def signedDeleteCollections : Family CollectionInput Bool where
  inputs := (["Node", "Agent", "AgentContext", "Tools", "Schedule", "PackInstallation", "Goal",
    "GoalCreationClaim", "InferenceBackend", "InferenceBackendMetadata",
    "InferenceBackendObservation", "BackendCredential", "AgentSession", "NotBundled"] : List String).map
    fun collection => { collection }
  model := fun i => Configuration.clientSignedDeleteCollection i.collection

structure OrdinaryMutationInput where
  intent : Bool
  mutation : Bool
  deriving ToJson

@[conformance_family]
def ordinaryClientMutation : Family OrdinaryMutationInput Bool where
  inputs := [(false, false), (false, true), (true, false), (true, true)].map
    fun (intent, mutation) => { intent, mutation }
  model := fun i => Configuration.ordinaryMutationAllowed i.intent i.mutation

structure HeldSignerInput where
  actor : String
  bearer : String
  signer : String
  hasPrivate : Bool
  deriving ToJson

@[conformance_family]
def heldClientSigner : Family HeldSignerInput Bool where
  inputs := [("user-a", "user-a", "user-a", true), ("user-a", "user-a", "user-b", true),
    ("user-a", "user-a", "user-a", false), ("user-a", "user-b", "user-a", true)].map
    fun (actor, bearer, signer, hasPrivate) => { actor, bearer, signer, hasPrivate }
  model := fun i => Configuration.heldClientSignerAuthorizes i.actor i.bearer i.signer i.hasPrivate

structure ImmutableInput where
  observed : String
  proposed : String
  deriving ToJson

@[conformance_family]
def configurationImmutable : Family ImmutableInput Bool where
  inputs := [("before", "before"), ("before", "after")].map
    fun (observed, proposed) => { observed, proposed }
  model := fun i => Configuration.immutableReplacementAllowed i.observed i.proposed

structure WriteRetryInput where
  dispatched : Bool
  confirmedAbort : Bool
  replayAuthorized : Bool
  deriving ToJson

@[conformance_family]
def signedWriteRetry : Family WriteRetryInput Bool where
  inputs := Id.run do
    let mut cases := []
    for dispatched in [false, true] do
      for confirmedAbort in [false, true] do
        for replayAuthorized in [false, true] do
          cases := cases ++ [⟨dispatched, confirmedAbort, replayAuthorized⟩]
    return cases
  model := fun i =>
    Configuration.clientSignedWriteMayRetry i.dispatched i.confirmedAbort i.replayAuthorized

structure ObservationAuthorityInput where
  actor : String
  object : String
  permission : String
  deriving ToJson

@[conformance_family]
def backendObservationAuthority : Family ObservationAuthorityInput Bool where
  inputs := [("runtime", "backend-metadata", "update"), ("runtime", "backend-observation", "update"),
    ("backend-owner", "backend-metadata", "update"), ("agent", "backend-observation", "read"),
    ("sibling", "backend-observation", "read")].map fun (actor, object, permission) =>
      { actor, object, permission }
  model := fun i => Household.allowed 10 Configuration.backendObservationFixture i.actor
    "configuration" i.object i.permission

structure BackendMetadataInput where
  name : String
  actor : String
  revoked : Bool
  direct : Bool
  deriving ToJson

structure BackendMetadataAccess where
  read : Bool
  update : Bool
  deriving ToJson

@[conformance_family]
def backendMetadataSelection : Family BackendMetadataInput BackendMetadataAccess where
  inputs := [⟨"selected", "agent", false, false⟩, ⟨"sibling", "sibling", false, false⟩,
    ⟨"revoked", "agent", true, false⟩, ⟨"direct-survives", "agent", true, true⟩]
  model := fun i =>
    let withDirect := if i.direct then Household.apply Configuration.inferenceUsersetFixture
      (.grant ⟨"configuration", "backend-metadata", "reader", .actor "agent"⟩)
      else Configuration.inferenceUsersetFixture
    let state := if i.revoked then Household.apply withDirect (.revoke
      ⟨"configuration", "backend-metadata", "selected_by", .entitySet "configuration" "profile" "selected_by"⟩)
      else withDirect
    { read := Household.allowed 8 state i.actor "configuration" "backend-metadata" "read"
      update := Household.allowed 8 state i.actor "configuration" "backend-metadata" "update" }

structure UsersetInput where
  name : String
  actor : String
  revoked : String
  document : String
  direct : Bool
  deriving ToJson

def usersetInputs : List UsersetInput :=
  [⟨"selected", "agent", "", "sampling", false⟩, ⟨"sibling", "sibling", "", "sampling", false⟩,
   ⟨"parent-owner", "profile-owner", "", "sampling", false⟩,
   ⟨"profile-revoked", "agent", "profile", "sampling", false⟩,
   ⟨"sampling-revoked", "agent", "sampling", "sampling", false⟩,
   ⟨"executor-revoked", "agent", "executor", "sampling", false⟩,
   ⟨"direct-reader-survives", "agent", "sampling", "sampling", true⟩,
   ⟨"retry-selected", "agent", "", "retry", false⟩, ⟨"retry-sibling", "sibling", "", "retry", false⟩,
   ⟨"retry-revoked", "agent", "execution", "retry", false⟩]

def usersetVisible (i : UsersetInput) : Bool :=
  let withDirect := if i.direct then Household.apply Configuration.inferenceUsersetFixture
    (.grant ⟨"configuration", "sampling", "reader", .actor "agent"⟩)
    else Configuration.inferenceUsersetFixture
  let state := if i.revoked == "profile" then Household.apply withDirect (.revoke
      ⟨"configuration", "profile", "selected_by", .entitySet "agents" "agent-doc" "executor"⟩)
    else if i.revoked == "sampling" then Household.apply withDirect (.revoke
      ⟨"configuration", "sampling", "selected_by", .entitySet "configuration" "profile" "selected_by"⟩)
    else if i.revoked == "executor" then Household.apply withDirect (.revoke
      ⟨"agents", "agent-doc", "executor", .actor "agent"⟩)
    else if i.revoked == "execution" then Household.apply withDirect (.revoke
      ⟨"configuration", "execution", "selected_by", .entitySet "configuration" "profile" "selected_by"⟩)
    else withDirect
  Household.allowed 8 state i.actor "configuration" i.document "read"

@[conformance_family]
def usersetNativeVisibility : Family UsersetInput Bool where
  inputs := usersetInputs
  model := usersetVisible

structure SelectionManagerInput where
  name : String
  grant : Bool
  revoke : Bool
  deriving ToJson

structure SelectionManagerAccess where
  selection : Bool
  reader : Bool
  update : Bool
  deriving ToJson

@[conformance_family]
def selectionManager : Family SelectionManagerInput SelectionManagerAccess where
  inputs := [⟨"denied", false, false⟩, ⟨"granted", true, false⟩, ⟨"revoked", true, true⟩]
  model := fun i =>
    let relation : Relation := ⟨"configuration", "skill", "selection_manager", .actor "delegate"⟩
    let grantedState := if i.grant then Household.apply Configuration.skillSelectionFixture
      (.grantAs "user" relation) else Configuration.skillSelectionFixture
    let state := if i.revoke then Household.apply grantedState (.revokeAs "user" relation)
      else grantedState
    { selection := mayManage state "delegate"
        ⟨"configuration", "skill", "selected_by", .entitySet "configuration" "context" "selected_by"⟩
      reader := mayManage state "delegate" ⟨"configuration", "skill", "reader", .actor "delegate"⟩
      update := Household.allowed 8 state "delegate" "configuration" "skill" "update" }

structure NodeManagerInput where
  name : String
  actor : String
  resource : String
  id : String
  relation : String
  revokeAdmin : Bool
  deriving ToJson

@[conformance_family]
def nodeManager : Family NodeManagerInput Bool where
  inputs := [
    ("node-owner", "user", "configuration", "config", "selected_by", false),
    ("node-admin", "admin", "configuration", "config", "selected_by", false),
    ("creator", "creator-agent", "configuration", "config", "selected_by", false),
    ("other-admin", "other-admin", "configuration", "config", "selected_by", false),
    ("revoked-admin", "admin", "configuration", "config", "selected_by", true),
    ("admin-cannot-grant-writer", "admin", "configuration", "config", "writer", false),
    ("root-owner", "user", "workspace_roots", "root", "reader", false),
    ("root-admin", "admin", "workspace_roots", "root", "reader", false),
    ("root-other-admin", "other-admin", "workspace_roots", "root", "reader", false),
    ("root-revoked-admin", "admin", "workspace_roots", "root", "reader", true),
    ("agent-owner", "user", "agents", "agent", "session_writer", false),
    ("agent-admin", "admin", "agents", "agent", "session_writer", false),
    ("agent-creator", "creator-agent", "agents", "agent", "session_writer", false),
    ("agent-other-admin", "other-admin", "agents", "agent", "session_writer", false),
    ("agent-revoked-admin", "admin", "agents", "agent", "session_writer", true)].map
    fun (name, actor, resource, id, relation, revokeAdmin) =>
      { name, actor, resource, id, relation, revokeAdmin }
  model := fun i =>
    let state := if i.revokeAdmin then Household.apply nodeManagerFixture
      (.revoke ⟨"nodes", "node", "administrator", .actor "admin"⟩) else nodeManagerFixture
    mayManage state i.actor ⟨i.resource, i.id, i.relation, .actor "selected-agent"⟩

structure ManagerActorInput where
  actor : String
  deriving ToJson

@[conformance_family]
def nodeManagerLinkRevocation : Family ManagerActorInput Bool where
  inputs := [⟨"admin"⟩, ⟨"user"⟩]
  model := fun i =>
    let state := Household.apply nodeManagerFixture (.revoke
      ⟨"configuration", "config", "selection_manager", .entitySet "nodes" "node" "administrator"⟩)
    mayManage state i.actor
      ⟨"configuration", "config", "selected_by", .entitySet "agents" "agent" "executor"⟩

@[conformance_family]
def agentManagerLinkRevocation : Family ManagerActorInput Bool where
  inputs := [⟨"admin"⟩, ⟨"user"⟩]
  model := fun i =>
    let state := Household.apply nodeManagerFixture (.revoke
      ⟨"agents", "agent", "administrator", .entitySet "nodes" "node" "administrator"⟩)
    mayManage state i.actor ⟨"agents", "agent", "reader", .actor "reader"⟩

structure InvokePhaseInput where
  phase : String
  deriving ToJson

def agentInvokeAllowed (i : InvokePhaseInput) : Bool :=
  let grant : Relation := ⟨"agents", "agent", "session_writer", .actor "reader"⟩
  let granted := Household.apply nodeManagerFixture (.grantAs "user" grant)
  let state := if i.phase == "after" then Household.apply granted (.revokeAs "user" grant) else granted
  Household.allowed 8 state "reader" "agents" "agent" "invoke"

@[conformance_family]
def agentManagerInvokeNative : Family InvokePhaseInput Bool where
  inputs := [⟨"before"⟩, ⟨"after"⟩]
  model := agentInvokeAllowed

def roleOf (actor : String) : String :=
  if actor == userDid then "user"
  else if actor == runtimeDid then "runtime"
  else if actor == agentDid then "agent"
  else if actor == siblingDid then "sibling"
  else "other"

structure EventConfigurationInput where
  actor : String
  role : String
  id : String
  permission : String
  revoked : Bool
  rebound : Bool
  postRevoke : Bool
  deriving ToJson

def eventConfigurationInputs : List EventConfigurationInput :=
  let open_ := fun (actor id permission : String) (revoked : Bool) =>
    ({ actor, role := roleOf actor, id, permission, revoked, rebound := false, postRevoke := false } :
      EventConfigurationInput)
  let rebound := fun (actor id : String) =>
    ({ actor, role := roleOf actor, id, permission := "read", revoked := true, rebound := true,
       postRevoke := false } : EventConfigurationInput)
  [open_ userDid "trigger" "update" false, open_ runtimeDid "trigger" "update" false,
   open_ runtimeDid "source" "read" false, open_ agentDid "trigger" "read" false,
   open_ agentDid "source" "read" false, open_ agentDid "trigger" "update" false,
   open_ siblingDid "source" "read" false, open_ agentDid "source" "read" true,
   rebound siblingDid "trigger", rebound siblingDid "source", rebound agentDid "source",
   rebound agentDid "trigger",
   { actor := siblingDid, role := roleOf siblingDid, id := "source", permission := "read",
     revoked := true, rebound := true, postRevoke := true }]

def eventConfigurationAfter (i : EventConfigurationInput) : Household.State :=
  let base := ConfigurationFixture.eventConfigurationState
  let granted := Automation.publishEventConfiguration base userDid "trigger" "source" runtimeDid agentDid true
  if i.postRevoke then
    let rebound := Automation.rebindEventConfiguration granted userDid "trigger" "source" agentDid siblingDid
    let revoked := Household.apply rebound (.revoke ⟨"configuration", "source", "reader", .actor siblingDid⟩)
    Automation.publishEventConfiguration revoked userDid "trigger" "source" runtimeDid siblingDid false
  else
    let revoked := if i.revoked then Household.apply granted
      (.revoke ⟨"configuration", "source", "reader", .actor agentDid⟩) else granted
    if i.rebound then
      Automation.rebindEventConfiguration revoked userDid "trigger" "source" agentDid siblingDid
    else revoked

@[conformance_family]
def eventConfigurationAccess : Family EventConfigurationInput Bool where
  inputs := eventConfigurationInputs
  model := fun i => Household.allowed 5 (eventConfigurationAfter i) i.actor "configuration" i.id i.permission

structure EventCursorInput where
  actor : String
  role : String
  permission : String
  revoked : Bool
  deriving ToJson

@[conformance_family]
def eventCursorAccess : Family EventCursorInput Bool where
  inputs := [(userDid, "update", false), (runtimeDid, "read", false), (runtimeDid, "update", false),
    (agentDid, "update", false), (siblingDid, "read", false), (agentDid, "update", true)].map
    fun (actor, permission, revoked) => { actor, role := roleOf actor, permission, revoked }
  model := fun i =>
    let initial : Household.State :=
      { objects := [⟨"nodes", "home", userDid⟩],
        relations := [⟨"nodes", "home", "runtime_reader", .actor runtimeDid⟩], enrolled := [] }
    let created := Automation.publishEventCursor initial userDid "cursor" "home" agentDid true true
    let state := if i.revoked then Household.apply created
      (.revoke ⟨"configuration", "cursor", "writer", .actor agentDid⟩) else created
    Household.allowed 5 state i.actor "configuration" "cursor" i.permission

structure FreshAdmissionInput where
  origin : Bool
  binding : Bool
  intact : Bool
  deriving ToJson

def freshAdmissionInputs : List FreshAdmissionInput :=
  [⟨true, true, true⟩, ⟨false, true, true⟩, ⟨true, false, true⟩, ⟨true, true, false⟩]

/-- A fresh store is driven through `FreshBackendStore::observe` and
`initialize`: the origin knob is whether the data directory is empty when it
is observed. Initialization installs the private legacy schema itself, so a
fresh admission always starts bound and intact. -/
@[conformance_family]
def freshBackendAdmission : Family FreshAdmissionInput Bool where
  inputs := freshAdmissionInputs.filter fun i => i.binding && i.intact
  model := fun i => Configuration.freshBackendAdmissionAllowed i.origin i.binding i.intact

/-- model-production-gap: a fresh-store admission cannot observe a missing or
broken private binding; `FreshBackendStore::initialize` installs the private
legacy schema itself, and a store whose binding is absent or altered is refused
only on reopen (`household_fresh_backend_reopen`). -/
@[conformance_family]
def freshBackendAdmissionUnreached : Family FreshAdmissionInput Bool where
  inputs := freshAdmissionInputs.filter fun i => !(i.binding && i.intact)
  model := fun i => Configuration.freshBackendAdmissionAllowed i.origin i.binding i.intact

end Conformance.Household
