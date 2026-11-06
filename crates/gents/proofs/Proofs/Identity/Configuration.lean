import Proofs.Identity.Household
import Proofs.ToolPolicy.Types

namespace Identity.Configuration
open Household

/-- The native source key identifies the physical configuration, its selected
Agent, and the tool purpose. Permissions remain separate native relations. -/
structure ToolGrantSource where
  toolsDoc : String
  agent : String
  purpose : String
  permission : String
  deriving BEq, DecidableEq, Repr

def validToolGrantSource (source : ToolGrantSource) : Bool :=
  !source.toolsDoc.isEmpty && !source.agent.isEmpty && !source.purpose.isEmpty &&
    !source.permission.isEmpty

/-- Schema commands expose collection/version reads, create/patch/activation
and materialization, migration publication, and view creation. These names
are the pinned DefraDB node-operation relations for those operations. -/
def schemaToolPermissions : List String :=
  ["get-collection", "patch-collection", "set-migration", "add-view"]

-- The category scope is owned by the configured tool surface.
def selfConfigCategoryEnabled (scope : ToolPolicy.EndpointScope String Unit) (category : String) : Bool :=
  match scope with
  | .none => false
  | .all => true
  | .only keys _ => decide (category ∈ keys)

-- An empty category scope does not expose self-configuration.
def selfConfigScopeEnabled : ToolPolicy.EndpointScope String Unit → Bool
  | .none => false
  | .all => true
  | .only keys _ => decide keys.Nonempty

def datastoreScopeEnabled : ToolPolicy.EndpointScope (String × String) (Finset String) → Bool
  | .none => false
  | .all => true
  | .only keys _ => decide keys.Nonempty

/-- Project operation requirements from the existing configured tool surface
after its stable ceiling, independently of transient service availability.
These source-specific node permissions do not confer document ACP, bypass,
or the ability to grant permissions. Native authorization remains decisive. -/
def toolOperationSources (surface : ToolPolicy.Surface) (packInstall : Bool) : List (String × String) :=
  let emit (purpose : String) (enabled : Bool) (permissions : List String) : List (String × String) :=
    if enabled then permissions.map (fun permission => (purpose, permission)) else []
  emit "schema" surface.schemaManagement schemaToolPermissions ++
  emit "datastore-query" (datastoreScopeEnabled surface.queryTools) ["get-collection"] ++
  emit "datastore-write" (datastoreScopeEnabled surface.writeTools) ["get-collection"] ++
  emit "session-history" surface.sessionHistory ["read-document"] ++
  emit "goal" surface.goalTools ["read-document", "update-document"] ++
  emit "goal-create" (surface.goalTools && surface.goalCreate)
    ["read-document", "update-document", "add-dac-relation"] ++
  ([("self-config:agent", "agent"), ("self-config:node", "node"),
    ("self-config:tools", "tools"), ("self-config:profile", "profile")].flatMap fun (purpose, category) =>
      emit purpose (surface.selfConfig && selfConfigCategoryEnabled surface.selfConfigCategories category)
        ["delete-dac-relation"]) ++
  emit "pack-install" (surface.selfConfig && selfConfigScopeEnabled surface.selfConfigCategories && packInstall)
    ["delete-dac-relation"]

/-- Tool selections and their source-scoped native grants change together.
Direct grants and ownership are separate native paths. The authorization and
commit observations are supplied by the native transaction, not an application
permission check. -/
structure ToolGrantProjection where
  enabled : List ToolGrantSource := []
  grants : List ToolGrantSource := []
  direct : List (String × String) := []
  owners : List String := []
  admins : List String := []
  deriving DecidableEq, Repr

def toolGrantEffective (state : ToolGrantProjection) (actor permission : String) : Bool :=
  state.owners.contains actor || state.admins.contains actor ||
    state.direct.contains (actor, permission) ||
    state.grants.any (fun grant => grant.agent == actor && grant.permission == permission)

/-- For a request without native DAC bypass, a session tool's node-operation
grant does not expand document visibility. HTTP may resolve bypass for a NAC
owner or administrator; managed-Agent signing does not allow bypass. -/
def toolSessionReadAllowed (grants : ToolGrantProjection) (household : State)
    (actor session : String) (nativeBypass : Bool) : Bool :=
  toolGrantEffective grants actor "read-document" &&
    (nativeBypass || Household.allowed 5 household actor "sessions" session "read")

theorem session_tool_cannot_bypass_document_acp (grants : ToolGrantProjection)
    (household : State) (actor session : String)
    (denied : Household.allowed 5 household actor "sessions" session "read" = false) :
    toolSessionReadAllowed grants household actor session false = false := by
  simp [toolSessionReadAllowed, denied]

def changeToolGrant (state : ToolGrantProjection) (source : ToolGrantSource)
    (enable authorized committed : Bool) : ToolGrantProjection :=
  if validToolGrantSource source && authorized && committed then
    let enabled := state.enabled.filter (· != source)
    let grants := state.grants.filter (· != source)
    { state with
      enabled := (if enable then source :: enabled else enabled)
      grants := (if enable then source :: grants else grants) }
  else state

/-- Rebinding a tool replaces its old source and adds its new one in the same
native transaction, so neither a failed removal nor a failed addition publishes
the new configuration. -/
def replaceToolGrant (state : ToolGrantProjection) (old next : ToolGrantSource)
    (authorized committed : Bool) : ToolGrantProjection :=
  if validToolGrantSource old && validToolGrantSource next && authorized && committed then
    let enabled := state.enabled.filter (fun source => source != old && source != next)
    let grants := state.grants.filter (fun source => source != old && source != next)
    { state with
      enabled := next :: enabled
      grants := next :: grants }
  else state

def validToolGrantGroup : List ToolGrantSource → Bool
  | [] => false
  | first :: rest =>
      validToolGrantSource first && rest.all (fun source =>
        validToolGrantSource source && source.toolsDoc == first.toolsDoc &&
          source.agent == first.agent && source.purpose == first.purpose)

/-- One tool selection may require several native permissions. Configuration
and every source-scoped relation commit together or all remain unchanged. -/
def changeToolGrantGroup (state : ToolGrantProjection) (sources : List ToolGrantSource)
    (enable authorized committed : Bool) : ToolGrantProjection :=
  if validToolGrantGroup sources && authorized && committed then
    let enabled := state.enabled.filter (fun source => !sources.contains source)
    let grants := state.grants.filter (fun source => !sources.contains source)
    { state with
      enabled := (if enable then sources ++ enabled else enabled)
      grants := (if enable then sources ++ grants else grants) }
  else state

/-- The pinned native node policy lets its owner or an administrator manage
permission relations. Tool document write access does not confer this role. -/
def nativeToolGrantManager (state : ToolGrantProjection) (actor : String) : Bool :=
  state.owners.contains actor || state.admins.contains actor

def configureToolGrantGroup (state : ToolGrantProjection) (household : State)
    (sources : List ToolGrantSource) (grantor : String) (enable committed : Bool) : ToolGrantProjection :=
  let writable := match sources.head? with
    | some source => Household.allowed 5 household grantor "configuration" source.toolsDoc "update"
    | none => false
  changeToolGrantGroup state sources enable
    (writable && nativeToolGrantManager state grantor) committed

/-- Native usersets represent shared selections without scanning Agents. A
permission edge names the physical Tools/purpose group, a context edge names
the physical AgentContext, and an Agent edge names its selected context. -/
structure ToolGroup where
  toolsDoc : String
  purpose : String
  deriving BEq, DecidableEq, Repr

structure ToolPermissionEdge where
  group : ToolGroup
  permission : String
  deriving BEq, DecidableEq, Repr

structure ToolContextEdge where
  group : ToolGroup
  contextDoc : String
  deriving BEq, DecidableEq, Repr

structure ToolAgentEdge where
  contextDoc : String
  agent : String
  deriving BEq, DecidableEq, Repr

structure ToolGrantHierarchy where
  permissions : List ToolPermissionEdge := []
  contexts : List ToolContextEdge := []
  agents : List ToolAgentEdge := []
  direct : List (String × String) := []
  owners : List String := []
  admins : List String := []
  deriving DecidableEq, Repr

/-- The hierarchy is a representation of the same per-Agent projection, not
an additional authorization decision. -/
def flattenToolHierarchy (state : ToolGrantHierarchy) : ToolGrantProjection :=
  let sources := state.permissions.flatMap fun edge =>
    state.contexts.flatMap fun context =>
      if edge.group == context.group then
        state.agents.filterMap fun member =>
          if member.contextDoc == context.contextDoc then
            some ⟨edge.group.toolsDoc, member.agent, edge.group.purpose, edge.permission⟩
          else none
      else []
  { enabled := sources, grants := sources, direct := state.direct,
    owners := state.owners, admins := state.admins }

def validToolGroup (group : ToolGroup) : Bool :=
  !group.toolsDoc.isEmpty && !group.purpose.isEmpty

def setToolHierarchy (state : ToolGrantHierarchy) (group : ToolGroup)
    (permissions : List String) (enable authorized committed : Bool) : ToolGrantHierarchy :=
  if validToolGroup group && !permissions.isEmpty && permissions.all (!·.isEmpty) &&
      authorized && committed then
    let other := state.permissions.filter fun edge =>
      edge.group != group || !permissions.contains edge.permission
    { state with permissions :=
      if enable then (permissions.map fun permission => ⟨group, permission⟩) ++ other else other }
  else state

