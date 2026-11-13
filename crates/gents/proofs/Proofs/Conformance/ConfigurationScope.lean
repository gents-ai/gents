import Proofs.Documents.Configuration
import Proofs.Documents.ConfigDefaults
import Lean
import Proofs.Conformance.Family

namespace Conformance.ConfigurationScope
open Configuration Lean

private def registry : Registry :=
  { tasks := fun owner id => if owner ∈ ["alice", "bob"] && id == "task"
      then some ⟨owner, ⟨"coding", true⟩⟩ else none
    agents := fun owner id => if owner ∈ ["alice", "bob"] && id == "coding"
      then some ⟨owner, ⟨some "context", "profile", true⟩⟩ else none
    contexts := fun owner id => if owner ∈ ["alice", "bob"] && id == "context"
      then some ⟨owner, { instructions := owner }⟩ else none
    profiles := fun owner id => if owner ∈ ["alice", "bob"] && id == "profile"
      then some ⟨owner, ⟨"backend", owner ++ "-model", none⟩⟩ else none
    backends := fun owner id => if owner ∈ ["alice", "bob"] && id == "backend"
      then some ⟨owner, ⟨true⟩⟩ else none }

example : (resolveTask registry "alice" "task").toOption.map (·.context.instructions) =
    some "alice" := by rfl
example : (resolveTask registry "bob" "task").toOption.map (·.inference.model) =
    some "bob-model" := by rfl
example : resolveTask registry "absent" "task" = .error .missingTask := by rfl

private def contextBoundsCases : List (Option Int × Option Int × Option Int) := [
  (some 872000, some 272000, some 872000),
  (some 872001, some 272000, some 872000),
  (some 872001, some 272000, none),
  (some 872001, some 272000, some 0),
  (some 872001, some 272000, some 128000),
  (none, some 272000, some 872000),
  (some 872001, none, some 872000),
  (some 872001, some (-1), some 872000)]

structure ContextBoundsInput where
  selected : Option Int
  modelDefault : Option Int
  maximum : Option Int
  deriving ToJson

@[conformance_family]
def contextBounds : Conformance.Family ContextBoundsInput Bool where
  inputs := contextBoundsCases.map fun (selected, modelDefault, maximum) =>
    { selected, modelDefault, maximum }
  model := fun i =>
    ConfigDefaults.contextOverrideAllowed i.selected i.modelDefault i.maximum

structure ContextPreviewInput where
  profileId : String
  modelName : String
  selected : Nat
  modelDefault : Option Nat
  maximum : Option Nat
  deriving ToJson

/-- The desired-state preview of an InferenceProfile on a backend whose recorded
catalog advertises the model: the selected window is refused with its text
exactly when it exceeds the advertised ceiling. -/
@[conformance_family]
def contextPreview : Conformance.Family ContextPreviewInput (Option String) where
  inputs := [(900000, some 272000, some 872000), (500000, some 272000, some 872000),
      (872000, some 272000, some 872000), (872001, none, some 872000),
      (872001, some 272000, none), (872001, some 900000, some 872000)].map
    fun (selected, modelDefault, maximum) =>
      { profileId := "profile", modelName := "model", selected, modelDefault, maximum }
  model := fun i =>
    ConfigDefaults.contextOverrideRejection i.profileId i.modelName (some i.selected)
      (i.modelDefault.map Int.ofNat) (i.maximum.map Int.ofNat)

private def defaultAgentCases :
    List (String × String × Bool × Option String × Option String) := [
  ("no_default", "alice", false, some "context", none),
  ("enabled_default", "alice", true, some "context", some "coding"),
  ("enabled_default_without_context", "alice", true, none, some "coding"),
  ("enabled_default_dangling_context", "alice", true, some "gone", some "coding"),
  ("disabled_default", "alice", false, some "context", some "coding"),
  ("disabled_default_without_context", "alice", false, none, some "coding"),
  ("missing_default", "alice", true, some "context", some "absent"),
  ("foreign_default", "bob", true, some "context", some "coding")]

structure DefaultAgentInput where
  name : String
  scope : String
  agentOwner : String
  agentDid : String
  enabled : Bool
  contextId : Option String
  defaultAgentDid : Option String
  deriving ToJson

