import Proofs.Conformance.Identity.Conformance
import Proofs.Conformance.Identity.HouseholdStore
import Proofs.Conformance.Identity.HouseholdBInference
import Proofs.Conformance.Family

namespace Conformance.Household

open Lean Identity Identity.Household Identity.Conformance

structure ProviderAssemblyInput where
  name : String
  actor : String
  role : String
  purpose : String
  revokeTools : Bool
  missingContext : Bool
  deriving ToJson

@[conformance_family]
def providerAssembly : Family ProviderAssemblyInput Bool where
  inputs := [("normal", agentDid, "normal", false, false), ("auxiliary", agentDid, "auxiliary", false, false),
    ("sibling", siblingDid, "normal", false, false), ("revoked-tools", agentDid, "normal", true, false),
    ("replaced-context", agentDid, "auxiliary", false, true)].map
    fun (name, actor, purpose, revokeTools, missingContext) =>
      { name, actor, role := roleOf actor, purpose, revokeTools, missingContext }
  model := fun i =>
    let state := if i.revokeTools then apply ConfigurationFixture.providerInitial
      (.revoke ⟨"configuration", "tools-doc", "reader", .actor agentDid⟩) else ConfigurationFixture.providerInitial
    let documents := [("agents", "agent-doc"),
      ("configuration", if i.missingContext then "replaced-context" else "context-doc"),
      ("configuration", "tools-doc")]
    Configuration.providerAssemblyAllowed state i.actor userDid documents

structure InferenceProviderInput where
  name : String
  actor : String
  revoked : String
  documents : List String
  deriving ToJson

@[conformance_family]
def inferenceProviderSelection : Family InferenceProviderInput Bool where
  inputs := ([("selected", "agent", ""), ("sibling", "sibling", "")] ++
    (["profile", "sampling", "execution", "retry", "compaction", "summary-profile"] : List String).map
      fun id => ("revoked-" ++ id, "agent", id)).map fun (name, actor, revoked) =>
        { name, actor, revoked, documents := Configuration.inferenceProviderDocuments.map Prod.snd }
  model := fun i =>
    let state := if i.revoked.isEmpty then Configuration.inferenceProviderFixture else
      apply Configuration.inferenceProviderFixture
        (.revoke ⟨"configuration", i.revoked, "selected_by", .entitySet "agents" "agent-doc" "executor"⟩)
    Configuration.providerConfigurationVisible state i.actor Configuration.inferenceProviderDocuments

/-- Provider input selection always runs as the turn's present Agent; the
sibling and absent callers are observed by `pluginBackendUse`. -/
@[conformance_family]
def pluginBackendSelection : Family PluginBackendInput Bool where
  inputs := pluginBackendCases.filter fun i => i.actor == some "agent"
  model := pluginBackendModel

structure BackendChoiceInput where
  name : String
  currentKind : String
  currentRef : Option String
  nextKind : String
  nextRef : Option String
  visibleDefault : Option String
  deriving ToJson

@[conformance_family]
def runtimeDefaultChoice : Family BackendChoiceInput Bool where
  inputs := [("cross-named-default", "ChatGptCodex", some "a1", "XaiGrokOAuth", some "g2", some "g2"),
    ("cross-wrong-default", "ChatGptCodex", some "a1", "XaiGrokOAuth", none, some "g2"),
    ("cross-empty-default", "ChatGptCodex", some "a1", "XaiGrokOAuth", none, none),
    ("same-provider-cannot-switch", "XaiGrokOAuth", some "g2", "XaiGrokOAuth", none, none)].map
    fun (name, currentKind, currentRef, nextKind, nextRef, visibleDefault) =>
      { name, currentKind, currentRef, nextKind, nextRef, visibleDefault }
  model := fun i =>
    InferenceOperations.backendChoiceAllowed (fun _ => i.visibleDefault)
      (some (i.currentKind, _root_.Configuration.BackendAuth.nodeOAuth i.currentRef))
      (i.nextKind, _root_.Configuration.BackendAuth.nodeOAuth i.nextRef)

structure OwnToolEditInput where
  name : String
  sets : List String
  deriving ToJson

def ownToolEditSets (names : List String) : List GrantSets.GrantSet :=
  [GrantSets.GrantSet.agentBaseline, .policyDelegate].filter fun set => names.contains set.name

def ownToolEditCases : List OwnToolEditInput :=
  [("own-only-goal", [GrantSets.GrantSet.agentBaseline.name]),
    ("engineer-goal", [GrantSets.GrantSet.agentBaseline.name, GrantSets.GrantSet.policyDelegate.name])].map
    fun (name, sets) => { name, sets }

/-- Whether an Agent configuring its own Tools may enable the goal tools while
holding `sets` directly. An Agent with only the User's own-only self-config
grant holds the Agent baseline: no NAC relation operation, so the held-bounded
rule refuses every source it would project, even one for a held baseline
permission. The Engineer's policy delegate set admits a source it holds. -/
def ownToolEditAccepted (i : OwnToolEditInput) : Bool :=
  let sources := ConfigurationFixture.editToolSources ToolPolicy.ContractCases.agentGoalOnly false
  let initial : Configuration.ToolGrantProjection :=
    { admins := [userDid],
      direct := ((ownToolEditSets i.sets).flatMap GrantSets.GrantSet.permissions).map (agentDid, ·) }
  let result := Configuration.configureToolGrantGroup initial ConfigurationFixture.toolGrantorHousehold
    sources agentDid true true
  !sources.isEmpty && result != initial

