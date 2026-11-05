import Lean
import Proofs.Identity.Household

namespace Identity.WorkspaceAndFileRequests

/-- Retained request schemas do not have a production publisher or consumer in
the direct-HTTP inventory flow. Their scope names a node, and their receipt
signer is the target node; none contains a physical session or selected Agent.
If used, the authenticated creator owns the request. A Node parent and explicit
runtime writer must commit with creation; the payload fields grant no access. -/
structure Publication where
  document : Bool := false
  nodeParent : Bool := false
  runtimeWriter : Bool := false
  deriving BEq, DecidableEq, Repr

def publish (committed : Bool) : Publication :=
  if committed then ⟨true, true, true⟩ else ⟨false, false, false⟩

def revokeRuntimeWriter (p : Publication) : Publication := { p with runtimeWriter := false }
def observeExisting (p : Publication) : Publication := p

theorem aborted_publication_has_no_partial_relationships : publish false = ⟨false, false, false⟩ := by rfl
theorem duplicate_does_not_restore_revoked_relationship :
    observeExisting (revokeRuntimeWriter (publish true)) = ⟨true, true, false⟩ := by rfl

def requestFixture : Household.State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"configuration", "request", "user"⟩],
    relations := [⟨"configuration", "request", "parent", .object "nodes" "node"⟩,
      ⟨"configuration", "request", "writer", .actor "runtime"⟩], enrolled := [] }

def requestPermission (actor permission : String) : Bool :=
  Household.allowed 5 requestFixture actor "configuration" "request" permission

theorem requester_and_explicit_runtime_have_native_access :
    requestPermission "user" "read" = true ∧ requestPermission "user" "update" = true ∧
    requestPermission "runtime" "read" = true ∧ requestPermission "runtime" "update" = true ∧
    requestPermission "other" "read" = false ∧ requestPermission "other" "update" = false := by native_decide

def revokedRuntimeFixture : Household.State :=
  Household.apply requestFixture (.revoke ⟨"configuration", "request", "writer", .actor "runtime"⟩)

theorem runtime_access_requires_its_explicit_grant :
    Household.allowed 5 revokedRuntimeFixture "runtime" "configuration" "request" "read" = false ∧
    Household.allowed 5 revokedRuntimeFixture "runtime" "configuration" "request" "update" = false := by native_decide

def requestAccessCasesJson : String :=
  (Lean.toJson (["user", "runtime", "agent", "other"].flatMap fun actor =>
    ["read", "update", "delete"].map fun permission => Lean.Json.mkObj
      [("actor", Lean.toJson actor), ("permission", Lean.toJson permission),
       ("expected", Lean.toJson (requestPermission actor permission))])).compress

/-- Filtering can hide every configured root. An empty actor-scoped result is
therefore indistinguishable from an unconfigured store, while the existing
unconfigured projection admits an authored root. A reader needs a native
completeness premise before using that fallback. -/
def rootProjectionAllows (visibleConfigured : Bool) : Bool := !visibleConfigured
def rootFallbackAllowed (allPolicyVisible visibleConfigured : Bool) : Bool :=
  allPolicyVisible && rootProjectionAllows visibleConfigured

theorem hidden_policy_looks_unconfigured :
    rootProjectionAllows false = true ∧ rootFallbackAllowed false false = false := by decide

theorem complete_unconfigured_store_keeps_fallback :
    rootFallbackAllowed true false = true := by decide

def rootCompletenessCasesJson : String :=
  (Lean.toJson ([false, true].flatMap fun complete => [false, true].map fun configured =>
    Lean.Json.mkObj [("complete", Lean.toJson complete),
      ("visible_configured", Lean.toJson configured),
      ("fallback_allowed", Lean.toJson (rootFallbackAllowed complete configured))])).compress

/-- Workspace effects are Agent-authored, while their logical parent grants
the user access. Host-only placement and operator root configuration must not
be inferred from that child relationship. -/
def workspaceFixture : Household.State :=
  { objects := [⟨"agents", "agent-doc", "user"⟩,
      ⟨"agent_children", "workspace", "agent"⟩],
    relations := [⟨"agent_children", "workspace", "parent", .object "agents" "agent-doc"⟩], enrolled := [] }

theorem workspace_parent_supplies_user_read_without_unrelated_access :
    Household.allowed 5 workspaceFixture "user" "agent_children" "workspace" "read" = true ∧
    Household.allowed 5 workspaceFixture "agent" "agent_children" "workspace" "update" = true ∧
    Household.allowed 5 workspaceFixture "other" "agent_children" "workspace" "read" = false := by native_decide

/-- The callback workspace owner retains the selected held Agent access for
both reads and writes. Matching this execution context is not a substitute for
the native document permission check performed by DefraDB. -/
def callbackActorMatches (accessActor selectedAgent : String) : Bool :=
  !selectedAgent.isEmpty && accessActor == selectedAgent

theorem callback_node_identity_is_not_the_agent_actor :
    callbackActorMatches "node" "agent" = false ∧ callbackActorMatches "agent" "agent" = true := by decide

def callbackActorCasesJson : String :=
  (Lean.toJson ([("agent", "agent", "agent"), ("node", "node", "agent"), ("empty", "", "")].map fun (role, actor, selected) => Lean.Json.mkObj
    [("actor", Lean.toJson role), ("expected", Lean.toJson (callbackActorMatches actor selected))])).compress

/-- Completion effects are published by the same held Agent selected for the
request. The node DID is host location and does not provide a substitute signer. -/
def completionActorMatches (held selected : String) : Bool :=
  !selected.isEmpty && held == selected

theorem completion_requires_selected_agent :
    completionActorMatches "agent" "agent" = true ∧
    completionActorMatches "node" "agent" = false := by decide

def completionActorCasesJson : String :=
  (Lean.toJson ([("agent", "agent", "agent"), ("node", "node", "agent"), ("empty", "", "")].map fun (role, held, selected) => Lean.Json.mkObj
    [("actor", Lean.toJson role), ("expected", Lean.toJson (completionActorMatches held selected))])).compress

end Identity.WorkspaceAndFileRequests
