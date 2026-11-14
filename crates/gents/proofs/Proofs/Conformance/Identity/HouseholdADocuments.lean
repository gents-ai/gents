import Proofs.Conformance.Identity.Conformance
import Proofs.Conformance.Identity.HouseholdStore
import Proofs.Conformance.Family

namespace Conformance.HouseholdA

open Lean Identity Identity.Household

structure ToolReferencesInput where
  name : String
  agentsEnabled : Bool
  targets : List String
  services : List String
  surfaces : List String
  deriving ToJson

structure ToolReferences where
  agentTarget : List String
  toolServiceRegistry : List String
  datastoreToolSurface : List String
  deriving ToJson

def toolReferencesOf (i : ToolReferencesInput) : ToolReferences :=
  let references := Configuration.toolSelectionReferences
    ⟨i.agentsEnabled, i.targets, i.services, i.surfaces⟩
  let ids (collection : String) := (references.lookup collection).getD []
  { agentTarget := ids "AgentTarget", toolServiceRegistry := ids "ToolServiceRegistry",
    datastoreToolSurface := ids "DatastoreToolSurface" }

structure SelfConfigInput where
  phase : String
  actor : String
  document : String
  deriving ToJson

def selfConfigState (phase : String) : State :=
  if phase == "before" then Configuration.inferenceUsersetFixture
  else if phase == "revoked" then Household.apply Configuration.inferenceSelfConfigFixture
    (.revoke ⟨"agents", "agent-doc", "config_writer", .actor "agent"⟩)
  else if phase == "direct-after-revoke" then Household.apply
    (Household.apply Configuration.inferenceSelfConfigFixture
      (.grant ⟨"configuration", "execution", "writer", .actor "agent"⟩))
    (.revoke ⟨"agents", "agent-doc", "config_writer", .actor "agent"⟩)
  else Configuration.inferenceSelfConfigFixture

def selfConfigUpdate (i : SelfConfigInput) : Bool :=
  Household.allowed 8 (selfConfigState i.phase) i.actor "configuration" i.document "update"

def selfConfigManager (i : SelfConfigInput) : Bool :=
  Household.mayManage (selfConfigState i.phase) i.actor
    ⟨"configuration", i.document, "selected_by", .entitySet "configuration" "profile" "selected_by"⟩

def selfConfigInputs : List SelfConfigInput :=
  [⟨"before", "agent", "execution"⟩, ⟨"granted", "agent", "profile"⟩,
   ⟨"granted", "agent", "context"⟩, ⟨"granted", "agent", "tools"⟩,
   ⟨"granted", "agent", "execution"⟩, ⟨"granted", "agent", "new-execution"⟩,
   ⟨"granted", "sibling", "execution"⟩, ⟨"before", "agent", "backend-metadata"⟩,
   ⟨"granted", "agent", "backend-metadata"⟩, ⟨"granted", "sibling", "backend-metadata"⟩,
   ⟨"revoked", "agent", "backend-metadata"⟩, ⟨"revoked", "agent", "execution"⟩,
   ⟨"revoked", "agent", "new-execution"⟩, ⟨"revoked", "agent", "profile"⟩,
   ⟨"revoked", "agent", "context"⟩, ⟨"revoked", "agent", "tools"⟩,
   ⟨"direct-after-revoke", "agent", "execution"⟩]

structure BindingInput where
  name : String
  sameNode : Bool
  destinationApproved : Bool
  credentialVersionMatches : Bool
  deriving ToJson

def bindingAccess (i : BindingInput) : Configuration.BackendCredentialAccess :=
  ⟨true, true, true, true, true, i.sameNode, true, true, i.destinationApproved,
    i.credentialVersionMatches⟩

def bindingInputs : List BindingInput :=
  [⟨"authorized", true, true, true⟩, ⟨"foreign-node", false, true, true⟩,
   ⟨"unapproved-destination", true, false, true⟩, ⟨"rotated-credential", true, true, false⟩]

end Conformance.HouseholdA

namespace Conformance.Household

open Lean Identity Identity.Household Conformance.HouseholdA

@[conformance_family]
def toolSelectionReferences : Family ToolReferencesInput ToolReferences where
  inputs := [⟨"empty", false, [], [], []⟩, ⟨"disabled-target", false, ["target"], [], []⟩,
    ⟨"independent-sets", true, ["target-a", "target-b"], ["service-a", "service-b"],
      ["surface-a", "surface-b"]⟩,
    ⟨"duplicates", true, ["target", "target"], ["service", "service"], ["surface", "surface"]⟩]
  model := toolReferencesOf

/-- The selection-change scenario stages the retry edge through the Agent,
profile and execution chain, so it observes only the retry document; the
sampling cases are replayed natively by `usersetNativeVisibility`. -/
@[conformance_family]
def usersetRetrySelection : Family UsersetInput Bool where
  inputs := usersetInputs.filter fun i => i.document == "retry"
  model := usersetVisible

@[conformance_family]
def inferenceSelfConfigUpdate : Family SelfConfigInput Bool where
  inputs := selfConfigInputs
  model := selfConfigUpdate

/-- The inference scenario links documents to the profile only by rebinding
its execution while granted and by deleting the new execution's link after the
revoke; the backend link is driven by the root-crate backend scenario. -/
@[conformance_family]
def inferenceSelfConfigManager : Family SelfConfigInput Bool where
  inputs := [⟨"granted", "agent", "execution"⟩, ⟨"revoked", "agent", "new-execution"⟩]
  model := selfConfigManager

@[conformance_family]
def inferenceSelfConfigBackendManager : Family SelfConfigInput Bool where
  inputs := [⟨"before", "agent", "backend-metadata"⟩, ⟨"granted", "agent", "backend-metadata"⟩,
    ⟨"revoked", "agent", "backend-metadata"⟩]
  model := selfConfigManager

/-- The private resolver binds a credential to its node, destination and
rotation; the profile, backend, runtime-actor and secret facts hold for it. -/
@[conformance_family]
def backendCredentialBinding : Family BindingInput Bool where
  inputs := bindingInputs
  model := fun i => Configuration.runtimeMayUseBackendCredential (bindingAccess i) false false

/-- Backend metadata carries a credential reference whose version must equal
the approved one; only the version varies here. -/
@[conformance_family]
def backendCredentialReferenceVersion : Family BindingInput Bool where
  inputs := bindingInputs.filter fun i => i.sameNode && i.destinationApproved
  model := fun i => Configuration.runtimeMayUseBackendCredential (bindingAccess i) false false

end Conformance.Household