def rebindToolContext (state : ToolGrantHierarchy) (contextDoc : String)
    (next : ToolGroup) (authorized committed : Bool) : ToolGrantHierarchy :=
  if !contextDoc.isEmpty && validToolGroup next && authorized && committed then
    let other := state.contexts.filter fun edge =>
      edge.contextDoc != contextDoc || edge.group.purpose != next.purpose
    { state with contexts := ⟨next, contextDoc⟩ :: other }
  else state

def rebindToolAgent (state : ToolGrantHierarchy) (agent contextDoc : String)
    (authorized committed : Bool) : ToolGrantHierarchy :=
  if !agent.isEmpty && !contextDoc.isEmpty && authorized && committed then
    { state with agents := ⟨contextDoc, agent⟩ :: state.agents.filter (fun edge => edge.agent != agent) }
  else state

theorem failed_tool_hierarchy_change_is_unchanged (state : ToolGrantHierarchy)
    (group : ToolGroup) (permissions : List String) (enable authorized : Bool) :
    setToolHierarchy state group permissions enable authorized false = state := by
  simp [setToolHierarchy]

theorem failed_tool_group_change_is_unchanged (state : ToolGrantProjection)
    (sources : List ToolGrantSource) (enable authorized : Bool) :
    changeToolGrantGroup state sources enable authorized false = state := by
  simp [changeToolGrantGroup]

theorem failed_tool_rebinding_is_unchanged (state : ToolGrantProjection)
    (old next : ToolGrantSource) (authorized : Bool) :
    replaceToolGrant state old next authorized false = state := by
  simp [replaceToolGrant]

theorem unauthorized_tool_change_is_unchanged (state : ToolGrantProjection)
    (source : ToolGrantSource) (enable committed : Bool) :
    changeToolGrant state source enable false committed = state := by
  simp [changeToolGrant]

theorem failed_tool_change_is_unchanged (state : ToolGrantProjection)
    (source : ToolGrantSource) (enable authorized : Bool) :
    changeToolGrant state source enable authorized false = state := by
  simp [changeToolGrant]

theorem tool_change_preserves_direct_and_owner (state : ToolGrantProjection)
    (source : ToolGrantSource) (enable authorized committed : Bool) :
    (changeToolGrant state source enable authorized committed).direct = state.direct ∧
    (changeToolGrant state source enable authorized committed).owners = state.owners ∧
    (changeToolGrant state source enable authorized committed).admins = state.admins := by
  unfold changeToolGrant
  split <;> simp

/-- Supported runtime-local configuration uses canonical bundled schemas. A
client-signed update can name only mutable fields; immutable replacement values
are checked and omitted by the caller. Native graph validation remains the
premise for unsupported CRDTs, schema changes and generated inputs. -/
def clientSignedCreateCollection (collection : String) : Bool :=
  ["Node", "Agent", "AgentContext", "CompactionConfig", "Skill", "DatastoreToolSurface", "Tools", "AgentTarget", "InferenceBackendMetadata", "InferenceProfile", "InferenceSampling", "InferenceExecution", "InferenceRetryPolicy", "ToolServiceRegistry", "ProjectionAcpBinding", "Task", "Schedule", "EventSource", "Trigger", "Callback", "CallbackBinding", "CallbackModule", "RepositoryPlacement", "EvalDefinition"].contains collection

def clientSignedUpdateSupported (collection : String) : Bool :=
  clientSignedCreateCollection collection

/-- A client signs deletes only for bundled configuration schemas or the
pack installation, Goal and GoalCreationClaim records. Native delete permission
remains required; session deletion has a separate product owner. -/
def clientSignedDeleteCollection (collection : String) : Bool :=
  (clientSignedCreateCollection collection && collection != "InferenceBackend") ||
    ["PackInstallation", "InferenceBackendMetadata", "InferenceBackendObservation",
      "BackendCredential", "Goal", "GoalCreationClaim"].contains collection

theorem client_delete_includes_goal : clientSignedDeleteCollection "Goal" = true := by decide

theorem client_delete_includes_goal_claim : clientSignedDeleteCollection "GoalCreationClaim" = true := by decide

theorem client_delete_excludes_session : clientSignedDeleteCollection "AgentSession" = false := by decide

/-- The legacy inline-secret collection is never selected by the new deletion owner. -/
theorem client_delete_excludes_legacy_backend : clientSignedDeleteCollection "InferenceBackend" = false := by decide

/-- A matching physical observation and metadata are retired together. A
credential may be shared, so this operation retains it. Incoming relationships
can remain at the old physical ID; fresh identities must not reuse that ID. -/
def removeBackendRecords (authorized matching committed : Bool) : Bool × Bool × Bool :=
  if authorized && matching && committed then (false, false, true) else (true, true, true)

theorem backend_removal_keeps_credential (authorized matching committed : Bool) :
    (removeBackendRecords authorized matching committed).2.2 = true := by
  unfold removeBackendRecords
  split <;> rfl

theorem backend_removal_rolls_back (authorized matching : Bool) :
    removeBackendRecords authorized matching false = (true, true, true) := by
  simp [removeBackendRecords]

/-- Native document identity commits the initial contents. Independent random
creation nonces separate identical logical backends; observations commit their
metadata's physical identity. Native hashing and nonce entropy are premises. -/
def backendGenesis (logical credential nonce : String) : String × String × String :=
  (logical, credential, nonce)

theorem backend_genesis_separates_nonces (logical credential first second : String)
    (distinct : first ≠ second) :
    backendGenesis logical credential first ≠ backendGenesis logical credential second := by
  intro equal
  apply distinct
  exact congrArg (fun value => value.2.2) equal

theorem backend_observation_separates_parents (first second : String) (distinct : first ≠ second) :
    ("observation", first) ≠ ("observation", second) := by
  intro equal
  apply distinct
  exact congrArg Prod.snd equal

/-- A client-intent HTTP transaction cannot send an ordinary mutation through
server-owned signing. Reads and embedded writes retain their existing route. -/
def ordinaryMutationAllowed (clientIntent mutation : Bool) : Bool :=
  !clientIntent || !mutation

theorem ordinary_client_mutation_refused : ordinaryMutationAllowed true true = false := by decide

/-- A transaction without verified client graph handoff cannot author a client
Goal mutation under another actor. Native ownership and signing remain separate
premises even when the requested actor matches. -/
def clientGoalTransactionActorMatches (transactionActor : Option String) (requester : String) : Bool :=
  !requester.isEmpty && transactionActor == some requester

theorem client_goal_agent_cannot_impersonate_user :
    clientGoalTransactionActorMatches (some "agent") "user" = false := by decide

/-- A managed Agent may execute a User's retained native graphs only after
the signed command, its physical targets and the current native snapshot all
match. A replacement additionally requires the User's exact delete graph. The
graphs establish signatures, not ACP grants; the transaction still observes
native authorization for both actors. -/
def clientGoalGraphHandoffAllowed (managedActor commandBound createBound nativeAuthorized
    replacing deleteBound : Bool) : Bool :=
  managedActor && commandBound && createBound && nativeAuthorized && (!replacing || deleteBound)

theorem client_goal_replacement_requires_delete_graph
    (actor command create authorized : Bool) :
    clientGoalGraphHandoffAllowed actor command create authorized true false = false := by
  simp [clientGoalGraphHandoffAllowed]

theorem client_goal_graph_cannot_replace_native_authorization
    (actor command create replacing delete : Bool) :
    clientGoalGraphHandoffAllowed actor command create false replacing delete = false := by
  simp [clientGoalGraphHandoffAllowed]

/-- A signed HTTP operation is enabled only by an explicitly held private
key whose derived DID is the authenticated actor and bearer. Public knowledge
of another DID never supplies its signing capability. -/
def heldClientSignerAuthorizes (actor bearer signer : String) (hasPrivate : Bool) : Bool :=
  hasPrivate && !actor.isEmpty && actor == bearer && actor == signer

theorem absent_capability_refused (actor bearer signer : String) :
    heldClientSignerAuthorizes actor bearer signer false = false := by simp [heldClientSignerAuthorizes]

theorem mismatched_capability_refused :
    heldClientSignerAuthorizes "user-a" "user-a" "user-b" true = false := by decide

/-- Immutable values in a replacement must equal the observed values; equal
values are omitted from the signed mutable patch. -/
def immutableReplacementAllowed (observed proposed : String) : Bool := observed == proposed

theorem changed_immutable_is_rejected : immutableReplacementAllowed "before" "after" = false := by decide

/-- Once a client-signed write conversation starts, a transaction failure cannot silently
reconstruct and sign a new graph. The caller must explicitly initiate another
operation after observing the outcome. -/
def clientSignedWriteMayRetry (dispatched : Bool) : Bool := !dispatched

theorem dispatched_write_never_retries : clientSignedWriteMayRetry true = false := by decide

def packPresent (state : State) (record : String) : Bool :=
  state.objects.any fun object => object.resource == "configuration" && object.id == record

/-- The trusted publisher commits a fresh PackInstallation and its physical
Node parent in one native transaction. The authenticated installer is the
native owner; the Node identifier supplies inheritance, not signing authority.
Updating an existing record never restores a revoked relationship. Native direct
creates can bypass this publisher and do not implicitly install a parent. -/
def publishPackInstallation (state : State) (installer record node : String)
    (fresh commit : Bool) : State :=
  if fresh && commit && !(packPresent state record) then
    apply (directCreate state installer "configuration" record true true)
      (.grantAs installer ⟨"configuration", record, "parent", .object "nodes" node⟩)
  else state

theorem aborted_pack_publication_is_unchanged (state : State)
    (installer record node : String) (fresh : Bool) :
    publishPackInstallation state installer record node fresh false = state := by
  simp [publishPackInstallation]

