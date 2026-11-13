import Proofs.Documents.Configuration
import Proofs.Store.Identity.State
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Family

namespace Conformance.Contracts.PackInstallation

structure OwnershipInput where
  coordinate : String
  origin : Option String
  deriving Lean.ToJson

structure OwnershipObservation where
  allowed : Bool
  removalAllowed : Bool
  deriving Lean.ToJson

@[conformance_family]
def ownership : Conformance.Family OwnershipInput OwnershipObservation where
  inputs := ["acme/demo", "other/demo", "acme/other"].flatMap fun coordinate =>
    [none, some "acme/demo", some "other/demo", some "demo"].map fun origin =>
      OwnershipInput.mk coordinate origin
  model := fun i =>
    OwnershipObservation.mk (Configuration.packDocumentWriteAllowed i.coordinate i.origin)
      (Configuration.packDocumentRemovalAllowed i.coordinate i.origin)

structure HostedSchemaInput where
  scopeMatches : Bool
  signed : Bool
  nativeUpdate : Bool
  deriving Lean.ToJson

@[conformance_family]
def hostedSchema : Conformance.Family HostedSchemaInput Bool where
  inputs := [false, true].flatMap fun scope => [false, true].flatMap fun signed =>
    [false, true].map fun nativeUpdate => HostedSchemaInput.mk scope signed nativeUpdate
  model := fun i => Identity.hostedSchemaAllowed i.scopeMatches i.signed i.nativeUpdate

structure HostedHomeInput where
  homeWritten : Bool
  deriving Lean.ToJson

@[conformance_family]
def hostedHome : Conformance.Family HostedHomeInput Bool where
  inputs := Configuration.hostedPackHomeCases.map HostedHomeInput.mk
  model := fun i => Configuration.hostedPackReportedSuccess true true i.homeWritten

structure HostedEvidenceInput where
  actorMatches : Bool
  graphVerified : Bool
  planApproved : Bool
  deriving Lean.ToJson

@[conformance_family]
def hostedEvidence : Conformance.Family HostedEvidenceInput Bool where
  inputs := Configuration.hostedPackEffectCases.map fun (actor, graph, plan) =>
    HostedEvidenceInput.mk actor graph plan
  model := fun i =>
    Configuration.hostedPackEffectAllowed i.actorMatches i.graphVerified i.planApproved true

deriving instance Lean.ToJson for Configuration.PackIdentity.Binding,
  Configuration.PackIdentity.DependencyBindings

structure IdentityPlanInput where
  name : String
  recorded : List Configuration.PackIdentity.Binding
  requested : List String
  provisioned : List Configuration.PackIdentity.Binding
  deriving Lean.ToJson

structure IdentityPlanResult where
  accepted : Bool
  selected : List Configuration.PackIdentity.Binding
  nextRecorded : List Configuration.PackIdentity.Binding
  deriving Lean.ToJson

/-- Production holds bindings in a map keyed by slot, so its observation lists
them in slot order. -/
def bySlot (bindings : List Configuration.PackIdentity.Binding) :
    List Configuration.PackIdentity.Binding :=
  (bindings.toArray.qsort fun a b => decide (a.slot < b.slot)).toList

@[conformance_family]
def identityPlan : Conformance.Family IdentityPlanInput IdentityPlanResult where
  inputs := Configuration.PackIdentity.cases.map fun c =>
    IdentityPlanInput.mk c.name c.recorded c.requested c.provisioned
  model := fun i =>
    let result := Configuration.PackIdentity.plan i.recorded i.requested i.provisioned
    IdentityPlanResult.mk result.isSome (bySlot ((result.map (·.selected)).getD []))
      (bySlot ((result.map (·.recorded)).getD []))

structure IdentityReferenceInput where
  name : String
  declared : List String
  available : List Configuration.PackIdentity.DependencyBindings
  dependency : String
  slot : String
  deriving Lean.ToJson

structure ResolvedBinding where
  creationNonce : String
  agentDid : String
  deriving Lean.ToJson

@[conformance_family]
def identityReference :
    Conformance.Family IdentityReferenceInput (Option ResolvedBinding) where
  inputs := Configuration.PackIdentity.referenceCases.map fun c =>
    IdentityReferenceInput.mk c.name c.declared c.available c.dependency c.slot
  model := fun i =>
    (Configuration.PackIdentity.resolveDependency i.declared i.available i.dependency i.slot).map
      fun binding => ResolvedBinding.mk binding.creationNonce binding.agentDid


structure HostedPluginInput where
  reviewed : Bool
  plan : Bool
  native : Bool
  deriving Lean.ToJson

def hostedPluginInputs : List HostedPluginInput :=
  [false, true].flatMap fun reviewed => [false, true].flatMap fun plan =>
    [false, true].map fun native => { reviewed, plan, native }

