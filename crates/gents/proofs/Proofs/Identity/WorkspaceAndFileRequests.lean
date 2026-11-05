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
  (Lean.toJson (["user", "runtime", "writer", "integrator", "other"].flatMap fun actor =>
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

/-- A canonical WorkspaceRoot publication checks the acting user's native
Node update permission by writing a fresh revision on the exact physical Node
in the same transaction as the user-signed root and its Node parent. ACP1
does not mandate that parent or fence on raw direct creates; those remain
outside the admitted canonical-publisher premise. Filtered root sets never
establish an unconfigured fallback. -/
def workspaceRootFixture : Household.State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"workspace_roots", "root", "user"⟩],
    relations := [⟨"nodes", "node", "runtime_reader", .actor "runtime"⟩,
      ⟨"workspace_roots", "root", "parent", .object "nodes" "node"⟩,
      ⟨"workspace_roots", "root", "reader", .actor "writer"⟩,
      ⟨"workspace_roots", "root", "reader", .actor "integrator"⟩], enrolled := [] }

def workspaceRootPermission (actor permission : String) : Bool :=
  Household.allowed 6 workspaceRootFixture actor "workspace_roots" "root" permission

def workspaceRootAfterRevocation : Household.State :=
  Household.apply workspaceRootFixture (.revoke
    ⟨"workspace_roots", "root", "parent", .object "nodes" "node"⟩)

def workspaceRootPublicationAllowed (actor : String) : Bool :=
  Household.allowed 6 workspaceRootFixture actor "nodes" "node" "update"

theorem runtime_root_access_is_read_only :
    workspaceRootPermission "runtime" "read" = true ∧
    workspaceRootPermission "runtime" "update" = false ∧
    workspaceRootPublicationAllowed "runtime" = false := by native_decide

theorem explicitly_selected_agents_read_root_without_update :
    workspaceRootPermission "writer" "read" = true ∧
    workspaceRootPermission "integrator" "read" = true ∧
    workspaceRootPermission "writer" "update" = false ∧
    workspaceRootPermission "integrator" "update" = false := by native_decide

def workspaceRootAfterWriterRevocation : Household.State :=
  Household.apply workspaceRootFixture (.revoke
    ⟨"workspace_roots", "root", "reader", .actor "writer"⟩)

def workspaceRootAccessCasesJson : String :=
  (Lean.toJson (["user", "runtime", "agent", "other"].flatMap fun actor =>
    ["read", "update", "delete"].map fun permission => Lean.Json.mkObj
      [("actor", Lean.toJson actor), ("permission", Lean.toJson permission),
       ("expected", Lean.toJson (workspaceRootPermission actor permission)),
       ("may_publish", Lean.toJson (workspaceRootPublicationAllowed actor)),
       ("after_parent_revocation", Lean.toJson (Household.allowed 6 workspaceRootAfterRevocation actor "workspace_roots" "root" permission)),
       ("after_writer_revocation", Lean.toJson (Household.allowed 6 workspaceRootAfterWriterRevocation actor "workspace_roots" "root" permission)),
       ("aborted_document", Lean.toJson (publish false).document)])).compress

/-- Workspace workflow configuration chooses the writer and integrator
Agents explicitly. Their selection is frozen into the recorded action plan;
the node DID and planner-provided labels are not substitute principals. -/
structure WorkspaceAgentSelection where
  writer : String
  integrator : String
  deriving BEq, DecidableEq, Repr

def freezeWorkspaceAgents (node : String) (configured : Option WorkspaceAgentSelection) :
    Option WorkspaceAgentSelection := do
  let agents ← configured
  if agents.writer.isEmpty || agents.integrator.isEmpty || agents.writer == node || agents.integrator == node
    then none else some agents

theorem missing_selection_does_not_select_node : freezeWorkspaceAgents "node" none = none := by rfl
theorem distinct_selected_agents_are_retained :
    freezeWorkspaceAgents "node" (some ⟨"writer", "integrator"⟩) = some ⟨"writer", "integrator"⟩ := by decide

/-- A filtered or malformed physical request is not proof that an exclusive
workspace binding is stale. Release requires a visible terminal request. -/
def mayReleaseExclusive (visible terminal : Bool) : Bool := visible && terminal

theorem invisible_request_does_not_release_binding : mayReleaseExclusive false true = false := by rfl

/-- A configured datastore create tool runs with a managed Agent's native
creation authority. Workspace-owned collections require their canonical
publisher so a tool cannot create an unparented child outside the scoped
inventory. Native direct writers remain subject to the admitted-writer premise. -/
def workspaceRuntimeOwnedCollections : List String :=
  ["WorkspaceRoot", "RepositoryPlacement", "IsolatedWorkspace", "WorkspaceBinding",
    "WorkspacePlacement", "WorkspaceReceipt"]

def workspaceDatastoreWriteAllowed (collection : String) : Bool :=
  !workspaceRuntimeOwnedCollections.contains collection

theorem workspace_binding_tool_create_refused :
    workspaceDatastoreWriteAllowed "WorkspaceBinding" = false := by decide