theorem updating_pack_does_not_regrant (state : State)
    (installer record node : String) (commit : Bool) :
    publishPackInstallation state installer record node false commit = state := by
  simp [publishPackInstallation]

theorem existing_pack_is_never_regranted (state : State)
    (installer record node : String) (present : packPresent state record = true) :
    publishPackInstallation state installer record node true true = state := by
  simp [publishPackInstallation, present]

theorem direct_pack_create_does_not_implicitly_link_node (state : State)
    (installer record : String) (authenticated nodeCreate : Bool) :
    (directCreate state installer "configuration" record authenticated nodeCreate).relations =
      state.relations := by
  exact direct_create_has_no_implicit_relations state installer "configuration" record authenticated nodeCreate

/-- A pack removal is confirmed only when the native delete permission holds
and the exact requested physical document is returned as deleted. This assumes
an authenticated deletion transport; it does not grant a configuration writer
the Node administrator's delete authority. -/
def packRemovalConfirmed (state : State) (actor record returned : String) : Bool :=
  !record.isEmpty && record == returned &&
    Household.allowed 5 state actor "configuration" record "delete"

/-- Native visibility does not distinguish a missing managed document from one
hidden by ACP. Cleanup can discard its installation record only after each
created, still-managed document has been observed by the removing actor. -/
def packCleanupInputsVisible (visible : List Bool) : Bool := visible.all id

theorem hidden_pack_cleanup_input_blocks (tail : List Bool) :
    packCleanupInputsVisible (false :: tail) = false := by
  simp [packCleanupInputsVisible]

/-- A cached datastore tool may dispatch only while the same transaction sees
the source through the selected Agent's native ACP, observes its current
selection, and confirms that the enabled declaration still matches. A grant
through another source does not preserve a removed selection. -/
def cachedDatastoreToolUsable (visible enabled selected declarationMatches : Bool) : Bool :=
  visible && enabled && selected && declarationMatches

theorem revoked_surface_is_not_usable (enabled selected declarationMatches : Bool) :
    cachedDatastoreToolUsable false enabled selected declarationMatches = false := by
  simp [cachedDatastoreToolUsable]

theorem removed_surface_selection_is_not_usable (visible enabled declarationMatches : Bool) :
    cachedDatastoreToolUsable visible enabled false declarationMatches = false := by
  simp [cachedDatastoreToolUsable]

/-- A staged tool write becomes durable only after source validation, exact
mutation confirmation, and the native commit all succeed. -/
def cachedDatastoreWriteCommitted (sourceValid responseConfirmed commit : Bool) : Bool :=
  sourceValid && responseConfirmed && commit

theorem unconfirmed_datastore_write_rolls_back (sourceValid commit : Bool) :
    cachedDatastoreWriteCommitted sourceValid false commit = false := by
  simp [cachedDatastoreWriteCommitted]

/-- A UX page uses its authenticated operator and a retained verified pack
snapshot rather than an Agent selection. Manifest approval is an input from
that snapshot; the native transaction checks the named declaration's current
visibility, enabled state, and equality. The model does not fence changes to
the filesystem that holds the installed pack. -/
def cachedUxDatastoreToolUsable (visible enabled manifestAllows declarationMatches : Bool) : Bool :=
  visible && enabled && manifestAllows && declarationMatches

theorem revoked_ux_surface_is_not_usable (enabled manifestAllows declarationMatches : Bool) :
    cachedUxDatastoreToolUsable false enabled manifestAllows declarationMatches = false := by
  simp [cachedUxDatastoreToolUsable]

/-- A remote client may create an application document only through native
validation of the client-approved signed graph. An embedded runtime write
instead uses its explicitly held runtime or managed-Agent signer. -/
def uxCreateTransportSupported (remoteClient validatedClientGraph : Bool) : Bool :=
  !remoteClient || validatedClientGraph

theorem unsigned_remote_ux_create_is_refused : uxCreateTransportSupported true false = false := by
  decide

theorem missing_pack_delete_is_not_confirmed (state : State) (actor record : String) :
    packRemovalConfirmed state actor record "" = false := by
  by_cases empty : record = ""
  · have emptyString : ("" : String).isEmpty = true := by decide
    simp [packRemovalConfirmed, empty, emptyString]
  · simp [packRemovalConfirmed, empty]

theorem other_pack_delete_is_not_confirmed (state : State) (actor record returned : String)
    (different : record ≠ returned) :
    packRemovalConfirmed state actor record returned = false := by
  simp [packRemovalConfirmed, different]

def packFixture : State :=
  { objects := [⟨"nodes", "node", "user"⟩],
    relations := [⟨"nodes", "node", "runtime_reader", .actor "runtime"⟩], enrolled := [] }

theorem agent_pack_owner_and_node_user_control :
    let published := publishPackInstallation packFixture "agent" "pack" "node" true true
    [Household.allowed 5 published "agent" "configuration" "pack" "update",
     Household.allowed 5 published "user" "configuration" "pack" "delete",
     Household.allowed 5 published "runtime" "configuration" "pack" "read",
     Household.allowed 5 published "other" "configuration" "pack" "read"] =
      [true, true, true, false] := by native_decide

theorem revoked_pack_parent_is_not_restored_by_update :
    let published := publishPackInstallation packFixture "agent" "pack" "node" true true
    let revoked := apply published (.revoke ⟨"configuration", "pack", "parent", .object "nodes" "node"⟩)
    let updated := publishPackInstallation revoked "agent" "pack" "node" false true
    [Household.allowed 5 updated "agent" "configuration" "pack" "read",
     Household.allowed 5 updated "user" "configuration" "pack" "read"] = [true, false] := by native_decide

theorem signed_configuration_creates_are_limited :
    [clientSignedCreateCollection "AgentContext", clientSignedCreateCollection "Tools",
     clientSignedCreateCollection "Skill"] = [true, true, true] := by decide

/-- A newly published configuration document may be linked to its Node and
to the Agent that selects it. Both grants are made by its authenticated owner. -/
def publishSelectedConfiguration (state : State) (creator config node agent : String) : State :=
  let withParent := apply state (.grantAs creator
    ⟨"configuration", config, "parent", .object "nodes" node⟩)
  apply withParent (.grantAs creator ⟨"configuration", config, "reader", .actor agent⟩)

/-- This contract requires the native transaction to include creation and both
relationships; an aborted transaction publishes none of them. A lost commit
response remains unknown and cannot authorize a blind replay. -/
def publishSelectedAtomic (state : State) (creator config node agent : String) (commit : Bool) : State :=
  if commit then publishSelectedConfiguration
    (directCreate state creator "configuration" config true true) creator config node agent
  else state

theorem aborted_selected_configuration_is_unchanged (state : State) (creator config node agent : String) :
    (publishSelectedAtomic state creator config node agent false).objects = state.objects ∧
    (publishSelectedAtomic state creator config node agent false).relations = state.relations := by
  exact ⟨rfl, rfl⟩

/-- Runtime metadata binds a physical Node document to the active Node DID.
Its visibility is re-observed in the publication snapshot as the separately
authenticated runtime; the Agent remains the transaction actor. This check
does not by itself prove that concurrent replacement conflicts with commit. -/
def usableNodeBinding (selectedNode observedNode physicalId runtime : String)
    (visible : Bool) : Bool :=
  !selectedNode.isEmpty && selectedNode == observedNode && selectedNode == runtime &&
    !physicalId.isEmpty && visible

theorem wrong_or_missing_node_binding_is_refused :
    [usableNodeBinding "node" "other" "doc" "node" true,
     usableNodeBinding "node" "node" "" "node" true,
     usableNodeBinding "node" "node" "doc" "node" false] = [false, false, false] := by decide

/-- Each fresh provider-input assembly checks the selected Agent's native read
of the exact configuration documents retained by its resolved generation.
The requester is not rechecked here. A revocation after an input is assembled
or sent cannot recall that input; ACP and provider I/O are not atomic. -/
def providerConfigurationVisible (state : State) (agent : String)
    (documents : List (String × String)) : Bool :=
  documents.all fun (resource, id) => Household.allowed 5 state agent resource id "read"

def providerAssemblyAllowed (state : State) (agent _requester : String)
    (documents : List (String × String)) : Bool :=
  providerConfigurationVisible state agent documents

theorem provider_assembly_does_not_recheck_requester (state : State)
    (agent first second : String) (documents : List (String × String)) :
    providerAssemblyAllowed state agent first documents =
      providerAssemblyAllowed state agent second documents := by rfl

def providerFixture : State :=
  { objects := [⟨"agents", "agent-doc", "user"⟩,
      ⟨"configuration", "context-doc", "user"⟩,
      ⟨"configuration", "tools-doc", "user"⟩],
    relations := [⟨"agents", "agent-doc", "executor", .actor "agent"⟩,
      ⟨"configuration", "context-doc", "reader", .actor "agent"⟩,
      ⟨"configuration", "tools-doc", "reader", .actor "agent"⟩], enrolled := [] }

def providerDocuments : List (String × String) :=
  [("agents", "agent-doc"), ("configuration", "context-doc"), ("configuration", "tools-doc")]

theorem provider_assembly_requires_each_native_read :
    providerConfigurationVisible providerFixture "agent" providerDocuments = true ∧
    providerConfigurationVisible providerFixture "sibling" providerDocuments = false := by decide

theorem revoked_config_is_not_used_by_next_assembly :
    let revoked := apply providerFixture (.revoke ⟨"configuration", "tools-doc", "reader", .actor "agent"⟩)
    providerConfigurationVisible revoked "agent" providerDocuments = false := by decide

