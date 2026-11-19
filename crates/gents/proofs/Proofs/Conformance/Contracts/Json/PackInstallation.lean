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

deriving instance Lean.ToJson for Configuration.WireSubject

inductive WireSource where
  | sentPack
  | ownHome
  | served
  deriving Lean.ToJson

structure WirePlanInput where
  requested : String
  approved : List Configuration.WireSubject
  source : WireSource
  subject : Configuration.WireSubject
  deriving Lean.ToJson

/-- A record from the pack the client sent or from its own home; a served
record it approved; one of a pack it never approved; one of an approved pack
whose record or coordinate differs; one with no approvals recorded; and, from
every source, a record of another plugin than the one asked for, approved or
not. -/
@[conformance_family]
def wirePlan : Conformance.Family WirePlanInput Bool where
  inputs :=
    let approved : Configuration.WireSubject := ⟨"p1", "acme/demo", "acme/tool", "r1"⟩
    let other : Configuration.WireSubject := ⟨"p1", "acme/demo", "acme/other", "r1"⟩
    [ ⟨"acme/tool", [], .sentPack, approved⟩,
      ⟨"acme/tool", [], .ownHome, approved⟩,
      ⟨"acme/tool", [approved], .served, approved⟩,
      ⟨"acme/tool", [approved], .served, ⟨"p2", "acme/demo", "acme/tool", "r1"⟩⟩,
      ⟨"acme/tool", [approved], .served, ⟨"p1", "acme/demo", "acme/tool", "r2"⟩⟩,
      ⟨"acme/tool", [approved], .served, ⟨"p1", "acme/other", "acme/tool", "r1"⟩⟩,
      ⟨"acme/tool", [], .served, approved⟩,
      ⟨"acme/tool", [approved, other], .served, other⟩,
      ⟨"acme/tool", [], .sentPack, other⟩,
      ⟨"acme/tool", [], .ownHome, other⟩ ]
  model := fun i =>
    (Configuration.wirePlanSubject i.requested i.approved
      (match i.source with
        | .sentPack => .sentPack i.subject
        | .ownHome => .ownHome i.subject
        | .served => .served i.subject)).isSome

structure WireApprovedIdsInput where
  approved : Option (List String)
  served : List String
  deriving Lean.ToJson

/-- Ids the approval yields, a subset of them, an extra one, a different one,
and an approval that could not derive its ids. -/
@[conformance_family]
def wireApprovedIdsCases : Conformance.Family WireApprovedIdsInput (Option (List String)) where
  inputs :=
    [ ⟨some ["a", "b"], ["a", "b"]⟩, ⟨some ["a", "b"], ["a"]⟩, ⟨some ["a"], ["a", "rogue"]⟩,
      ⟨some ["a"], ["rogue"]⟩, ⟨none, ["a"]⟩ ]
  model := fun i => Configuration.wireApprovedIds i.approved i.served

structure WireReviewedJobInput where
  approved : Option (Option String)
  served : Option String
  deriving Lean.ToJson

/-- The approved job, another one, a job where the pack has none, none where
it has one, no job either side, and an approval that could not derive it. -/
@[conformance_family]
def wireReviewedJobCases : Conformance.Family WireReviewedJobInput (Option (Option String)) where
  inputs :=
    [ ⟨some (some "j1"), some "j1"⟩, ⟨some (some "j1"), some "j2"⟩, ⟨some none, some "j1"⟩,
      ⟨some (some "j1"), none⟩, ⟨some none, none⟩, ⟨none, some "j1"⟩ ]
  model := fun i => Configuration.wireReviewedJob i.approved i.served

/-- An id part as a case writes it: a slot's name, or text. -/
structure IdPartInput where
  slot : Bool
  value : String
  deriving Lean.ToJson

def IdPartInput.part (input : IdPartInput) : Configuration.IdPart :=
  if input.slot then .slot input.value else .text input.value

def text (value : String) : IdPartInput := ⟨false, value⟩
def slot (name : String) : IdPartInput := ⟨true, name⟩

def templates (ids : Option (List (List IdPartInput))) : Option (List (List Configuration.IdPart)) :=
  ids.map (·.map (·.map IdPartInput.part))

structure ApprovalInput where
  digest : String
  coordinate : String
  yields : Option (List (List IdPartInput))
  deriving Lean.ToJson

def ApprovalInput.approval (input : ApprovalInput) : Configuration.PackApproval :=
  ⟨input.digest, input.coordinate, [], templates input.yields⟩

structure WireFilledIdsInput where
  approved : Option (List (List IdPartInput))
  agent : Option String
  served : List String
  deriving Lean.ToJson

/-- A literal id; a slot id filled with the wired agent; a slot id the runtime
reports for another agent; a slot id with no agent to fill it, as for a pack
wired for targets; a literal id beside an unfillable one; and an approval
that could not derive its ids. -/
@[conformance_family]
def wireFilledIdsCases : Conformance.Family WireFilledIdsInput (Option (List String)) where
  inputs :=
    let slotted := [slot "worker", text ":tools"]
    [ ⟨some [[text "a"]], some "did:key:agent", ["a"]⟩,
      ⟨some [slotted], some "did:key:agent", ["did:key:agent:tools"]⟩,
      ⟨some [slotted], some "did:key:agent", ["did:key:rogue:tools"]⟩,
      ⟨some [slotted], none, ["did:key:agent:tools"]⟩,
      ⟨some [[text "a"], slotted], none, ["a"]⟩,
      ⟨none, some "did:key:agent", ["a"]⟩ ]
  model := fun i =>
    Configuration.wireApprovedIds (Configuration.wireFilledIds (templates i.approved) i.agent)
      i.served

