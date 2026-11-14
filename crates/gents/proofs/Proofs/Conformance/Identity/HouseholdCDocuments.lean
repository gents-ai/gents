import Proofs.Conformance.Identity.Conformance
import Proofs.Conformance.Identity.HouseholdDocuments
import Proofs.Conformance.Family

namespace Conformance.HouseholdC

open Lean Identity Identity.Household Identity.Conformance

structure PluginProfileInput where
  name : String
  actor : Option String
  revoked : Bool
  deriving ToJson

def pluginProfileAllowed (i : PluginProfileInput) : Bool :=
  let state := if i.revoked then Household.apply Configuration.inferenceUsersetFixture (.revoke
      ⟨"configuration", "profile", "selected_by", .entitySet "agents" "agent-doc" "executor"⟩)
    else Configuration.inferenceUsersetFixture
  Configuration.pluginModelMayUse state i.actor "profile"

/-- One delete-capable plugin root `team/worker` installed at `rootDigest`
and one Tools selection of it, pinned to `pin`, for a selecting Agent whose
managed identity is derived from the Node key when `managed`. -/
structure PluginDeleteInput where
  name : String
  installed : Bool
  selected : Bool
  managed : Bool
  pin : Option String
  rootDigest : String
  deriving ToJson

def pluginDeleteSourceCount (i : PluginDeleteInput) : Nat :=
  let root : Configuration.PluginDeleteRoot := ⟨"team/worker", i.rootDigest⟩
  let selection : Configuration.PluginDeleteSelection :=
    ⟨"tools-doc", "context-doc", "team/worker", i.pin, agentDid,
      (if i.managed then agentDid else userDid), i.selected⟩
  (Configuration.pluginDeleteSources
    { roots := (if i.installed then [root] else []), selections := [selection] }).length

structure CredentialPublicationInput where
  metadata : String
  secret : String
  authorized : Bool
  committed : Bool
  deriving ToJson

structure CredentialVersions where
  metadata : String
  secret : String
  deriving ToJson

def credentialVersions (i : CredentialPublicationInput) : CredentialVersions :=
  let result := Configuration.publishBackendCredential ⟨"old", "old"⟩ i.metadata i.secret
    i.authorized i.committed
  { metadata := result.metadataVersion, secret := result.secretVersion }

structure SessionToolInput where
  session : String
  granted : Bool
  admin : Bool
  nativeBypass : Bool
  deriving ToJson

def sessionToolAllowed (i : SessionToolInput) : Bool :=
  let grants : Configuration.ToolGrantProjection :=
    { enabled := (if i.granted then [ConfigurationFixture.sessionToolSource] else []),
      grants := (if i.granted then [ConfigurationFixture.sessionToolSource] else []),
      admins := (if i.admin then [agentDid] else []) }
  Configuration.toolSessionReadAllowed grants ConfigurationFixture.sessionToolHousehold agentDid
    i.session i.nativeBypass

def sessionToolInputs : List SessionToolInput :=
  [⟨"owned", true, false, false⟩, ⟨"shared", true, false, false⟩,
   ⟨"foreign", true, false, false⟩, ⟨"owned", false, false, false⟩,
   ⟨"foreign", false, true, true⟩, ⟨"foreign", false, true, false⟩]

end Conformance.HouseholdC

namespace Conformance.Household

open Lean Identity Identity.Household Conformance.HouseholdC

@[conformance_family]
def pluginProfile : Family PluginProfileInput Bool where
  inputs := [⟨"selected", some "agent", false⟩, ⟨"sibling", some "sibling", false⟩,
    ⟨"absent", none, false⟩, ⟨"revoked", some "agent", true⟩]
  model := pluginProfileAllowed

@[conformance_family]
def pluginDelete : Family PluginDeleteInput Nat where
  inputs := [⟨"unpinned-upgrade", true, true, true, none, "digest-b"⟩,
    ⟨"selected-installed", true, true, true, some "digest-a", "digest-a"⟩,
    ⟨"uninstalled", false, true, true, some "digest-a", "digest-a"⟩,
    ⟨"disabled", true, false, true, some "digest-a", "digest-a"⟩,
    ⟨"foreign-actor", true, true, false, some "digest-a", "digest-a"⟩,
    ⟨"replaced-digest", true, true, true, some "digest-b", "digest-a"⟩]
  model := pluginDeleteSourceCount

/-- Each publication starts from metadata and a private credential both at
version `old`. -/
@[conformance_family]
def backendCredentialPublication : Family CredentialPublicationInput CredentialVersions where
  inputs := [⟨"new", "new", true, true⟩, ⟨"new", "other", true, true⟩,
    ⟨"new", "new", false, true⟩, ⟨"new", "new", true, false⟩]
  model := credentialVersions

/-- The reading Agent owns `owned`, holds a reader grant on `shared`, and has
no relation to `foreign`, which its sibling owns. -/
@[conformance_family]
def sessionToolAccess : Family SessionToolInput Bool where
  inputs := sessionToolInputs.filter fun i => !i.nativeBypass
  model := sessionToolAllowed

/-- model-production-gap: the session tool reads as the managed Agent, and
managed-Agent signing never resolves native DAC bypass; only an HTTP request
authenticated by a NAC owner's or administrator's own key does, and no session
tool path makes one. -/
@[conformance_family]
def sessionToolAccessUnreached : Family SessionToolInput Bool where
  inputs := sessionToolInputs.filter fun i => i.nativeBypass
  model := sessionToolAllowed

/-- The native-tool scenario revalidates one cached surface tool as a selected
Agent through `revalidate_datastore_tool_in_txn` while the User revokes, disables,
deselects and changes the surface. -/
@[conformance_family]
def datastoreToolRevalidation : Family DatastoreToolUseInput Bool where
  inputs := datastoreToolUseInputs
  model := fun i =>
    Configuration.cachedDatastoreToolUsable i.visible i.enabled i.selected i.declarationMatches

end Conformance.Household