/-- Main and summary inference use physical documents from the resolved
generation. Every selected profile and optional sampling, execution, retry,
and compaction document must be visible to the executing Agent at each fresh
provider-input assembly. Backend credentials remain runtime-private and are
not included in this Agent-readable set. -/
def inferenceProviderFixture : State :=
  { objects := [⟨"agents", "agent-doc", "user"⟩, ⟨"configuration", "profile", "user"⟩,
      ⟨"configuration", "sampling", "user"⟩,
      ⟨"configuration", "execution", "user"⟩,
      ⟨"configuration", "retry", "user"⟩,
      ⟨"configuration", "compaction", "user"⟩,
      ⟨"configuration", "summary-profile", "user"⟩],
    relations := [⟨"agents", "agent-doc", "executor", .actor "agent"⟩,
      ⟨"configuration", "profile", "selected_by", .entitySet "agents" "agent-doc" "executor"⟩,
      ⟨"configuration", "sampling", "selected_by", .entitySet "agents" "agent-doc" "executor"⟩,
      ⟨"configuration", "execution", "selected_by", .entitySet "agents" "agent-doc" "executor"⟩,
      ⟨"configuration", "retry", "selected_by", .entitySet "agents" "agent-doc" "executor"⟩,
      ⟨"configuration", "compaction", "selected_by", .entitySet "agents" "agent-doc" "executor"⟩,
      ⟨"configuration", "summary-profile", "selected_by", .entitySet "agents" "agent-doc" "executor"⟩], enrolled := [] }

def inferenceProviderDocuments : List (String × String) :=
  ["profile", "sampling", "execution", "retry", "compaction", "summary-profile"].map
    fun id => ("configuration", id)

theorem selected_inference_is_visible_only_to_granted_agent :
    [providerConfigurationVisible inferenceProviderFixture "agent" inferenceProviderDocuments,
     providerConfigurationVisible inferenceProviderFixture "sibling" inferenceProviderDocuments] =
      [true, false] := by native_decide

theorem revoked_inference_selection_blocks_next_assembly :
    let revoked := apply inferenceProviderFixture
      (.revoke ⟨"configuration", "retry", "selected_by", .entitySet "agents" "agent-doc" "executor"⟩)
    providerConfigurationVisible revoked "agent" inferenceProviderDocuments = false := by native_decide

/-- A selected physical Agent contributes one relationship per physical
configuration document, regardless of how many paths from that Agent select
it. Only selection differences mutate that relationship: unchanged selections
must not recreate a manually revoked grant. A direct actor reader remains an
independent native grant. The commit premise covers the transaction and all
native relation-management checks, including an abort on a failed change. -/
def updateInferenceSelection (state : State) (manager agentDoc : String)
    (before after : List String) (committed : Bool) : State :=
  if committed then
    let removed := before.foldl (fun state id =>
      if after.contains id then state else
        apply state (.revokeAs manager
          ⟨"configuration", id, "selected_by", .entitySet "agents" agentDoc "executor"⟩)) state
    after.foldl (fun state id =>
      if before.contains id then state else
        apply state (.grantAs manager
          ⟨"configuration", id, "selected_by", .entitySet "agents" agentDoc "executor"⟩)) removed
  else state

def inferenceSelectionFixture : State :=
  { objects := [⟨"agents", "agent-doc", "user"⟩,
      ⟨"agents", "sibling-doc", "user"⟩, ⟨"configuration", "profile", "user"⟩],
    relations := [⟨"agents", "agent-doc", "executor", .actor "agent"⟩,
      ⟨"agents", "sibling-doc", "executor", .actor "sibling"⟩], enrolled := [] }

theorem inference_selection_scopes_shared_profile_to_each_agent :
    let selected := updateInferenceSelection inferenceSelectionFixture "user" "agent-doc" [] ["profile"] true
    [Household.allowed 5 selected "agent" "configuration" "profile" "read",
     Household.allowed 5 selected "sibling" "configuration" "profile" "read"] = [true, false] := by native_decide

theorem unchanged_selection_does_not_restore_revoked_relation :
    updateInferenceSelection inferenceSelectionFixture "user" "agent-doc" ["profile"] ["profile"] true =
      inferenceSelectionFixture := by rfl

theorem inference_selection_preserves_independent_reader :
    let selected := updateInferenceSelection inferenceSelectionFixture "user" "agent-doc" [] ["profile"] true
    let direct := apply selected (.grantAs "user" ⟨"configuration", "profile", "reader", .actor "agent"⟩)
    let removed := updateInferenceSelection direct "user" "agent-doc" ["profile"] [] true
    Household.allowed 5 removed "agent" "configuration" "profile" "read" = true := by native_decide

theorem inference_selection_aborted (state : State) (manager agentDoc : String) (before after : List String) :
    updateInferenceSelection state manager agentDoc before after false = state := by rfl

/-- A computed permission used to propagate a selection also contains the
native parent document's owner. It cannot represent Agent executors alone. -/
def computedSelectionCandidate : List ResourceDefinition :=
  [{ name := "configuration",
     relations := [⟨"selected_parent", ["configuration"], []⟩],
     permissions := [permission "selected_executor" [] [],
       permission "read" [] [("selected_parent", "selected_executor")]] }]

def computedSelectionCounterexample : State :=
  { objects := [⟨"configuration", "parent", "parent-owner"⟩,
      ⟨"configuration", "child", "child-owner"⟩],
    relations := [⟨"configuration", "child", "selected_parent", .object "configuration" "parent"⟩],
    enrolled := [] }

theorem computed_selection_also_grants_parent_owner :
    allowedIn computedSelectionCandidate 5 computedSelectionCounterexample
      "parent-owner" "configuration" "child" "read" = true := by native_decide

/-- A direct entity-set edge evaluates the named relation, without the
automatic owner grant attached to a computed permission. Profiles and their
dependencies can therefore share a chain while retaining each Agent's own
executor relation. Fuel bounds this executable model; the native engine's
cycle and evaluation budget remains the external termination premise. -/
def inferenceUsersetFixture : State :=
  { objects := [⟨"agents", "agent-doc", "agent-owner"⟩,
      ⟨"agents", "sibling-doc", "agent-owner"⟩,
      ⟨"configuration", "profile", "profile-owner"⟩,
      ⟨"configuration", "backend-metadata", "backend-owner"⟩,
      ⟨"configuration", "sampling", "sampling-owner"⟩,
      ⟨"configuration", "execution", "execution-owner"⟩,
      ⟨"configuration", "retry", "retry-owner"⟩],
    relations := [⟨"agents", "agent-doc", "executor", .actor "agent"⟩,
      ⟨"agents", "sibling-doc", "executor", .actor "sibling"⟩,
      ⟨"configuration", "profile", "selected_by", .entitySet "agents" "agent-doc" "executor"⟩,
      ⟨"configuration", "backend-metadata", "selected_by", .entitySet "configuration" "profile" "selected_by"⟩,
      ⟨"configuration", "sampling", "selected_by", .entitySet "configuration" "profile" "selected_by"⟩,
      ⟨"configuration", "execution", "selected_by", .entitySet "configuration" "profile" "selected_by"⟩,
      ⟨"configuration", "retry", "selected_by", .entitySet "configuration" "execution" "selected_by"⟩],
    enrolled := [] }

/-- An explicit User transaction grants the selected physical inference
documents separately. Native relation management is needed to detach their
selection edges. These grants persist until explicitly revoked; a later
selection or reconciliation neither removes nor recreates them. -/
def grantInferenceSelfConfiguration (state : State) (agent : String)
    (documents : List String) (authorized committed : Bool) : State :=
  if authorized && committed then
    documents.foldl (fun state id =>
      apply (apply state (.grant ⟨"configuration", id, "writer", .actor agent⟩))
        (.grant ⟨"configuration", id, "selection_manager", .actor agent⟩)) state
  else state

def inferenceSelfConfigFixture : State :=
  grantInferenceSelfConfiguration inferenceUsersetFixture "agent"
    ["profile", "sampling", "execution", "retry"] true true

theorem inference_self_config_is_explicit_and_scoped :
    [allowed 8 inferenceUsersetFixture "agent" "configuration" "execution" "update",
     allowed 8 inferenceSelfConfigFixture "agent" "configuration" "execution" "update",
     allowed 8 inferenceSelfConfigFixture "sibling" "configuration" "execution" "update",
     allowed 8 inferenceSelfConfigFixture "agent" "configuration" "backend-metadata" "update"] =
      [false, true, false, false] := by native_decide

theorem revoked_inference_writer_is_not_recreated :
    let revoked := apply inferenceSelfConfigFixture
      (.revoke ⟨"configuration", "execution", "writer", .actor "agent"⟩)
    allowed 8 revoked "agent" "configuration" "execution" "update" = false := by native_decide

theorem selected_inference_manager_can_rebind_its_sources :
    let edge : Relation := ⟨"configuration", "execution", "selected_by",
      .entitySet "configuration" "profile" "selected_by"⟩
    [mayManage inferenceUsersetFixture "agent" edge,
     mayManage inferenceSelfConfigFixture "agent" edge,
     mayManage inferenceSelfConfigFixture "sibling" edge] = [false, true, false] := by native_decide

theorem failed_inference_grant_is_atomic (state : State) (agent : String)
    (documents : List String) (authorized : Bool) :
    grantInferenceSelfConfiguration state agent documents authorized false = state := by
  simp [grantInferenceSelfConfiguration]