def workspaceDatastoreWriteCasesJson : String :=
  (Lean.toJson ((workspaceRuntimeOwnedCollections ++ ["Notes"]).map fun collection =>
    Lean.Json.mkObj [("collection", Lean.toJson collection),
      ("allowed", Lean.toJson (workspaceDatastoreWriteAllowed collection))])).compress

/-- Workspace publication attaches its physical Agent parent and the selected
writer and integrator in the same transaction. Every admitted child is linked
to this workspace; a child outside that publication is not a binding in the
workspace's authoritative inventory. ACP1 does not require native direct-create
operations to attach that parent. This publication contract assumes the native
node admission boundary limits writers to the canonical publisher; arbitrary
authorized raw creates can produce orphans and are outside this guarantee. -/
structure WorkspacePublication where
  document : Bool := false
  agentParent : Bool := false
  writer : Bool := false
  integrator : Bool := false
  deriving BEq, DecidableEq, Repr

def publishWorkspace (commit : Bool) : WorkspacePublication :=
  if commit then ⟨true, true, true, true⟩ else ⟨false, false, false, false⟩

def revokeWorkspaceWriter (p : WorkspacePublication) : WorkspacePublication := { p with writer := false }
def reuseWorkspace (p : WorkspacePublication) : WorkspacePublication := p

theorem failed_workspace_publication_is_empty : publishWorkspace false = ⟨false, false, false, false⟩ := by rfl
theorem workspace_reuse_does_not_restore_revocation :
    reuseWorkspace (revokeWorkspaceWriter (publishWorkspace true)) = ⟨true, true, false, true⟩ := by rfl

/-- A worktree effect starts only after the workspace and its native parent and
selected Agent relationships have committed. An unknown reservation outcome
does not authorize a fresh host effect. -/
def mayProvisionWorkspace (reservationConfirmed : Bool) : Bool := reservationConfirmed

theorem unconfirmed_reservation_has_no_host_effect : mayProvisionWorkspace false = false := by rfl

/-- After Executing is durable, a missing reservation is an unconfirmed or
legacy recovery state. It is not permission to create a replacement reservation. -/
def mayReserveWorkspace (executionStarted reservationVisible : Bool) : Bool :=
  !executionStarted && !reservationVisible

theorem executing_cannot_create_replacement_reservation : mayReserveWorkspace true false = false := by rfl

/-- Callback publication can complete provisioning or repeat an identical
result. A stale retry cannot replace an advanced workspace lifecycle. -/
def callbackWorkspacePublishable (existing desired : String) : Bool :=
  existing == desired || (existing == "provisioning" && (desired == "ready" || desired == "provisionFailed"))

theorem callback_retry_does_not_reopen_cleaning :
    callbackWorkspacePublishable "cleaning" "ready" = false := by decide

def workspacePublicationCasesJson : String :=
  (Lean.Json.mkObj [("revoked_writer_restored", Lean.toJson (reuseWorkspace (revokeWorkspaceWriter (publishWorkspace true))).writer),
    ("stale_publication_allowed", Lean.toJson (callbackWorkspacePublishable "cleaning" "ready")),
    ("aborted_document_visible", Lean.toJson (publishWorkspace false).document),
    ("effect_after_failed_reservation", Lean.toJson (mayProvisionWorkspace false))]).compress

def workspacePublicationFixture : Household.State :=
  { objects := [⟨"agents", "agent-doc", "user"⟩,
      ⟨"workspaces", "workspace", "creator"⟩,
      ⟨"workspace_children", "binding", "writer"⟩],
    relations := [⟨"workspaces", "workspace", "parent", .object "agents" "agent-doc"⟩,
      ⟨"workspaces", "workspace", "writer", .actor "writer"⟩,
      ⟨"workspaces", "workspace", "integrator", .actor "integrator"⟩,
      ⟨"workspace_children", "binding", "parent", .object "workspaces" "workspace"⟩],
    enrolled := [] }

def workspaceChildPermission (actor permission : String) : Bool :=
  Household.allowed 8 workspacePublicationFixture actor "workspace_children" "binding" permission

theorem selected_agents_see_the_same_binding :
    workspaceChildPermission "writer" "read" = true ∧
    workspaceChildPermission "integrator" "read" = true ∧
    workspaceChildPermission "other" "read" = false := by native_decide

def workspaceChildCasesJson : String :=
  (Lean.toJson (["user", "creator", "writer", "integrator", "other"].flatMap fun actor =>
    ["read", "update", "delete"].map fun permission => Lean.Json.mkObj
      [("actor", Lean.toJson actor), ("permission", Lean.toJson permission),
       ("expected", Lean.toJson (workspaceChildPermission actor permission))])).compress

/-- ACP1 allows a child owner with native relation-delete capability to unlink
a binding. Its former parent readers then cannot distinguish the hidden child
from absence. Canonical Agent capability provisioning does not grant that
delete operation; granting it explicitly invalidates inventory completeness. -/
def revokedWorkspaceChildFixture : Household.State :=
  Household.apply workspacePublicationFixture
    (.revoke ⟨"workspace_children", "binding", "parent", .object "workspaces" "workspace"⟩)