/-- The hosted plugin-root scenario applies a reviewed archive's graph with
the client's plan in a native-authorized transaction: it builds only the case
where all three hold. -/
@[conformance_family]
def hostedPlugin : Conformance.Family HostedPluginInput Bool where
  inputs := hostedPluginInputs.filter fun i => i.reviewed && i.plan && i.native
  model := fun i => Configuration.hostedPluginInstallAllowed i.reviewed i.plan i.native

/-- model-production-gap: production has no single decision over the reviewed
digest, the client plan and native authorization; each denial is distributed
across the hosted install route, and no production scenario yet installs a
plugin root with one of them missing. -/
@[conformance_family]
def hostedPluginUnreached : Conformance.Family HostedPluginInput Bool where
  inputs := hostedPluginInputs.filter fun i => !(i.reviewed && i.plan && i.native)
  model := fun i => Configuration.hostedPluginInstallAllowed i.reviewed i.plan i.native

deriving instance Lean.ToJson for Configuration.PackAttemptEvidence

structure FinalityInput where
  attempts : List Configuration.PackAttemptEvidence
  deriving Lean.ToJson

structure FinalityOutput where
  unconfirmed : Bool
  deriving Lean.ToJson

def finalityInputs (histories : List (List Configuration.PackAttemptEvidence)) :
    List FinalityInput :=
  histories.map fun attempts => { attempts }

/-- A store transaction scenario builds one attempt: a rejected callback (no
commit) or a commit whose receipt is lost (unknown commit). -/
@[conformance_family]
def finalityTransaction : Conformance.Family FinalityInput FinalityOutput where
  inputs := finalityInputs (Configuration.packFinalityCases.filter
    (· ∈ [[.noCommit], [.unknownCommit]]))
  model := fun i => { unconfirmed := Configuration.packCommitUnconfirmed i.attempts }

/-- The staged plugin scenario times out one commit acknowledgement. -/
@[conformance_family]
def finalityStagedPlugin : Conformance.Family FinalityInput FinalityOutput where
  inputs := finalityInputs (Configuration.packFinalityCases.filter (· == [.unknownCommit]))
  model := fun i => { unconfirmed := Configuration.packCommitUnconfirmed i.attempts }

/-- model-production-gap: the shared transaction owner folds the attempts of
one write into a sticky unconfirmed flag inside its retry loop, and no
scenario drives several commit attempts of one transaction or a write with
no failed attempt to an error, so these histories have no production run. -/
@[conformance_family]
def finalityUnreached : Conformance.Family FinalityInput FinalityOutput where
  inputs := finalityInputs (Configuration.packFinalityCases.filter
    (· ∉ [[.noCommit], [.unknownCommit]]))
  model := fun i => { unconfirmed := Configuration.packCommitUnconfirmed i.attempts }

example : Configuration.packCommitUnconfirmed [.noCommit] = false := by decide

example : Configuration.packCommitUnconfirmed [.unknownCommit, .noCommit] = true := by decide

inductive PinSource where
  | installed | shipped
  deriving Lean.ToJson, BEq

structure PinInput where
  name : String
  source : PinSource
  pin : Option String
  actual : String
  deriving Lean.ToJson

def pinCases : List PinInput := [
  ⟨"installed_unpinned", .installed, none, "a"⟩,
  ⟨"installed_match", .installed, some "a", "a"⟩,
  ⟨"installed_mismatch", .installed, some "b", "a"⟩,
  ⟨"shipped_match", .shipped, some "a", "a"⟩,
  ⟨"shipped_mismatch", .shipped, some "b", "a"⟩]

@[conformance_family]
def pinPrepare : Conformance.Family PinInput Bool where
  inputs := pinCases
  model := fun i => Configuration.pluginPinMatches i.pin i.actual

/-- The graph-commit scenario pins a plugin that is already installed in the
home and ships none, so only installed sources can be built there. -/
@[conformance_family]
def pinInstalledCommit : Conformance.Family PinInput Bool where
  inputs := pinCases.filter (·.source == .installed)
  model := fun i => Configuration.pluginPinMatches i.pin i.actual

structure WireLockInput where
  held : Bool
  deriving Lean.ToJson

structure WireLockResolution where
  resolveAllowed : Bool
  deriving Lean.ToJson

structure WireLockPublish where
  publishAllowed : Bool
  deriving Lean.ToJson

@[conformance_family]
def wireLockResolution : Conformance.Family WireLockInput WireLockResolution where
  inputs := [false, true].map WireLockInput.mk
  model := fun i => WireLockResolution.mk (Configuration.stagedPluginResolutionAllowed i.held)

@[conformance_family]
def wireLockInstallDuringWire : Conformance.Family WireLockInput WireLockPublish where
  inputs := [false, true].map WireLockInput.mk
  model := fun i => WireLockPublish.mk (Configuration.wireAndPluginPublishAllowed i.held)