/-- Backend observations are independently writable by the runtime. The
runtime's observation grant must not allow it to change the User-owned
provider, endpoint, or credential reference. Selected Agents can read both. -/
def backendObservationFixture : State :=
  { inferenceUsersetFixture with
    objects := ⟨"configuration", "backend-observation", "backend-owner"⟩ :: inferenceUsersetFixture.objects,
    relations := [⟨"configuration", "backend-observation", "writer", .actor "runtime"⟩,
      ⟨"configuration", "backend-observation", "selected_by", .entitySet "configuration" "backend-metadata" "selected_by"⟩]
      ++ inferenceUsersetFixture.relations }

theorem backend_observation_does_not_authorize_runtime_configuration :
    [allowed 10 backendObservationFixture "runtime" "configuration" "backend-metadata" "update",
     allowed 10 backendObservationFixture "runtime" "configuration" "backend-observation" "update",
     allowed 10 backendObservationFixture "backend-owner" "configuration" "backend-metadata" "update",
     allowed 10 backendObservationFixture "agent" "configuration" "backend-observation" "read",
     allowed 10 backendObservationFixture "sibling" "configuration" "backend-observation" "read"] =
      [false, true, true, true, false] := by native_decide

theorem nested_selection_does_not_grant_parent_owner_or_sibling :
    [allowed 8 inferenceUsersetFixture "agent" "configuration" "sampling" "read",
     allowed 8 inferenceUsersetFixture "sibling" "configuration" "sampling" "read",
     allowed 8 inferenceUsersetFixture "profile-owner" "configuration" "sampling" "read"] =
      [true, false, false] := by native_decide

theorem revoked_userset_edge_blocks_only_that_source :
    let withDirect := apply inferenceUsersetFixture
      (.grant ⟨"configuration", "sampling", "reader", .actor "agent"⟩)
    let revoked := apply withDirect (.revoke
      ⟨"configuration", "sampling", "selected_by", .entitySet "configuration" "profile" "selected_by"⟩)
    [allowed 8 revoked "agent" "configuration" "sampling" "read",
     allowed 8 revoked "profile-owner" "configuration" "sampling" "read"] = [true, false] := by native_decide

theorem revoked_executor_removes_nested_access :
    let revoked := apply inferenceUsersetFixture
      (.revoke ⟨"agents", "agent-doc", "executor", .actor "agent"⟩)
    allowed 8 revoked "agent" "configuration" "sampling" "read" = false := by native_decide

/-- Only a separately protected metadata document participates in the profile
selection graph. Inline credentials and the runtime's credential relationship
remain outside this graph. A profile reader never gains metadata update. -/
theorem selected_profile_grants_only_metadata_read :
    [allowed 8 inferenceUsersetFixture "agent" "configuration" "backend-metadata" "read",
     allowed 8 inferenceUsersetFixture "agent" "configuration" "backend-metadata" "update",
     allowed 8 inferenceUsersetFixture "sibling" "configuration" "backend-metadata" "read"] =
      [true, false, false] := by native_decide

theorem revoked_backend_selection_preserves_direct_reader :
    let direct := apply inferenceUsersetFixture
      (.grant ⟨"configuration", "backend-metadata", "reader", .actor "agent"⟩)
    let revoked := apply direct (.revoke
      ⟨"configuration", "backend-metadata", "selected_by", .entitySet "configuration" "profile" "selected_by"⟩)
    allowed 8 revoked "agent" "configuration" "backend-metadata" "read" = true := by native_decide

theorem revoked_backend_selection_blocks_inherited_reader :
    let revoked := apply inferenceUsersetFixture (.revoke
      ⟨"configuration", "backend-metadata", "selected_by", .entitySet "configuration" "profile" "selected_by"⟩)
    allowed 8 revoked "agent" "configuration" "backend-metadata" "read" = false := by native_decide

theorem retry_follows_execution_and_profile_selection :
    [allowed 8 inferenceUsersetFixture "agent" "configuration" "retry" "read",
     allowed 8 inferenceUsersetFixture "sibling" "configuration" "retry" "read",
     allowed 8 inferenceUsersetFixture "profile-owner" "configuration" "retry" "read"] =
      [true, false, false] := by native_decide

theorem revoked_execution_edge_blocks_retry :
    let revoked := apply inferenceUsersetFixture (.revoke
      ⟨"configuration", "execution", "selected_by", .entitySet "configuration" "profile" "selected_by"⟩)
    allowed 8 revoked "agent" "configuration" "retry" "read" = false := by native_decide

/-- Every plugin model request uses the immediate caller's native read of the
exact profile selected for that plugin invocation. A missing caller cannot
borrow the runtime identity. A runtime-private backend handle is only resolved
after that read; its separate credential boundary is outside this predicate.
Revocation after provider I/O begins cannot recall an in-flight request. -/
def pluginModelMayUse (state : State) (actor : Option String) (profile : String) : Bool :=
  match actor with
  | none => false
  | some actor => allowed 8 state actor "configuration" profile "read"

theorem plugin_model_requires_immediate_agent :
    [pluginModelMayUse inferenceUsersetFixture (some "agent") "profile",
     pluginModelMayUse inferenceUsersetFixture (some "sibling") "profile",
     pluginModelMayUse inferenceUsersetFixture none "profile"] = [true, false, false] := by native_decide

theorem revoked_plugin_profile_blocks_next_request :
    let revoked := apply inferenceUsersetFixture (.revoke
      ⟨"configuration", "profile", "selected_by", .entitySet "agents" "agent-doc" "executor"⟩)
    pluginModelMayUse revoked (some "agent") "profile" = false := by native_decide

def summaryUsersetFixture : State :=
  { objects := [⟨"agents", "agent", "user"⟩, ⟨"configuration", "context", "user"⟩,
      ⟨"configuration", "compaction", "user"⟩, ⟨"configuration", "summary-profile", "user"⟩],
    relations := [⟨"agents", "agent", "executor", .actor "selected"⟩,
      ⟨"configuration", "context", "selected_by", .entitySet "agents" "agent" "executor"⟩,
      ⟨"configuration", "compaction", "selected_by", .entitySet "configuration" "context" "selected_by"⟩,
      ⟨"configuration", "summary-profile", "selected_by", .entitySet "configuration" "compaction" "selected_by"⟩],
    enrolled := [] }

theorem summary_inference_follows_compaction_selection :
    [allowed 8 summaryUsersetFixture "selected" "configuration" "summary-profile" "read",
     allowed 8 summaryUsersetFixture "sibling" "configuration" "summary-profile" "read"] =
      [true, false] := by native_decide

/-- A selection update changes only physical source documents that enter or
leave the set. Native relation-management authorization and the document
change must commit together; a failed grant or revocation aborts the update.
The two paths selecting the same source produce only one relationship.
This is a publication-owner contract, not a native constraint on arbitrary
GraphQL updates: a direct document writer can bypass source-edge maintenance.
An explicit native relationship revocation still takes effect independently. -/
def updateUsersetSelection (state : State) (manager targetResource targetId targetRelation : String)
    (before after : List String) (committed : Bool) : State :=
  let removed := before.eraseDups.filter fun id => !after.contains id
  let added := after.eraseDups.filter fun id => !before.contains id
  let edge := fun id => ⟨"configuration", id, "selected_by",
    .entitySet targetResource targetId targetRelation⟩
  if committed && (removed ++ added).all (fun id => mayManage state manager (edge id)) then
    let reduced := removed.foldl (fun state id => apply state (.revokeAs manager (edge id))) state
    added.foldl (fun state id => apply state (.grantAs manager (edge id))) reduced
  else state

theorem aborted_userset_selection_is_unchanged (state : State)
    (manager targetResource targetId targetRelation : String) (before after : List String) :
    updateUsersetSelection state manager targetResource targetId targetRelation before after false = state := by
  simp [updateUsersetSelection]

theorem unchanged_userset_selection_preserves_revocation :
    updateUsersetSelection inferenceUsersetFixture "sampling-owner" "configuration" "profile" "selected_by"
      ["sampling"] ["sampling"] true = inferenceUsersetFixture := by rfl

/-- A Skill's instructions may enter a fresh provider input only while the
executing Agent can read the exact Skill selected by its exact AgentContext.
The source edge is separate from a direct reader grant. -/
def skillSelectionFixture : State :=
  { objects := [⟨"agents", "agent", "user"⟩,
      ⟨"configuration", "context", "user"⟩,
      ⟨"configuration", "skill", "user"⟩],
    relations := [⟨"agents", "agent", "executor", .actor "selected"⟩,
      ⟨"configuration", "context", "selected_by", .entitySet "agents" "agent" "executor"⟩,
      ⟨"configuration", "skill", "selected_by", .entitySet "configuration" "context" "selected_by"⟩],
    enrolled := [] }

theorem skill_selected_agent_only :
    [allowed 8 skillSelectionFixture "selected" "configuration" "skill" "read",
     allowed 8 skillSelectionFixture "sibling" "configuration" "skill" "read"] = [true, false] := by native_decide

/-- Compaction uses the existing context selection userset. A distinct direct
reader remains independent when a context selects a different compaction. -/
def compactionSelectionFixture : State :=
  { skillSelectionFixture with
    objects := [⟨"agents", "agent", "user"⟩, ⟨"configuration", "context", "user"⟩,
      ⟨"configuration", "compaction-a", "user"⟩, ⟨"configuration", "compaction-b", "user"⟩],
    relations := [⟨"agents", "agent", "executor", .actor "selected"⟩,
      ⟨"configuration", "context", "selected_by", .entitySet "agents" "agent" "executor"⟩,
      ⟨"configuration", "compaction-a", "selected_by", .entitySet "configuration" "context" "selected_by"⟩] }