def workspaceChildRevocationCasesJson : String :=
  (Lean.toJson (["user", "creator", "writer", "integrator", "other"].map fun actor =>
    Lean.Json.mkObj [("actor", Lean.toJson actor),
      ("read", Lean.toJson (Household.allowed 8 revokedWorkspaceChildFixture actor "workspace_children" "binding" "read"))])).compress

/-- Exclusive admission reads the complete published-child set and commits a
new child together with a write to the shared workspace revision. The shared
write makes two decisions from the same prior snapshot conflict. -/
def admitExclusive (complete occupied : Bool) : Bool := complete && !occupied

theorem incomplete_inventory_cannot_admit : admitExclusive false false = false := by rfl
theorem occupied_inventory_cannot_admit : admitExclusive true true = false := by rfl

/-- Cleanup claims its physical workspace in a native authorized transaction.
The complete canonical binding inventory must contain no active binding, and
the shared workspace write records cleaning before the host effect. -/
def mayBeginCleanup (nativeUpdate complete active : Bool) : Bool :=
  nativeUpdate && complete && !active

theorem cleanup_without_native_update_cannot_begin :
    mayBeginCleanup false true false = false := by rfl
theorem cleanup_with_active_binding_cannot_begin :
    mayBeginCleanup true true true = false := by rfl

def workspaceCleanupCasesJson : String :=
  let cases : List (String × Bool × Bool) :=
    [("active", true, true), ("unauthorized", false, false), ("available", true, false)]
  (Lean.toJson (cases.map fun (name, allowed, active) => Lean.Json.mkObj
    [("name", Lean.toJson name), ("allowed", Lean.toJson allowed),
      ("active", Lean.toJson active), ("begins", Lean.toJson (mayBeginCleanup allowed true active))])).compress

def workspaceAdmissionCasesJson : String :=
  (Lean.toJson ([false, true].map fun occupied =>
    Lean.Json.mkObj [("complete", Lean.toJson true), ("occupied", Lean.toJson occupied),
      ("admitted", Lean.toJson (admitExclusive true occupied))])).compress

/-- The user-authored RepositoryPlacement and selected Agent reader grants
commit together. Reusing an existing placement does not restore a revoked
reader; the host effect still requires a visible placement and a committed
workspace reservation. -/
def workspaceRepositoryFixture : Household.State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"configuration", "repository", "user"⟩],
    relations := [⟨"configuration", "repository", "parent", .object "nodes" "node"⟩,
      ⟨"configuration", "repository", "reader", .actor "agent"⟩], enrolled := [] }

def revokedWorkspaceRepository : Household.State :=
  Household.apply workspaceRepositoryFixture (.revoke ⟨"configuration", "repository", "reader", .actor "agent"⟩)

def quickstartRepositoryRead (state : Household.State) (actor : String) : Bool :=
  Household.allowed 6 state actor "configuration" "repository" "read"

theorem repository_reader_revocation_survives_reuse :
    quickstartRepositoryRead revokedWorkspaceRepository "agent" = false := by native_decide

def workspaceQuickstartCasesJson : String :=
  (Lean.toJson (["user", "agent", "other"].map fun actor => Lean.Json.mkObj
    [("actor", Lean.toJson actor),
     ("before", Lean.toJson (quickstartRepositoryRead workspaceRepositoryFixture actor)),
     ("after_revoke", Lean.toJson (quickstartRepositoryRead revokedWorkspaceRepository actor)),
     ("aborted_document_visible", Lean.toJson (publish false).document)])).compress

def workspaceLivenessCasesJson : String :=
  (Lean.toJson ([false, true].flatMap fun visible => [false, true].map fun terminal =>
    Lean.Json.mkObj [("visible", Lean.toJson visible), ("terminal", Lean.toJson terminal),
      ("may_release", Lean.toJson (mayReleaseExclusive visible terminal))])).compress

def workspaceAgentSelectionCasesJson : String :=
  let cases : List (String × Option WorkspaceAgentSelection) :=
    [("missing", none), ("separate", some ⟨"writer", "integrator"⟩),
    ("same", some ⟨"writer", "writer"⟩), ("node", some ⟨"node", "integrator"⟩),
    ("blank", some ⟨"", "integrator"⟩)]
  (Lean.toJson (cases.map fun (name, configured) =>
      let selected := freezeWorkspaceAgents "node" configured
      Lean.Json.mkObj [("name", Lean.toJson name), ("accepted", Lean.toJson selected.isSome),
        ("writer", Lean.toJson (selected.map (·.writer))),
        ("integrator", Lean.toJson (selected.map (·.integrator)))] )).compress

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

def workspaceCompletionAccessCasesJson : String :=
  (Lean.toJson (["agent", "other"].map fun actor => Lean.Json.mkObj
    [("actor", Lean.toJson actor),
     ("read", Lean.toJson (Household.allowed 5 workspaceFixture actor "agent_children" "workspace" "read"))])).compress

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
