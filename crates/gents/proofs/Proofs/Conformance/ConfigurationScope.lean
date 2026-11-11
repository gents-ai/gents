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

structure ContextLimit where
  negative : Bool
  magnitude : Nat
  deriving ToJson

private def ContextLimit.ofInt (n : Int) : ContextLimit := { negative := n < 0, magnitude := n.natAbs }

private def ContextLimit.toInt (l : ContextLimit) : Int :=
  if l.negative then -(l.magnitude : Int) else (l.magnitude : Int)

structure ContextBoundsInput where
  selected : Option ContextLimit
  modelDefault : Option ContextLimit
  maximum : Option ContextLimit
  deriving ToJson

@[conformance_family]
def contextBounds : Conformance.Family ContextBoundsInput Bool where
  inputs := contextBoundsCases.map fun (selected, modelDefault, maximum) =>
    { selected := selected.map ContextLimit.ofInt
      modelDefault := modelDefault.map ContextLimit.ofInt
      maximum := maximum.map ContextLimit.ofInt }
  model := fun i =>
    ConfigDefaults.contextOverrideAllowed (i.selected.map ContextLimit.toInt)
      (i.modelDefault.map ContextLimit.toInt) (i.maximum.map ContextLimit.toInt)

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

private def discoveryCaseJson (name requestedOwner observedOwner requestedBackend : String)
    (requestedCredentials observedCredentials : Option String) : Json :=
  let observed := advertisement observedCredentials
  let result := catalogFor requestedOwner requestedBackend requestedCredentials (some ⟨observedOwner, observed⟩)
  let catalogJson := fun (c : DiscoveryObservation) => Json.mkObj
    [("backend_id", toJson c.backendId), ("credential_node", toJson c.nodeDid),
     ("model_names_with_unknown_capabilities", toJson (c.models.map (·.modelName)))]
  Json.mkObj [("name", toJson name), ("requested_owner", toJson requestedOwner),
    ("requested_backend", toJson requestedBackend), ("requested_credentials", toJson requestedCredentials),
    ("observed_owner", toJson observedOwner), ("observed", catalogJson observed),
    ("result", result.map catalogJson |>.getD Json.null)]

def discoveryCasesJson : String := (toJson [
  discoveryCaseJson "same_owner_shared_credentials" "alice" "alice" "backend" none none,
  discoveryCaseJson "foreign_owner_same_label_shared_credentials" "alice" "bob" "backend" none none,
  discoveryCaseJson "wrong_backend" "alice" "alice" "other" none none,
  discoveryCaseJson "matching_oauth_scope" "alice" "alice" "backend" (some "alice") (some "alice"),
  discoveryCaseJson "foreign_oauth_scope" "alice" "alice" "backend" (some "alice") (some "bob"),
  discoveryCaseJson "oauth_is_not_shared_credentials" "alice" "alice" "backend" none (some "alice"),
  discoveryCaseJson "shared_catalog_is_not_oauth" "alice" "alice" "backend" (some "alice") none,
  discoveryCaseJson "oauth_catalog_still_requires_backend_owner" "alice" "bob" "backend" (some "alice") (some "alice")
  ]).compress

end Conformance.ConfigurationScope