theorem compaction_selected_agent_only :
    [allowed 8 compactionSelectionFixture "selected" "configuration" "compaction-a" "read",
     allowed 8 compactionSelectionFixture "sibling" "configuration" "compaction-a" "read",
     allowed 8 compactionSelectionFixture "selected" "configuration" "compaction-b" "read"] =
      [true, false, false] := by native_decide

theorem compaction_context_revocation_stops_inherited_read :
    let revoked := apply compactionSelectionFixture (.revoke
      ⟨"configuration", "context", "selected_by", .entitySet "agents" "agent" "executor"⟩)
    allowed 8 revoked "selected" "configuration" "compaction-a" "read" = false := by native_decide

/-- A context's Tools is a shared native source, selected through the same
physical Context-to-Agent edge as the Skills. A new Agent selecting an
existing context does not need to grant a direct reader on its Tools. -/
def toolsSelectionFixture : State :=
  { skillSelectionFixture with
    objects := ⟨"configuration", "tools", "user"⟩ :: skillSelectionFixture.objects
    relations := ⟨"configuration", "tools", "selected_by",
      .entitySet "configuration" "context" "selected_by"⟩ :: skillSelectionFixture.relations }

theorem context_tools_are_visible_to_selected_agent_only :
    [allowed 8 toolsSelectionFixture "selected" "configuration" "tools" "read",
     allowed 8 toolsSelectionFixture "sibling" "configuration" "tools" "read"] = [true, false] := by native_decide

theorem removing_agent_context_selection_revokes_transitive_tools_and_skill :
    let removed := apply toolsSelectionFixture (.revoke
      ⟨"configuration", "context", "selected_by", .entitySet "agents" "agent" "executor"⟩)
    [allowed 8 removed "selected" "configuration" "tools" "read",
     allowed 8 removed "selected" "configuration" "skill" "read"] = [false, false] := by native_decide

/-- Existing Tools validation retains all referenced AgentTargets, services,
and datastore surfaces, including AgentTargets while agent tools are disabled.
These sets identify source-edge differences, not an authorization decision. -/
structure ToolSelectionReferences where
  agentsEnabled : Bool
  targets : List String
  services : List String
  surfaces : List String
  deriving Repr

def toolSelectionReferences (references : ToolSelectionReferences) : List (String × List String) :=
  [("AgentTarget", references.targets.eraseDups),
   ("ToolServiceRegistry", references.services.eraseDups),
   ("DatastoreToolSurface", references.surfaces.eraseDups)]

theorem disabled_agent_tools_still_retain_target_edges :
    toolSelectionReferences ⟨false, ["target"], [], []⟩ =
      [("AgentTarget", ["target"]), ("ToolServiceRegistry", []), ("DatastoreToolSurface", [])] := by decide

/-- Cross-owner selection needs the source owner's explicit management grant.
It authorizes only the native selection relationship, not source updates or
direct reader/writer grants. Reading the source remains a separate native
permission, such as an explicitly granted reader or a Node parent grant. -/
def delegatedSelectionFixture : State :=
  apply skillSelectionFixture (.grantAs "user"
    ⟨"configuration", "skill", "selection_manager", .actor "delegate"⟩)

theorem selection_manager_is_narrow :
    [mayManage delegatedSelectionFixture "delegate"
       ⟨"configuration", "skill", "selected_by", .entitySet "configuration" "context" "selected_by"⟩,
     mayManage delegatedSelectionFixture "delegate"
       ⟨"configuration", "skill", "reader", .actor "delegate"⟩,
     mayManage delegatedSelectionFixture "delegate"
       ⟨"configuration", "skill", "writer", .actor "delegate"⟩,
     allowed 8 delegatedSelectionFixture "delegate" "configuration" "skill" "read",
     allowed 8 delegatedSelectionFixture "delegate" "configuration" "skill" "update"] =
      [true, false, false, false, false] := by native_decide

theorem revoking_selection_manager_removes_management :
    let revoked := apply delegatedSelectionFixture (.revoke
      ⟨"configuration", "skill", "selection_manager", .actor "delegate"⟩)
    mayManage revoked "delegate"
      ⟨"configuration", "skill", "selected_by", .entitySet "configuration" "context" "selected_by"⟩ = false := by native_decide

/-- A selection manager can share the source with any entity set accepted by
the native policy, including another Agent's executor. The owner must grant
that authority deliberately; the relation is not an automatic Agent grant. -/
def delegatedArbitraryShareFixture : State :=
  { delegatedSelectionFixture with
    objects := ⟨"agents", "other-agent", "other-owner"⟩ :: delegatedSelectionFixture.objects
    relations := ⟨"agents", "other-agent", "executor", .actor "other"⟩ :: delegatedSelectionFixture.relations }

theorem selection_manager_can_share_with_other_agent :
    let shared := apply delegatedArbitraryShareFixture (.grantAs "delegate"
      ⟨"configuration", "skill", "selected_by", .entitySet "agents" "other-agent" "executor"⟩)
    allowed 8 shared "other" "configuration" "skill" "read" = true := by native_decide

theorem revoked_skill_edge_blocks_next_provider_input :
    let revoked := apply skillSelectionFixture (.revoke
      ⟨"configuration", "skill", "selected_by", .entitySet "configuration" "context" "selected_by"⟩)
    allowed 8 revoked "selected" "configuration" "skill" "read" = false := by native_decide

/-- A cached Skill body is also checked when the tool returns it: a grant
revoked after provider-input assembly cannot recall that input, but it blocks
a later tool result that has not yet been assembled. -/
def skillToolMayReturn (state : State) (agent skill : String) : Bool :=
  allowed 8 state agent "configuration" skill "read"

def skillCatalogMayReturn (state : State) (agent : String) (skills : List String) : Bool :=
  skills.all (skillToolMayReturn state agent)

theorem revoked_skill_blocks_fresh_tool_result :
    let revoked := apply skillSelectionFixture (.revoke
      ⟨"configuration", "skill", "selected_by", .entitySet "configuration" "context" "selected_by"⟩)
    skillToolMayReturn revoked "selected" "skill" = false := by native_decide

theorem revoked_skill_blocks_fresh_catalog :
    let revoked := apply skillSelectionFixture (.revoke
      ⟨"configuration", "skill", "selected_by", .entitySet "configuration" "context" "selected_by"⟩)
    skillCatalogMayReturn revoked "selected" ["skill"] = false := by native_decide

/-- Each configuration parent is installed by the authenticated creator, and
selection must explicitly grant each Agent reader and any intended writer. A
writer cannot manage relations. Native creator ownership persists independently
of revocable relations, so a user-owned document must be user-created. -/
def grantsFixture : State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"configuration", "config", "user"⟩],
    relations := [⟨"configuration", "config", "parent", .object "nodes" "node"⟩,
      ⟨"configuration", "config", "reader", .actor "agent"⟩,
      ⟨"configuration", "config", "writer", .actor "writer"⟩], enrolled := [] }

theorem unrelated_actor_cannot_publish_selected_configuration :
    (publishSelectedConfiguration grantsFixture "other" "config" "node" "other").relations.length =
      grantsFixture.relations.length := by
  native_decide

theorem explicit_configuration_grants_limit_access :
    [Household.allowed 5 grantsFixture "user" "configuration" "config" "delete",
     Household.allowed 5 grantsFixture "agent" "configuration" "config" "read",
     Household.allowed 5 grantsFixture "agent" "configuration" "config" "update",
     Household.allowed 5 grantsFixture "writer" "configuration" "config" "read",
     Household.allowed 5 grantsFixture "writer" "configuration" "config" "update",
     Household.allowed 5 grantsFixture "writer" "configuration" "config" "delete",
     Household.allowed 5 grantsFixture "other" "configuration" "config" "read"] =
    [true, true, false, true, true, false, false] := by native_decide

theorem revocation_removes_configuration_reader :
    let revoked := apply grantsFixture
      (.revoke ⟨"configuration", "config", "reader", .actor "agent"⟩)
    Household.allowed 5 revoked "agent" "configuration" "config" "read" = false := by native_decide

theorem node_administrator_inherits_configuration_access :
    let delegated := apply grantsFixture
      (.grantAs "user" ⟨"nodes", "node", "administrator", .actor "admin"⟩)
    [Household.allowed 5 delegated "admin" "configuration" "config" "read",
     Household.allowed 5 delegated "admin" "configuration" "config" "update",
     Household.allowed 5 delegated "admin" "configuration" "config" "delete"] =
    [true, true, true] := by native_decide

/-- Projection ACP bindings describe which already-existing native policies a
Node uses. They are Node configuration, not authority for their selected Agent:
the configuration creator retains ownership, and the Node owner administers
the binding through its physical parent. Merely naming an Agent grants nothing. -/
def projectionBindingFixture : State :=
  { objects := [⟨"nodes", "node", "user"⟩,
      ⟨"configuration", "binding", "creator"⟩],
    relations := [⟨"configuration", "binding", "parent", .object "nodes" "node"⟩,
      ⟨"nodes", "node", "runtime_reader", .actor "runtime"⟩], enrolled := [] }

theorem projection_binding_respects_node_and_creator_ownership :
    [Household.allowed 5 projectionBindingFixture "creator" "configuration" "binding" "update",
     Household.allowed 5 projectionBindingFixture "user" "configuration" "binding" "update",
     Household.allowed 5 projectionBindingFixture "runtime" "configuration" "binding" "read",
     Household.allowed 5 projectionBindingFixture "runtime" "configuration" "binding" "update",
     Household.allowed 5 projectionBindingFixture "agent" "configuration" "binding" "read",
     Household.allowed 5 projectionBindingFixture "outsider" "configuration" "binding" "read"] =
    [true, true, true, false, false, false] := by native_decide