/-- Two approved versions of `acme/demo`, `p1` yielding `old` and `p2`
yielding `new`, both yielding `a`, and another pack yielding `other`. -/
def twoVersions : List ApprovalInput :=
  [ ⟨"p1", "acme/demo", some [[text "a"], [text "old"]]⟩,
    ⟨"p2", "acme/demo", some [[text "a"], [text "new"]]⟩,
    ⟨"q1", "acme/other", some [[text "other"]]⟩ ]

structure WireOwnPackIdsInput where
  approvals : List ApprovalInput
  recordDigest : String
  served : List String
  deriving Lean.ToJson

/-- The newer version's record with its own ids, with an id only the older
version yields, and with an id only another pack yields; the older version's
record with its ids; a record of no approval; and an approval without ids. -/
@[conformance_family]
def wireOwnPackIdsCases : Conformance.Family WireOwnPackIdsInput (Option (List String)) where
  inputs :=
    [ ⟨twoVersions, "p2", ["a", "new"]⟩, ⟨twoVersions, "p2", ["a", "old"]⟩,
      ⟨twoVersions, "p2", ["other"]⟩, ⟨twoVersions, "p1", ["old"]⟩, ⟨twoVersions, "p3", ["a"]⟩,
      ⟨[⟨"p1", "acme/demo", none⟩], "p1", ["a"]⟩ ]
  model := fun i =>
    Configuration.wireOwnPackIds (i.approvals.map ApprovalInput.approval)
      ⟨i.recordDigest, "acme/demo", "acme/tool", "r1"⟩
      "did:key:agent" i.served

structure WireTargetPackIdsInput where
  approvals : List ApprovalInput
  coordinate : String
  installed : Option String
  served : List String
  deriving Lean.ToJson

/-- Of two approved versions, the installed newer one with its ids and with
an id only the older yields; an installed digest of neither; no installed
digest; another coordinate's digest; one approved version installed; and a
slot id, which a target pack never fills. -/
@[conformance_family]
def wireTargetPackIdsCases : Conformance.Family WireTargetPackIdsInput (Option (List String)) where
  inputs :=
    [ ⟨twoVersions, "acme/demo", some "p2", ["new"]⟩,
      ⟨twoVersions, "acme/demo", some "p2", ["old"]⟩,
      ⟨twoVersions, "acme/demo", some "p3", ["a"]⟩,
      ⟨twoVersions, "acme/demo", none, ["a"]⟩,
      ⟨twoVersions, "acme/demo", some "q1", ["other"]⟩,
      ⟨[⟨"p1", "acme/demo", some [[text "a"]]⟩], "acme/demo", some "p1", ["a"]⟩,
      ⟨[⟨"p1", "acme/demo", some [[slot "worker"]]⟩], "acme/demo", some "p1",
        ["did:key:agent"]⟩ ]
  model := fun i =>
    Configuration.wireTargetPackIds (i.approvals.map ApprovalInput.approval) i.coordinate i.installed i.served

inductive ApprovalsCase where
  | absent
  | oversized
  | unparseable
  | consistent
  | otherDigest
  | otherCoordinate
  | duplicateDigest
  deriving Lean.ToJson

def ApprovalsCase.file : ApprovalsCase → Configuration.ApprovalsFile
  | .absent => .absent
  | .oversized => .oversized
  | .unparseable => .unparseable
  | .consistent => .parsed [⟨"p1", "acme/demo", [⟨"p1", "acme/demo", "acme/tool", "r1"⟩], none⟩]
  | .otherDigest => .parsed [⟨"p1", "acme/demo", [⟨"p2", "acme/demo", "acme/tool", "r1"⟩], none⟩]
  | .otherCoordinate =>
    .parsed [⟨"p1", "acme/demo", [⟨"p1", "acme/other", "acme/tool", "r1"⟩], none⟩]
  | .duplicateDigest =>
    .parsed [⟨"p1", "acme/demo", [⟨"p1", "acme/demo", "acme/tool", "r1"⟩], none⟩,
      ⟨"p1", "acme/demo", [], none⟩]

structure ApprovalsReadInput where
  file : ApprovalsCase
  deriving Lean.ToJson

/-- Every kind of approvals record a client can find: the number of subjects
it reads, or `none` when it refuses the record. -/
@[conformance_family]
def approvalsRead : Conformance.Family ApprovalsReadInput (Option Nat) where
  inputs := [.absent, .oversized, .unparseable, .consistent, .otherDigest, .otherCoordinate,
    .duplicateDigest].map ApprovalsReadInput.mk
  model := fun i => (Configuration.approvedSubjects i.file.file).map List.length

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