@[conformance_family]
def defaultAgentPublication : Conformance.Family DefaultAgentInput Bool where
  inputs := defaultAgentCases.map fun (name, owner, enabled, contextId, defaultId) =>
    { name, scope := "alice", agentOwner := owner, agentDid := "coding", enabled, contextId
      defaultAgentDid := defaultId }
  model := fun i =>
    let reg : Registry := { registry with
      agents := fun scope id => if scope = i.scope ∧ id = i.agentDid
        then some ⟨i.agentOwner, ⟨i.contextId, "profile", i.enabled⟩⟩ else none }
    defaultAgentPublishable reg i.scope i.defaultAgentDid &&
      (resolveContext reg i.scope i.contextId).toBool

structure OauthAuth where
  kind : String
  key : Option String := none
  id : Option String := none
  version : Option String := none
  variableName : Option String := none
  accountRef : Option String := none
  allowOverage : Option Bool := none
  accessKeyId : Option String := none
  secretAccessKey : Option String := none
  sessionToken : Option String := none
  credentialsJson : Option String := none
  deriving ToJson

private def OauthAuth.ofBackend : BackendAuth → OauthAuth
  | .unauthenticated => { kind := "unauthenticated" }
  | .apiKey key => { kind := "api_key", key := some key }
  | .credential id version => { kind := "credential", id := some id, version := some version }
  | .environment name => { kind := "environment", variableName := some name }
  | .nodeOAuth account => { kind := "node_oauth", accountRef := account }
  | .nodeOAuthPool allowOverage => { kind := "node_oauth_pool", allowOverage := some allowOverage }
  | .awsCredentials id secret token =>
    { kind := "aws_credentials", accessKeyId := some id, secretAccessKey := some secret
      sessionToken := token }
  | .googleCredentials json => { kind := "google_credentials", credentialsJson := some json }

private def OauthAuth.toBackend (a : OauthAuth) : BackendAuth :=
  match a.kind with
  | "unauthenticated" => .unauthenticated
  | "api_key" => .apiKey (a.key.getD "")
  | "credential" => .credential (a.id.getD "") (a.version.getD "")
  | "environment" => .environment (a.variableName.getD "")
  | "node_oauth" => .nodeOAuth a.accountRef
  | "node_oauth_pool" => .nodeOAuthPool (a.allowOverage.getD false)
  | "aws_credentials" =>
    .awsCredentials (a.accessKeyId.getD "") (a.secretAccessKey.getD "") a.sessionToken
  | _ => .googleCredentials (a.credentialsJson.getD "")

structure OauthRow where
  owner : String
  providerKind : String
  account : Option String
  enabled : Bool
  deriving ToJson

structure OauthResolved where
  owner : String
  account : Option String
  deriving ToJson

structure OauthInput where
  name : String
  scope : String
  storedOwner : String
  providerKind : String
  auth : OauthAuth
  rows : List OauthRow
  deriving ToJson

structure OauthOutput where
  usesNodeOauth : Bool
  resolved : List OauthResolved
  deriving ToJson

private def oauthRows : List OAuthAccountRow := [
  ⟨"alice", "ChatGptCodex", none, true⟩,
  ⟨"alice", "ChatGptCodex", some "acct-1", true⟩,
  ⟨"bob", "ChatGptCodex", some "acct-2", true⟩,
  ⟨"alice", "ChatGptCodex", some "acct-off", false⟩]

private def oauthAccountCases : List (String × String × String × String × BackendAuth) := [
  ("original_none_under_scope", "alice", "alice", "ChatGptCodex", .nodeOAuth none),
  ("reference_under_scope", "alice", "alice", "ChatGptCodex", .nodeOAuth (some "acct-1")),
  ("foreign_stored_owner_uses_executing_scope", "alice", "bob", "ChatGptCodex",
    .nodeOAuth (some "acct-1")),
  ("environment_has_no_owner", "alice", "alice", "OpenAiCompatible", .environment "KEY"),
  ("foreign_node_row_rejected", "alice", "alice", "ChatGptCodex",
    .nodeOAuth (some "acct-2")),
  ("disabled_row_not_picked", "alice", "alice", "ChatGptCodex", .nodeOAuth (some "acct-off")),
  ("pool_holds_every_enabled_scope_row", "alice", "alice", "ChatGptCodex", .nodeOAuthPool false),
  ("pool_overage_choice_kept", "alice", "alice", "ChatGptCodex", .nodeOAuthPool true),
  ("aws_credentials_have_no_owner", "alice", "alice", "ClaudeMantle",
    .awsCredentials "AKIDEXAMPLE" "secret" none),
  ("aws_session_credentials_have_no_owner", "alice", "alice", "ClaudeAnthropicAws",
    .awsCredentials "AKIDEXAMPLE" "secret" (some "token")),
  ("google_credentials_have_no_owner", "alice", "alice", "ClaudeGoogleCloud",
    .googleCredentials "{}")]