theorem projection_binding_revoked_parent_loses_inherited_control :
    let revoked := apply projectionBindingFixture
      (.revoke ⟨"configuration", "binding", "parent", .object "nodes" "node"⟩)
    [Household.allowed 5 revoked "user" "configuration" "binding" "read",
     Household.allowed 5 revoked "runtime" "configuration" "binding" "read",
     Household.allowed 5 revoked "creator" "configuration" "binding" "update"] =
    [false, false, true] := by native_decide

/-- An Agent-authored data-plane pairing overlay remains owned by that Agent.
The user administers it through the Node parent, and the runtime reads it via
the Node's explicit runtime-reader grant. An Agent's sibling has no access. -/
def agentPairingFixture : State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"configuration", "overlay", "agent"⟩],
    relations := [⟨"nodes", "node", "runtime_reader", .actor "runtime"⟩,
      ⟨"configuration", "overlay", "parent", .object "nodes" "node"⟩], enrolled := [] }

theorem agent_pairing_overlay_preserves_creator_isolation :
    [Household.allowed 5 agentPairingFixture "agent" "configuration" "overlay" "update",
     Household.allowed 5 agentPairingFixture "user" "configuration" "overlay" "update",
     Household.allowed 5 agentPairingFixture "runtime" "configuration" "overlay" "read",
     Household.allowed 5 agentPairingFixture "runtime" "configuration" "overlay" "update",
     Household.allowed 5 agentPairingFixture "sibling" "configuration" "overlay" "read"] =
    [true, true, true, false, false] := by native_decide

theorem revoked_pairing_parent_is_not_implicit :
    let state := apply agentPairingFixture
      (.revoke ⟨"configuration", "overlay", "parent", .object "nodes" "node"⟩)
    [Household.allowed 5 state "agent" "configuration" "overlay" "read",
     Household.allowed 5 state "user" "configuration" "overlay" "read",
     Household.allowed 5 state "runtime" "configuration" "overlay" "read"] =
    [true, false, false] := by native_decide

/-- An Engineer that creates an Agent and its configuration remains each
native document's owner. The Node owner administers both through their Node
parents. Linking a child does not grant the Engineer access to the Node itself. -/
def engineerCreatedFixture : State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"agents", "agent", "engineer"⟩,
      ⟨"configuration", "config", "engineer"⟩],
    relations := [⟨"agents", "agent", "parent", .object "nodes" "node"⟩,
      ⟨"configuration", "config", "parent", .object "nodes" "node"⟩], enrolled := [] }

theorem engineer_creator_and_node_owner_administer_created_documents :
    [Household.allowed 5 engineerCreatedFixture "engineer" "agents" "agent" "update",
     Household.allowed 5 engineerCreatedFixture "engineer" "configuration" "config" "update",
     Household.allowed 5 engineerCreatedFixture "user" "agents" "agent" "update",
     Household.allowed 5 engineerCreatedFixture "user" "configuration" "config" "update",
     Household.allowed 5 engineerCreatedFixture "user" "agents" "agent" "delete",
     Household.allowed 5 engineerCreatedFixture "user" "configuration" "config" "delete",
     Household.allowed 5 engineerCreatedFixture "engineer" "nodes" "node" "read"] =
    [true, true, true, true, true, true, false] := by native_decide

/-- Secret documents follow the same user-owned Node, but do not expose a
reader relation to selected Agents. A separately granted runtime writer can
refresh credentials and observations without gaining relation management. -/
def secretFixture : State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"secret_configuration", "secret", "user"⟩],
    relations := [⟨"secret_configuration", "secret", "parent", .object "nodes" "node"⟩,
      ⟨"secret_configuration", "secret", "runtime_writer", .actor "runtime"⟩], enrolled := [] }

theorem secret_runtime_writer_is_narrow :
    [Household.allowed 5 secretFixture "runtime" "secret_configuration" "secret" "read",
     Household.allowed 5 secretFixture "runtime" "secret_configuration" "secret" "update",
     Household.allowed 5 secretFixture "runtime" "secret_configuration" "secret" "delete",
     Household.allowed 5 secretFixture "agent" "secret_configuration" "secret" "read"] =
    [true, true, false, false] := by native_decide

/-- The owning user retains native control of its secret document; an Agent
does not acquire secret access from an inference selection. -/
theorem user_owns_secret_and_agent_cannot_read :
    [Household.allowed 5 secretFixture "user" "secret_configuration" "secret" "read",
     Household.allowed 5 secretFixture "user" "secret_configuration" "secret" "update",
     Household.allowed 5 secretFixture "agent" "secret_configuration" "secret" "read"] =
    [true, true, false] := by native_decide

/-- A selected Agent reads secret-free metadata through the ordinary native
configuration reader relation. The separately stored secret has only the
owning Node and runtime-writer links; this fixture grants the Agent no Node
reader, administrator, or credential ownership. This fixture assumes the
metadata has never contained a credential in its accessible block history. -/
def backendCredentialFixture : State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"configuration", "backend", "user"⟩,
      ⟨"secret_configuration", "credential", "user"⟩],
    relations := [⟨"configuration", "backend", "parent", .object "nodes" "node"⟩,
      ⟨"configuration", "backend", "reader", .actor "agent"⟩,
      ⟨"secret_configuration", "credential", "parent", .object "nodes" "node"⟩,
      ⟨"secret_configuration", "credential", "runtime_writer", .actor "runtime"⟩],
    enrolled := [] }

def pluginBackendFixture : State :=
  { backendCredentialFixture with
    objects := ⟨"configuration", "profile", "user"⟩ :: backendCredentialFixture.objects,
    relations := ⟨"configuration", "profile", "reader", .actor "agent"⟩ :: backendCredentialFixture.relations }

/-- Both documents are observed as the immediate caller before runtime secret
resolution; an absent caller cannot borrow runtime authority. -/
def pluginBackendMayUse (state : State) (actor : Option String) : Bool :=
  match actor with
  | none => false
  | some actor => Household.allowed 5 state actor "configuration" "profile" "read" &&
      Household.allowed 5 state actor "configuration" "backend" "read"

/-- Authorized provider transport necessarily discloses the credential to
the configured provider. Native policy prevents clients and Agents from
reading the credential record; provider response content is an external trust
premise rather than an authorization signal. -/
def providerCredentialDisclosureIsExternal : Bool := true

/-- Provider I/O may finish after authorization changes. Publication requires
the same selected backend, connection and native actor update authority in its write transaction. -/
def backendDiscoveryMayPublish (actorAuthorized sameDocument sameConnection committed : Bool) : Bool :=
  actorAuthorized && sameDocument && sameConnection && committed

/-- A completed probe cannot publish to a replacement document or connection. -/
def backendProbeMayPublish (sameDocument sameConnection committed : Bool) : Bool :=
  sameDocument && sameConnection && committed

theorem plugin_backend_requires_both_actor_reads :
    [pluginBackendMayUse pluginBackendFixture (some "agent"),
     pluginBackendMayUse pluginBackendFixture (some "sibling"),
     pluginBackendMayUse pluginBackendFixture none] = [true, false, false] := by native_decide

theorem revoked_backend_blocks_next_provider_assembly :
    let revoked := apply pluginBackendFixture
      (.revoke ⟨"configuration", "backend", "reader", .actor "agent"⟩)
    providerConfigurationVisible revoked "agent"
      [("configuration", "profile"), ("configuration", "backend")] = false := by native_decide

/-- A selected Agent can read the Backend but not the runtime credential. -/
theorem selected_agent_reads_metadata_without_secret :
    [Household.allowed 5 backendCredentialFixture "agent" "configuration" "backend" "read",
     Household.allowed 5 backendCredentialFixture "agent" "configuration" "backend" "update",
     Household.allowed 5 backendCredentialFixture "agent" "secret_configuration" "credential" "read",
     Household.allowed 5 backendCredentialFixture "runtime" "secret_configuration" "credential" "read",
     Household.allowed 5 backendCredentialFixture "user" "secret_configuration" "credential" "update"] =
    [true, false, false, true, true] := by native_decide

theorem revoked_runtime_writer_loses_secret_access :
    let revoked := apply secretFixture
      (.revoke ⟨"secret_configuration", "secret", "runtime_writer", .actor "runtime"⟩)
    [Household.allowed 5 revoked "runtime" "secret_configuration" "secret" "read",
     Household.allowed 5 revoked "runtime" "secret_configuration" "secret" "update"] =
    [false, false] := by native_decide

/-- Backend metadata and its credential are separate native documents. The
metadata may be shared only if neither its current state nor its accessible
history contains a raw secret. Clearing a field is not a proof that earlier
signed CRDT blocks no longer contain it. Native ACP and history visibility
are external premises of this decision. -/
def backendMetadataShareable (currentSecret historicalSecret : Bool) : Bool :=
  !currentSecret && !historicalSecret

/-- The legacy ungoverned lineage is safe for Agent admission only after the
native store has durably fenced history, writes, recreation, and replication. -/
def legacyBackendAdmissionAllowed (nativeLockdownProven : Bool) : Bool :=
  nativeLockdownProven

theorem unavailable_legacy_lockdown_denies_admission :
    legacyBackendAdmissionAllowed false = false := rfl

/-- A separate fresh-store admission requires a witness made before the store
was opened, and a legacy collection installed with its private policy from
birth. The native collection binding and historical-block read checks remain
external premises; a missing schema or an empty filtered query is not a fresh
store witness. -/
def freshBackendAdmissionAllowed (freshOrigin privateBinding bindingIntact : Bool) : Bool :=
  freshOrigin && privateBinding && bindingIntact