@[conformance_family]
def ownToolEdit : Family OwnToolEditInput Bool where
  inputs := ownToolEditCases.filter fun i => i.name == "own-only-goal"
  model := ownToolEditAccepted

/-- model-production-gap: the self-configuration tool runs only for an Agent
that also holds its own-only self-configuration grant, so an Agent holding
exactly the Agent baseline and policy delegate sets cannot be built through it.
With that grant added, production accepts the goal-tools edit the model refuses
for the two sets alone. -/
@[conformance_family]
def ownToolEditUnreached : Family OwnToolEditInput Bool where
  inputs := ownToolEditCases.filter fun i => i.name != "own-only-goal"
  model := ownToolEditAccepted

structure DanglingRepairInput where
  actor : String
  nacEnabled : Bool
  admin : Bool
  completeInventory : Bool
  repair : Bool
  deriving ToJson

structure DanglingRepairOutcome where
  refusal : Option String
  applied : Bool
  deriving ToJson

/-- The NAC-enabled cases come first: disabling native NAC is the fixture's
last change. -/
@[conformance_family]
def danglingSelectionRepair : Family DanglingRepairInput DanglingRepairOutcome where
  inputs := [("user", true, true, true), ("user", true, true, false), ("agent", true, false, true),
    ("agent", true, false, false), ("user", false, true, true)].map
    fun (actor, nacEnabled, admin, repair) =>
      { actor, nacEnabled, admin, completeInventory := false, repair }
  model := fun i =>
    let authority : Configuration.DestructiveAuthority :=
      ⟨i.nacEnabled, i.admin, if i.completeInventory then .complete else .acpFiltered⟩
    { refusal := if i.repair then
        (Configuration.destructiveAdminDecision authority).map Configuration.DestructiveRefusal.name
        else none
      applied := Configuration.danglingRepairApplied i.actor authority i.repair }

structure OperationsCheck where
  phase : String
  actor : String
  role : String
  permission : String
  deriving ToJson

def operationsRole (actor : String) : String :=
  if actor == otherAgentDid then "outsider" else roleOf actor

def operationsParent : Relation := ⟨"operations", "readiness", "parent", .object "nodes" "home"⟩

def operationsState (phase : String) : State :=
  let attached := Household.apply OperationsFixture.initial (.grantAs runtimeDid operationsParent)
  if phase == "attached" then attached
  else if phase == "detached" then Household.apply attached (.revokeAs runtimeDid operationsParent)
  else OperationsFixture.initial

def operationsCheck (phase actor permission : String) : OperationsCheck :=
  { phase, actor, role := operationsRole actor, permission }

def operationsAllowed (i : OperationsCheck) : Bool :=
  Household.allowed 5 (operationsState i.phase) i.actor "operations" "readiness" i.permission

/-- A deferred enrollment document is observed while its parent edge is
attached: the User's update, and the reads of the Agent, its sibling and an
outsider. -/
@[conformance_family]
def operationsDeferredEnrollment : Family OperationsCheck Bool where
  inputs := [operationsCheck "attached" userDid "update", operationsCheck "attached" agentDid "read",
    operationsCheck "attached" siblingDid "read", operationsCheck "attached" otherAgentDid "read"]
  model := operationsAllowed

/-- Runtime status is observed with its parent attached, then republished
and read again after the runtime detaches it. -/
@[conformance_family]
def operationsRuntimeStatus : Family OperationsCheck Bool where
  inputs := [operationsCheck "attached" userDid "read", operationsCheck "attached" userDid "update",
    operationsCheck "attached" runtimeDid "update", operationsCheck "attached" agentDid "read",
    operationsCheck "attached" siblingDid "read", operationsCheck "attached" otherAgentDid "update",
    operationsCheck "detached" runtimeDid "update", operationsCheck "detached" userDid "read"]
  model := operationsAllowed

example : (operationsRuntimeStatus.inputs.map operationsAllowed) =
    [true, false, true, false, false, false, true, false] := by native_decide

structure FreshOriginInput where
  locked : Bool
  empty : Bool
  unexposed : Bool
  committed : Bool
  deriving ToJson

def freshOriginCases : List FreshOriginInput :=
  [(true, true, true, true), (false, true, true, true), (true, false, true, true),
    (true, true, false, true), (true, true, true, false)].map
    fun (locked, empty, unexposed, committed) => { locked, empty, unexposed, committed }

@[conformance_family]
def freshBackendOrigin : Family FreshOriginInput Bool where
  inputs := freshOriginCases.filter fun i => i.unexposed && i.committed
  model := fun i => Configuration.freshBackendOrigin i.locked i.empty i.unexposed i.committed

/-- model-production-gap: `FreshBackendStore::initialize` builds its temporary
native Node without HTTP or P2P transport and offers no seam that fails its
schema commit, so an exposed or uncommitted fresh-store baseline cannot be
driven. -/
@[conformance_family]
def freshBackendOriginUnreached : Family FreshOriginInput Bool where
  inputs := freshOriginCases.filter fun i => !(i.unexposed && i.committed)
  model := fun i => Configuration.freshBackendOrigin i.locked i.empty i.unexposed i.committed

end Conformance.Household
