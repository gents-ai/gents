import Proofs.Conformance.Identity.Conformance
import Proofs.Conformance.Identity.HouseholdStore
import Proofs.Conformance.Family

namespace Conformance.Household

open Lean Identity Identity.Household

def affectedDocuments (name : String) : List _root_.Configuration.Affected.Document :=
  let docs := [_root_.Configuration.Affected.missingSchedule, _root_.Configuration.Affected.task,
    _root_.Configuration.Affected.agent, _root_.Configuration.Affected.tools]
  let collision : _root_.Configuration.Affected.Document :=
    ⟨"tools-collision", ["surface-a", "surface-b"],
      _root_.Configuration.Affected.uniqueToolNames ["duplicate", "duplicate"]⟩
  let surfaceA : _root_.Configuration.Affected.Document := ⟨"surface-a", [], true⟩
  let surfaceB : _root_.Configuration.Affected.Document := ⟨"surface-b", [], true⟩
  let registry : _root_.Configuration.Registry :=
    { tasks := fun _ _ => none,
      agents := fun scope id => if scope == "owner" && id == "disabled-agent" then
        some ⟨"owner", ⟨none, "profile", false⟩⟩ else none,
      contexts := fun _ _ => none, profiles := fun _ _ => none, backends := fun _ _ => none }
  let defaultNode : _root_.Configuration.Affected.Document :=
    ⟨"default-node", ["disabled-agent"],
      _root_.Configuration.defaultAgentPublishable registry "owner" (some "disabled-agent")⟩
  let disabledAgent : _root_.Configuration.Affected.Document := ⟨"disabled-agent", ["profile"], true⟩
  let profile : _root_.Configuration.Affected.Document := ⟨"profile", ["backend"], true⟩
  if name == "visible-reference" then docs ++ [_root_.Configuration.Affected.schedule]
  else if name == "shared-surface-collision" then [collision, surfaceA, surfaceB]
  else if name == "disabled-default" then [defaultNode, disabledAgent, profile]
  else docs

structure AffectedInput where
  name : String
  documents : List String
  changed : List String
  deriving ToJson

@[conformance_family]
def configurationAffected : Family AffectedInput Bool where
  inputs := [("unrelated-edit", ["tools"]), ("changed-referrer", ["trigger"]),
    ("shared-target-edit", ["task"]), ("removed-target", ["schedule"]),
    ("visible-reference", ["trigger"]), ("shared-surface-collision", ["surface-a"]),
    ("disabled-default", ["disabled-agent"])].map fun (name, changed) =>
      { name, documents := (affectedDocuments name).map fun document => document.key, changed }
  model := fun i => _root_.Configuration.Affected.accepted (affectedDocuments i.name) i.changed

structure UxDatastoreInput where
  name : String
  visible : Bool
  enabled : Bool
  declared : Bool
  unchanged : Bool
  deriving ToJson

@[conformance_family]
def uxDatastore : Family UxDatastoreInput Bool where
  inputs := [⟨"allowed", true, true, true, true⟩, ⟨"revoked", false, true, true, true⟩,
    ⟨"disabled", true, false, true, true⟩, ⟨"not-declared", true, true, false, true⟩,
    ⟨"changed", true, true, true, false⟩]
  model := fun i => Configuration.cachedUxDatastoreToolUsable i.visible i.enabled i.declared i.unchanged

structure DatastoreToolUseInput where
  name : String
  visible : Bool
  enabled : Bool
  selected : Bool
  declarationMatches : Bool
  deriving ToJson

def datastoreToolUseInputs : List DatastoreToolUseInput :=
  [⟨"allowed", true, true, true, true⟩, ⟨"revoked", false, true, true, true⟩,
   ⟨"disabled", true, false, true, true⟩, ⟨"deselected", true, true, false, true⟩,
   ⟨"changed", true, true, true, false⟩]

@[conformance_family]
def datastoreToolSelection : Family DatastoreToolUseInput Bool where
  inputs := datastoreToolUseInputs
  model := fun i =>
    Configuration.cachedDatastoreToolUsable i.visible i.enabled i.selected i.declarationMatches

structure CallbackClaimInput where
  claimTime : Bool
  retainedConfiguration : Bool
  deriving ToJson

@[conformance_family]
def callbackClaimRecorded : Family CallbackClaimInput Bool where
  inputs := [(false, false), (true, false), (false, true), (true, true)].map
    fun (claimTime, retainedConfiguration) => { claimTime, retainedConfiguration }
  model := fun i => Identity.UnattendedReceipt.visibleCallbackClaim i.claimTime i.retainedConfiguration

structure RemovalInput where
  name : String
  target : String
  returned : List String
  deriving ToJson

@[conformance_family]
def configurationRemoval : Family RemovalInput Bool where
  inputs := [(["doc"], "exact"), ([], "empty"), (["other"], "different"),
    (["doc", "doc"], "duplicate")].map fun (returned, name) => { name, target := "doc", returned }
  model := fun i => _root_.Configuration.Affected.removalConfirmed i.target i.returned

structure CallbackAccessInput where
  actor : String
  role : String
  id : String
  revoked : Bool
  deriving ToJson

@[conformance_family]
def callbackConfigurationAccess : Family CallbackAccessInput Bool where
  inputs := [(Identity.Conformance.runtimeDid, "binding", false),
    (Identity.Conformance.runtimeDid, "callback", false),
    (Identity.Conformance.runtimeDid, "module", false), (Identity.Conformance.agentDid, "binding", false),
    (Identity.Conformance.agentDid, "callback", false), (Identity.Conformance.agentDid, "module", false),
    (Identity.Conformance.siblingDid, "binding", false), (Identity.Conformance.agentDid, "binding", true)].map
    fun (actor, id, revoked) => { actor, role := roleOf actor, id, revoked }
  model := fun i =>
    let granted := Automation.publishCallbackReaders Identity.Conformance.ConfigurationFixture.callbackState
      Identity.Conformance.userDid "binding" "callback" Identity.Conformance.agentDid true
    let state := if i.revoked then Household.apply granted
      (.revoke ⟨"configuration", "binding", "reader", .actor Identity.Conformance.agentDid⟩) else granted
    Household.allowed 5 state i.actor "configuration" i.id "read"

structure NodeBindingInput where
  sameNode : Bool
  hasDoc : Bool
  visible : Bool
  deriving ToJson

@[conformance_family]
def nodeBinding : Family NodeBindingInput Bool where
  inputs := [⟨true, true, true⟩, ⟨false, true, true⟩, ⟨true, false, true⟩, ⟨true, true, false⟩]
  model := fun i =>
    Configuration.usableNodeBinding "node" (if i.sameNode then "node" else "other")
      (if i.hasDoc then "doc" else "") "node" i.visible

structure ProjectionInput where
  revoked : Bool
  actor : String
  permission : String
  deriving ToJson

@[conformance_family]
def projectionBinding : Family ProjectionInput Bool where
  inputs := [false, true].flatMap fun revoked =>
    ["user", "creator", "runtime", "agent", "sibling", "outsider"].flatMap fun actor =>
      ["read", "update"].map fun permission => { revoked, actor, permission }
  model := fun i =>
    let state := if i.revoked then Household.apply Configuration.projectionBindingFixture
      (.revoke ⟨"configuration", "binding", "parent", .object "nodes" "node"⟩)
      else Configuration.projectionBindingFixture
    Household.allowed 5 state i.actor "configuration" "binding" i.permission

end Conformance.Household