@[conformance_family]
def wireLockWireDuringRemoval : Conformance.Family WireLockInput WireLockPublish where
  inputs := [false, true].map WireLockInput.mk
  model := fun i => WireLockPublish.mk (Configuration.wireAndPluginPublishAllowed i.held)

structure WireTargetInput where
  «local» : Bool
  preview : Bool
  remove : Bool
  deriving Lean.ToJson

@[conformance_family]
def wireTarget : Conformance.Family WireTargetInput Bool where
  inputs := [false, true].flatMap fun localTarget => [false, true].flatMap fun preview =>
    [false, true].map fun remove => { «local» := localTarget, preview, remove }
  model := fun i => Configuration.wireTargetAllowed i.«local» i.preview i.remove

structure OperatorWireInput where
  operator : Bool
  ready : Bool
  home : Bool
  deriving Lean.ToJson

structure OperatorWireOutput where
  allowed : Bool
  deriving Lean.ToJson

@[conformance_family]
def operatorWire : Conformance.Family OperatorWireInput OperatorWireOutput where
  inputs := [false, true].flatMap fun operator => [false, true].flatMap fun ready =>
    [false, true].map fun home => { operator, ready, home }
  model := fun i => { allowed := Configuration.operatorWireAvailable i.operator i.ready i.home }

structure ReferenceInput where
  witness : String
  references : List (List String)
  rootShips : List String
  dependencyShips : List String
  installed : List String
  «local» : Bool
  deriving Lean.ToJson

def referenceCases : List ReferenceInput := [
  ⟨"plugin_references.empty", [[]], [], [], [], true⟩,
  ⟨"plugin_references.missing", [["acme/search"]], [], [], [], true⟩,
  ⟨"plugin_references.shipped", [["acme/search"]], ["acme/search"], [], [], true⟩,
  ⟨"plugin_references.dependency", [["acme/search"]], [], ["acme/search"], [], true⟩,
  ⟨"plugin_references.installed", [["acme/search"]], [], [], ["acme/search"], true⟩,
  ⟨"plugin_references.wrong_namespace", [["other/search"]], ["acme/search"], [], [], true⟩,
  ⟨"plugin_references.mixed", [["acme/search", "acme/parse"]], ["acme/search"], [],
    ["acme/parse"], true⟩,
  ⟨"plugin_references.missing_in_dependency", [["acme/search"], ["acme/missing"]],
    ["acme/search"], [], [], true⟩,
  ⟨"plugin_references.repeated", [["acme/search", "acme/search"]], [], ["acme/search"], [], true⟩,
  ⟨"plugin_references.remote_empty", [[]], [], [], [], false⟩,
  ⟨"plugin_references.remote_missing", [["acme/search"]], [], [], [], false⟩,
  ⟨"plugin_references.remote_recorded", [["acme/search"]], [], [], ["acme/search"], false⟩,
  ⟨"plugin_references.remote_shipped", [["acme/search"]], ["acme/search"], [], [], false⟩,
  ⟨"plugin_references.remote_dependency", [[]], [], ["acme/search"], [], false⟩]

@[conformance_family]
def referenceAvailability : Conformance.Family ReferenceInput Bool where
  inputs := referenceCases
  model := fun i => Configuration.pluginInstallAvailable i.references.flatten
    (i.rootShips ++ i.dependencyShips) i.installed i.«local»

/-- The removal check reads the node's configuration, so its observation is
the observed one. -/
@[conformance_family]
def referenceRemoval : Conformance.Family ReferenceInput Bool where
  inputs := referenceCases
  model := fun i => Configuration.pluginRetirementAllowed i.references.flatten
    (i.rootShips ++ i.dependencyShips) true

structure RetentionInput where
  rootShips : List String
  deriving Lean.ToJson

structure RetentionObservation where
  retainNotificationRecord : Bool
  retainRetiredRecord : Bool
  deriving Lean.ToJson

@[conformance_family]
def storeRecordRetention : Conformance.Family RetentionInput RetentionObservation where
  inputs := [[], ["acme/search"]].map fun rootShips => { rootShips }
  model := fun i =>
    { retainNotificationRecord := Configuration.retainPluginStoreRecord i.rootShips false
      retainRetiredRecord := Configuration.retainPluginStoreRecord i.rootShips true }

structure EvalSubjectInput where
  name : String
  selected : Option Configuration.PackIdentity.Binding
  slot : String
  deriving Lean.ToJson

@[conformance_family]
def evalSubject : Conformance.Family EvalSubjectInput (Option String) where
  inputs := Configuration.PackIdentity.evalSubjectCases.map fun c =>
    EvalSubjectInput.mk c.name c.selected c.slot
  model := fun i => Configuration.PackIdentity.resolveEvalSubject i.selected i.slot

end Conformance.Contracts.PackInstallation