@[conformance_family]
def oauthAccount : Conformance.Family OauthInput OauthOutput where
  inputs := oauthAccountCases.map fun (name, scope, storedOwner, kind, auth) =>
    { name, scope, storedOwner, providerKind := kind, auth := OauthAuth.ofBackend auth
      rows := oauthRows.map fun r =>
        { owner := r.owner, providerKind := r.kind, account := r.account, enabled := r.enabled } }
  model := fun i =>
    let auth := i.auth.toBackend
    let rows : List OAuthAccountRow :=
      i.rows.map fun r => ⟨r.owner, r.providerKind, r.account, r.enabled⟩
    let resolved := fun (r : OAuthAccountRow) => ({ owner := r.owner, account := r.account } : OauthResolved)
    { usesNodeOauth := (oauthLookupOwner i.scope auth).isSome
      resolved :=
        match auth with
        | .nodeOAuth account => ((oauthResolve i.scope i.providerKind account rows).map resolved).toList
        | .nodeOAuthPool _ => (oauthPool i.scope i.providerKind rows).map resolved
        | _ => [] }

structure DefaultReplacementInput where
  current : Option String
  candidate : Option String
  deriving ToJson

@[conformance_family]
def defaultReplacement : Conformance.Family DefaultReplacementInput Bool where
  inputs :=
    ([none, some "coding"] : List (Option String)).flatMap fun current =>
      ([none, some "coding", some "review"] : List (Option String)).map fun candidate =>
        { current, candidate }
  model := fun i => defaultAgentReplacementAllowed i.current i.candidate

private def advertisement (credentials : Option String) : DiscoveryObservation :=
  ⟨"backend", credentials, [⟨"advertised", none⟩]⟩

example : catalogFor "alice" "backend" none (some ⟨"bob", advertisement none⟩) = none := by rfl
example : catalogFor "alice" "backend" none (some ⟨"alice", advertisement none⟩) =
    some (advertisement none) := by rfl

structure DiscoveryInput where
  name : String
  requestedOwner : String
  requestedBackend : String
  requestedCredentials : Option String
  observedOwner : String
  observedCredentials : Option String
  deriving ToJson

structure CatalogObservation where
  backendId : String
  credentialNode : Option String
  modelNamesWithUnknownCapabilities : List String
  deriving ToJson

private def discoveryInputs : List DiscoveryInput :=
  [ ⟨"same_owner_shared_credentials", "alice", "backend", none, "alice", none⟩
  , ⟨"foreign_owner_same_label_shared_credentials", "alice", "backend", none, "bob", none⟩
  , ⟨"wrong_backend", "alice", "other", none, "alice", none⟩
  , ⟨"matching_oauth_scope", "alice", "backend", some "alice", "alice", some "alice"⟩
  , ⟨"foreign_oauth_scope", "alice", "backend", some "alice", "alice", some "bob"⟩
  , ⟨"oauth_is_not_shared_credentials", "alice", "backend", none, "alice", some "alice"⟩
  , ⟨"shared_catalog_is_not_oauth", "alice", "backend", some "alice", "alice", none⟩
  , ⟨"oauth_catalog_still_requires_backend_owner", "alice", "backend", some "alice", "bob",
      some "alice"⟩ ]

private def discoveryModel (i : DiscoveryInput) : Option CatalogObservation :=
  (catalogFor i.requestedOwner i.requestedBackend i.requestedCredentials
    (some ⟨i.observedOwner, advertisement i.observedCredentials⟩)).map fun catalog =>
      { backendId := catalog.backendId, credentialNode := catalog.nodeDid
        modelNamesWithUnknownCapabilities := catalog.models.map (·.modelName) }

/-- `backend_catalog` reads the observation of the backend document it was
given, so it only sees catalogs whose backend owner is the requested owner. -/
@[conformance_family]
def discovery : Conformance.Family DiscoveryInput (Option CatalogObservation) where
  inputs := discoveryInputs.filter fun i => i.observedOwner == i.requestedOwner
  model := discoveryModel

/-- model-production-gap: a backend observation carries no owner of its own;
production reaches a catalog only through the owner-qualified backend document
that holds it, so no production call is handed another owner's catalog for a
same-labelled backend and these owner-rejection cases are not reached. -/
@[conformance_family]
def discoveryUnreached : Conformance.Family DiscoveryInput (Option CatalogObservation) where
  inputs := discoveryInputs.filter fun i => i.observedOwner != i.requestedOwner
  model := discoveryModel

end Conformance.ConfigurationScope