/-- A fresh-store witness can be minted only from the locked empty-store
observation when the private baseline committed before either native network
listener was exposed. A failed or interrupted commit produces no witness. -/
def freshBackendOrigin (locked empty unexposed committed : Bool) : Bool :=
  locked && empty && unexposed && committed

theorem exposed_fresh_backend_has_no_origin (locked empty committed : Bool) :
    freshBackendOrigin locked empty false committed = false := by
  cases locked <;> cases empty <;> simp [freshBackendOrigin]

/-- Reopen relies on trusted local storage preserving the provenance marker
with the same encrypted store. Authentication binds it to the held store key;
the native policy check is repeated before exposing the store and on admission.
This does not protect against an actor who can replace individual store files. -/
def freshBackendReopenAllowed (markerAuthentic sameStore privateBinding : Bool) : Bool :=
  markerAuthentic && sameStore && privateBinding

theorem missing_backend_marker_cannot_reopen (sameStore privateBinding : Bool) :
    freshBackendReopenAllowed false sameStore privateBinding = false := by
  simp [freshBackendReopenAllowed]

theorem fresh_backend_without_origin_refused (privateBinding bindingIntact : Bool) :
    freshBackendAdmissionAllowed false privateBinding bindingIntact = false := by
  simp [freshBackendAdmissionAllowed]

theorem fresh_backend_without_private_binding_refused (freshOrigin bindingIntact : Bool) :
    freshBackendAdmissionAllowed freshOrigin false bindingIntact = false := by
  cases freshOrigin <;> simp [freshBackendAdmissionAllowed]

/-- A legacy backend with a secret in its accessible history cannot be shared
with an Agent merely because its current value was redacted. -/
theorem legacy_backend_redaction_does_not_make_history_public :
    backendMetadataShareable false true = false := by decide

/-- Inline keys cannot be exposed through a shared metadata document. -/
theorem current_inline_secret_cannot_be_shared (historicalSecret : Bool) :
    backendMetadataShareable true historicalSecret = false := by
  simp [backendMetadataShareable]

/-- Runtime provider work includes execution, discovery, usage observations,
and plugin provider transport. When a Profile is selected the requester must
be authorized to read it; background discovery may have no Profile. The
requester must be authorized to read secret-free Backend metadata. Only the authenticated
runtime may read the referenced secret, whose Node and credential identity
must match the selected backend metadata. A credential may be deliberately
shared by several backends on that Node. The actual provider and destination
must also match an operator-approved credential binding, including any
redirects; an Agent-editable endpoint must not turn the runtime into a secret
exfiltration oracle. Neither the Agent nor a plugin
receives the raw credential. Native ACP, the runtime identity binding, and
the exact metadata/credential reads are external premises. -/
structure BackendCredentialAccess where
  profileRequired : Bool
  profileVisible : Bool
  backendVisible : Bool
  runtimeActor : Bool
  secretReadable : Bool
  sameNode : Bool
  selectedBackend : Bool
  sameCredential : Bool
  destinationApproved : Bool
  credentialVersionMatches : Bool
  deriving BEq, Repr

def runtimeMayUseBackendCredential (access : BackendCredentialAccess)
    (currentSecret historicalSecret : Bool) : Bool :=
  (!access.profileRequired || access.profileVisible) && access.backendVisible &&
    backendMetadataShareable currentSecret historicalSecret &&
    access.runtimeActor && access.secretReadable && access.sameNode &&
    access.selectedBackend && access.sameCredential && access.destinationApproved &&
    access.credentialVersionMatches

/-- A denied requester cannot obtain provider service through the runtime. -/
theorem hidden_backend_cannot_use_credential (access : BackendCredentialAccess)
    (currentSecret historicalSecret : Bool)
    (hidden : access.backendVisible = false) :
    runtimeMayUseBackendCredential access currentSecret historicalSecret = false := by
  simp [runtimeMayUseBackendCredential, hidden]

/-- A hidden Profile cannot be used to obtain its selected backend credential. -/
theorem hidden_profile_cannot_use_credential (access : BackendCredentialAccess)
    (currentSecret historicalSecret : Bool)
    (required : access.profileRequired = true)
    (hidden : access.profileVisible = false) :
    runtimeMayUseBackendCredential access currentSecret historicalSecret = false := by
  simp [runtimeMayUseBackendCredential, required, hidden]

/-- A credential from another Node is never used for this backend. -/
theorem foreign_node_credential_refused (access : BackendCredentialAccess)
    (currentSecret historicalSecret : Bool)
    (foreign : access.sameNode = false) :
    runtimeMayUseBackendCredential access currentSecret historicalSecret = false := by
  simp [runtimeMayUseBackendCredential, foreign]

/-- An Agent cannot impersonate the runtime to read a credential. -/
theorem agent_cannot_resolve_runtime_secret (access : BackendCredentialAccess)
    (currentSecret historicalSecret : Bool)
    (notRuntime : access.runtimeActor = false) :
    runtimeMayUseBackendCredential access currentSecret historicalSecret = false := by
  simp [runtimeMayUseBackendCredential, notRuntime]

/-- The runtime never sends a credential to an unapproved provider destination. -/
theorem unapproved_destination_refused (access : BackendCredentialAccess)
    (currentSecret historicalSecret : Bool)
    (unapproved : access.destinationApproved = false) :
    runtimeMayUseBackendCredential access currentSecret historicalSecret = false := by
  simp [runtimeMayUseBackendCredential, unapproved]

/-- A new provider operation cannot use a cached credential when its admitted
version is already known to have rotated. An in-flight request is outside this
admission decision. -/
theorem stale_credential_refused (access : BackendCredentialAccess)
    (currentSecret historicalSecret : Bool)
    (rotated : access.credentialVersionMatches = false) :
    runtimeMayUseBackendCredential access currentSecret historicalSecret = false := by
  simp [runtimeMayUseBackendCredential, rotated]

/-- The native transaction commits the metadata reference and private version
together; a denied write, mismatched reference, or failed commit changes neither. -/
structure BackendCredentialPublication where
  metadataVersion : String
  secretVersion : String
  deriving BEq, DecidableEq, Repr

def publishBackendCredential (prior : BackendCredentialPublication)
    (metadataVersion secretVersion : String) (authorized committed : Bool) : BackendCredentialPublication :=
  if authorized && committed && decide (metadataVersion = secretVersion) then
    ⟨metadataVersion, secretVersion⟩
  else prior

theorem failed_backend_credential_publication_is_unchanged
    (prior : BackendCredentialPublication) (metadataVersion secretVersion : String)
    (authorized : Bool) :
    publishBackendCredential prior metadataVersion secretVersion authorized false = prior := by
  simp [publishBackendCredential]

theorem mismatched_backend_credential_publication_is_unchanged
    (prior : BackendCredentialPublication) (metadataVersion secretVersion : String)
    (authorized committed : Bool) (different : metadataVersion ≠ secretVersion) :
    publishBackendCredential prior metadataVersion secretVersion authorized committed = prior := by
  simp [publishBackendCredential, different]

/-- A configuration reference is application data, not a native ACP grant.
Creation alone installs neither a relationship to a selecting Agent nor a
grant to its actor, including when several Agents select the same document. -/
theorem create_does_not_install_selection_grants (state : State)
    (creator resource id : String) (authenticated nodeCreate : Bool) :
    (directCreate state creator resource id authenticated nodeCreate).relations = state.relations :=
  direct_create_has_no_implicit_relations state creator resource id authenticated nodeCreate

/-- The authenticated creator becomes the native owner. If an Agent creates
its own configuration, revoking a separate writer relation does not revoke its
ownership; a policy requiring user ownership must account for that create path. -/
theorem agent_created_configuration_retains_creator (state : State)
    (agent resource id : String)
    (fresh : state.objects.any (fun object => object.resource == resource && object.id == id) = false)
    (grant : Relation) :
    (apply (directCreate state agent resource id true true) (.revoke grant)).objects =
      ⟨resource, id, agent⟩ :: state.objects := by
  simp [apply, directCreate, fresh]

/-- A runtime reads the user-owned Node and its Agents through its own
explicit reader grant. The grant conveys no administration or session access. -/
def runtimeReadFixture : State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"agents", "agent", "user"⟩,
      ⟨"sessions", "session", "user"⟩],
    relations := [⟨"nodes", "node", "runtime_reader", .actor "runtime"⟩,
      ⟨"agents", "agent", "parent", .object "nodes" "node"⟩,
      ⟨"sessions", "session", "parent", .object "agents" "agent"⟩],
    enrolled := [] }

theorem runtime_read_grant_is_limited :
    [Household.allowed 5 runtimeReadFixture "runtime" "nodes" "node" "read",
     Household.allowed 5 runtimeReadFixture "runtime" "agents" "agent" "read",
     Household.allowed 5 runtimeReadFixture "runtime" "nodes" "node" "update",
     Household.allowed 5 runtimeReadFixture "runtime" "sessions" "session" "read"] =
    [true, true, false, false] := by native_decide

theorem revoked_runtime_reader_loses_inherited_read :
    let revoked := apply runtimeReadFixture
      (.revoke ⟨"nodes", "node", "runtime_reader", .actor "runtime"⟩)
    [Household.allowed 5 revoked "runtime" "nodes" "node" "read",
     Household.allowed 5 revoked "runtime" "agents" "agent" "read"] = [false, false] := by native_decide

theorem unlinked_agent_is_not_visible_to_runtime :
    Household.allowed 5 { runtimeReadFixture with
      relations := [⟨"nodes", "node", "runtime_reader", .actor "runtime"⟩] }
      "runtime" "agents" "agent" "read" = false := by native_decide

end Identity.Configuration
