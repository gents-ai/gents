import Proofs.Identity.Permission
import Lean

namespace Identity.Household

structure RelationDefinition where
  name : String
  types : List String := []
  manages : List String := []
  deriving BEq, Repr

structure PermissionDefinition where
  name : String
  relations : List String := []
  parents : List (String × String) := []
  deriving BEq, Repr

structure ResourceDefinition where
  name : String
  relations : List RelationDefinition
  permissions : List PermissionDefinition
  deriving BEq, Repr

def permission (name : String) (relations : List String)
    (parents : List (String × String)) : PermissionDefinition :=
  ⟨name, relations, parents⟩

/-- Required native policy wiring remains unenforced pending #3485. Users own Node
configuration and transitively administer Agent configuration through its parent Node.
Narrower per-Agent reader, session-writer and administrator grants stay independent.
User-created sessions are user-owned with an explicit working agent. Unattended sessions
are agent-owned and inherit configured user access and deletion through their parent Agent.
Session children are agent-owned and inherit user access and control from their session.
Agent children such as callback invocations and results are agent-owned and inherit
user access and control from their physical Agent parent. The runtime key owns
node-local operational data not caused by an agent and authenticates peer admission;
operational children inherit user access from the user-owned Node. Agent-caused work
never falls back to runtime authority. Enrollment itself grants no document access. ACP1 cannot create relationships
on demand when an event triggers an agent; configure required grants beforehand where
possible. Runtime configuration readers do not inherit session access: session_read
and session_admin follow user ownership and explicit grants, separately from Agent
configuration read and self-configuration. All checks are native database
read/update/delete and ownership checks. A distinct native Agent invoke
permission grants request creation; a configuration reader cannot invoke. A
delivered request is sender-owned and may link to its target Agent after its
creation; that grant is not atomic with document creation. Once its session
exists, the session relationship also permits explicitly granted session readers
to observe it. Inherited document permissions do not grant relation management:
the document owner or a directly granted manager must perform those grants. Native
collection bindings contain the policy ID. Adding resources changes that ID; existing
bindings cannot be silently rewritten and require an explicit supported upgrade. -/
def policy : List ResourceDefinition :=
  [ { name := "nodes"
      relations := [⟨"administrator", ["actor"], ["administrator", "runtime_reader"]⟩, ⟨"runtime_reader", ["actor"], []⟩]
      permissions := [permission "read" ["administrator", "runtime_reader"] [],
        permission "update" ["administrator"] [], permission "delete" ["administrator"] []] }
  , { name := "operations"
      relations := [⟨"parent", ["nodes"], []⟩]
      permissions := [permission "read" [] [("parent", "read")],
        permission "update" [] [], permission "delete" [] []] }
  , { name := "pairing_observations"
      relations := [⟨"parent", ["nodes"], []⟩, ⟨"route_reader", ["actor"], []⟩,
        ⟨"tool_reader", ["actor"], []⟩,
        ⟨"reader_manager", ["actor"], ["route_reader", "tool_reader"]⟩]
      permissions := [permission "read" ["route_reader", "tool_reader"] [("parent", "read")],
        permission "update" [] [], permission "delete" [] []] }
  , { name := "configuration"
      relations := [⟨"parent", ["nodes"], []⟩, ⟨"reader", ["actor"], []⟩,
        ⟨"selected_by", ["agents->executor", "configuration->selected_by"], []⟩,
        ⟨"selection_manager", ["actor"], ["selected_by"]⟩,
        ⟨"writer", ["actor"], []⟩]
      permissions := [permission "read" ["reader", "writer", "selected_by"] [("parent", "read")],
        permission "update" ["writer"] [("parent", "update")],
        permission "delete" [] [("parent", "delete")]] }
  , { name := "secret_configuration"
      relations := [⟨"parent", ["nodes"], []⟩, ⟨"runtime_writer", ["actor"], []⟩]
      permissions := [permission "read" ["runtime_writer"] [("parent", "read")],
        permission "update" ["runtime_writer"] [("parent", "update")],
        permission "delete" [] [("parent", "delete")]] }
  , { name := "agents"
      relations := [⟨"parent", ["nodes"], []⟩, ⟨"reader", ["actor"], []⟩,
        ⟨"session_writer", ["actor"], []⟩, ⟨"administrator", ["actor"],
          ["parent", "reader", "session_writer", "administrator", "executor", "config_writer"]⟩,
        ⟨"executor", ["actor"], []⟩, ⟨"config_writer", ["actor"], []⟩]
      permissions := [permission "read" ["reader", "session_writer", "administrator", "executor", "config_writer"] [("parent", "read")],
        permission "update" ["administrator", "config_writer"] [("parent", "update")],
        permission "delete" ["administrator"] [("parent", "delete")],
        permission "invoke" ["session_writer", "executor", "administrator"] [("parent", "delete")],
        permission "session_read" ["reader", "session_writer", "administrator"] [("parent", "delete")],
        permission "session_admin" ["administrator"] [("parent", "delete")]] }
  , { name := "sessions"
      relations := [⟨"parent", ["agents"], []⟩, ⟨"reader", ["actor"], []⟩,
        ⟨"working_agent", ["actor"], []⟩]
      permissions := [permission "read" ["reader", "working_agent"] [("parent", "session_read")],
        permission "update" ["working_agent"] [("parent", "session_admin"), ("parent", "session_writer")],
        permission "delete" [] [("parent", "session_admin")]] }
  , { name := "delivered_requests"
      relations := [⟨"parent", ["agents"], []⟩, ⟨"session", ["sessions"], []⟩,
        ⟨"working_agent", ["actor"], []⟩,
        ⟨"reader", ["actor"], []⟩]
      permissions := [permission "read" ["working_agent", "reader"] [("parent", "session_read"), ("session", "read")],
        permission "update" ["working_agent"] [("parent", "session_admin"), ("session", "delete")],
        permission "delete" [] [("parent", "session_admin"), ("session", "delete")]] }
  , { name := "goal_controls"
      relations := [⟨"parent", ["sessions"], []⟩, ⟨"working_agent", ["actor"], []⟩]
      permissions := [permission "read" ["working_agent"] [("parent", "read")],
        permission "update" ["working_agent"] [("parent", "delete")],
        permission "delete" [] [("parent", "delete")]] }
  , { name := "goal_receipts"
      relations := [⟨"parent", ["goal_controls"], []⟩, ⟨"reader", ["actor"], []⟩]
      permissions := [permission "read" ["reader"] [("parent", "read")],
        permission "update" [] [], permission "delete" [] []] }
  , { name := "agent_children"
      relations := [⟨"parent", ["agents"], []⟩]
      permissions := [permission "read" [] [("parent", "session_read")],
        permission "update" [] [("parent", "session_admin")],
        permission "delete" [] [("parent", "session_admin")]] }
  , { name := "workspaces"
      relations := [⟨"parent", ["agents"], []⟩, ⟨"reader", ["actor"], []⟩, ⟨"writer", ["actor"], []⟩,
        ⟨"integrator", ["actor"], []⟩]
      permissions := [permission "read" ["reader", "writer", "integrator"] [("parent", "session_read")],
        permission "update" ["writer", "integrator"] [("parent", "session_admin")],
        permission "delete" [] [("parent", "session_admin")]] }
  , { name := "workspace_children"
      relations := [⟨"parent", ["workspaces"], []⟩]
      permissions := [permission "read" [] [("parent", "read")],
        permission "update" [] [("parent", "update")],
        permission "delete" [] [("parent", "delete")]] }
  , { name := "children"
      relations := [⟨"parent", ["sessions"], []⟩, ⟨"reader", ["actor"], []⟩,
        ⟨"working_agent", ["actor"], []⟩]
      permissions := [permission "read" ["reader", "working_agent"] [("parent", "read")],
        permission "update" ["working_agent"] [("parent", "delete")],
        permission "delete" [] [("parent", "delete")]] } ]

structure Object where
  resource : String
  id : String
  owner : DID
  deriving Repr

inductive Subject where
  | actor (did : DID)
  | object (resource id : String)
  | entitySet (resource id relation : String)
  deriving BEq, Repr

structure Relation where
  resource : String
  id : String
  relation : String
  subject : Subject
  deriving BEq, Repr

structure State where
  objects : List Object
  relations : List Relation
  enrolled : List DID
  deriving Repr

/-- Managed Agent creation stages the child document and its Node parent and
executor relationships in one native transaction. A failed transaction exposes
none of them; NAC capabilities are granted only after commit. -/
def commitAgentCreation (state : State) (childId creator childDid nodeId : String)
    (commit : Bool) : State :=
  if commit then
    { state with
      objects := ⟨"agents", childId, creator⟩ :: state.objects
      relations := ⟨"agents", childId, "parent", .object "nodes" nodeId⟩ ::
        ⟨"agents", childId, "executor", .actor childDid⟩ :: state.relations }
  else state

theorem aborted_agent_creation_is_unchanged (state : State)
    (childId creator childDid nodeId : String) :
    commitAgentCreation state childId creator childDid nodeId false = state := by rfl

theorem committed_agent_creation_retains_creator_and_parent (state : State)
    (childId creator childDid nodeId : String) :
    (commitAgentCreation state childId creator childDid nodeId true).objects =
      ⟨"agents", childId, creator⟩ :: state.objects ∧
    (commitAgentCreation state childId creator childDid nodeId true).relations =
      ⟨"agents", childId, "parent", .object "nodes" nodeId⟩ ::
        ⟨"agents", childId, "executor", .actor childDid⟩ :: state.relations := by
  exact ⟨rfl, rfl⟩

/-- Conditional atomic-publication contract for an implementation whose native
transaction actually includes both the document and its relationships. ACP1
alone does not supply that premise when grants are made after creation. -/
def commitUserSession (state : State) (sessionId user agent agentId : String) (commit : Bool) : State :=
  if commit then
    { state with
      objects := ⟨"sessions", sessionId, user⟩ :: state.objects
      relations := ⟨"sessions", sessionId, "parent", .object "agents" agentId⟩ ::
        ⟨"sessions", sessionId, "working_agent", .actor agent⟩ :: state.relations }
  else state

theorem aborted_user_session_is_unchanged (state : State) (sessionId user agent agentId : String) :
    commitUserSession state sessionId user agent agentId false = state := by rfl

theorem committed_user_session_has_working_agent (state : State) (sessionId user agent agentId : String) :
    (commitUserSession state sessionId user agent agentId true).relations =
      ⟨"sessions", sessionId, "parent", .object "agents" agentId⟩ ::
      ⟨"sessions", sessionId, "working_agent", .actor agent⟩ :: state.relations := by rfl

/-- A fresh client reservation publishes its session and both relationships
in one native transaction. An existing session is never regranted by repeating
the reservation; an interrupted older pending publication is recovered by its
separate creator-checked recovery owner. -/
def reserveUserSession (state : State) (sessionId user agent agentId : String)
    (fresh commit : Bool) : State :=
  if fresh then commitUserSession state sessionId user agent agentId commit else state

theorem user_session_reservation_duplicate_preserves_state
    (state : State) (sessionId user agent agentId : String) (commit : Bool) :
    reserveUserSession state sessionId user agent agentId false commit = state := by rfl

theorem user_session_reservation_abort
    (state : State) (sessionId user agent agentId : String) (fresh : Bool) :
    reserveUserSession state sessionId user agent agentId fresh false = state := by
  cases fresh <;> rfl

/-- Conditional atomic-publication contract for an agent-created session and
its Agent parent; it depends on the document and grant sharing a transaction. -/
def commitAgentSession (state : State) (sessionId agent agentId : String) (commit : Bool) : State :=
  if commit then
    { state with
      objects := ⟨"sessions", sessionId, agent⟩ :: state.objects
      relations := ⟨"sessions", sessionId, "parent", .object "agents" agentId⟩ :: state.relations }
  else state

theorem aborted_agent_session_is_unchanged (state : State) (sessionId agent agentId : String) :
    commitAgentSession state sessionId agent agentId false = state := by rfl

theorem committed_agent_session_has_parent (state : State) (sessionId agent agentId : String) :
    (commitAgentSession state sessionId agent agentId true).relations =
      ⟨"sessions", sessionId, "parent", .object "agents" agentId⟩ :: state.relations := by rfl

/-- Conditional atomic-publication contract for an agent child. ACP1 does not
ensure the actor or parent merely because the child contains their identifiers. -/
def commitAgentChild (state : State) (childId agent agentId : String) (commit : Bool) : State :=
  if commit then
    { state with
      objects := ⟨"agent_children", childId, agent⟩ :: state.objects
      relations := ⟨"agent_children", childId, "parent", .object "agents" agentId⟩ :: state.relations }
  else state

theorem aborted_agent_child_is_unchanged (state : State) (childId agent agentId : String) :
    commitAgentChild state childId agent agentId false = state := by rfl

theorem committed_agent_child_has_owner_and_parent (state : State) (childId agent agentId : String) :
    (commitAgentChild state childId agent agentId true).objects =
      ⟨"agent_children", childId, agent⟩ :: state.objects ∧
    (commitAgentChild state childId agent agentId true).relations =
      ⟨"agent_children", childId, "parent", .object "agents" agentId⟩ :: state.relations := by
  exact ⟨rfl, rfl⟩

/-- ACP1 admits a direct document create when the authenticated actor passes
the node's create gate. The new native owner is that actor; target fields do
not themselves install or authorize any relationship. This transition models
the adversarial GraphQL path, independently of trusted publishers. -/
def directCreate (state : State) (actor resource id : String)
    (authenticated nodeCreate : Bool) : State :=
  if authenticated && nodeCreate &&
      !(state.objects.any (fun object => object.resource == resource && object.id == id)) then
    { state with objects := ⟨resource, id, actor⟩ :: state.objects }
  else state

theorem direct_create_has_no_implicit_relations (state : State)
    (actor resource id : String) (authenticated nodeCreate : Bool) :
    (directCreate state actor resource id authenticated nodeCreate).relations = state.relations := by
  unfold directCreate
  split <;> rfl

theorem direct_receipt_creator_need_not_be_selected_agent
    (state : State) (actor selectedAgent id : String) (_different : actor ≠ selectedAgent)
    (fresh : state.objects.any (fun object => object.resource == "goal_receipts" && object.id == id) = false) :
    (directCreate state actor "goal_receipts" id true true).objects =
      ⟨"goal_receipts", id, actor⟩ :: state.objects := by simp [directCreate, fresh]

/-- Conditional authorization fence: this calculation is sound only if the
native transaction observes and validates the versions of all direct and
inherited ACP dependencies. ACP1's detached permission and relationship calls
alone do not establish that external premise. -/
def commitAuthorized (authorized : Bool) (observedVersion currentVersion : Nat) : Bool :=
  authorized && observedVersion == currentVersion

theorem revoked_authority_cannot_commit (version : Nat) :
    commitAuthorized true version (version + 1) = false := by simp [commitAuthorized]

theorem authority_race_cases :
    [commitAuthorized true 7 7, commitAuthorized true 7 8,
      commitAuthorized false 8 8, commitAuthorized true 7 9] =
    [true, false, false, false] := by decide

/-- Go-compatible IDs omit types and management metadata. Registration must
atomically compare the complete policy at its ID, including those fields; a
preflight read followed by an overwrite does not satisfy this contract. -/
def registerPolicy (existing : Option String) (candidate : String) : Option String :=
  match existing with
  | none => some candidate
  | some prior => if prior == candidate then some prior else none

theorem conflicting_policy_cannot_replace (prior candidate : String) (different : prior ≠ candidate) :
    registerPolicy (some prior) candidate = none := by
  simp [registerPolicy, beq_iff_eq, different]

theorem two_distinct_policy_registrations_cannot_both_win
    (left right : String) (different : left ≠ right) :
    registerPolicy (registerPolicy none left) right = none := by
  simp [registerPolicy, beq_iff_eq, different]

/-- Fuel bounds the child → session → Agent → Node hierarchy for registered
objects. Unregistered existing objects follow `nativeReadForExisting` instead;
their fail-open behavior is not represented by this lookup. Native cycle handling
is not asserted by this model. Relation targets such as session_writer are native
tuple-to-userset checks and do not inherit the automatic object-owner permission. -/
def allowedIn : List ResourceDefinition → Nat → State → DID → String → String → String → Bool
  | _, 0, _, _, _, _, _ => false
  | definitions, fuel + 1, state, actor, resource, id, permission =>
    match state.objects.find? (fun object => object.resource == resource && object.id == id),
        (definitions.find? (·.name == resource)).bind (fun r => r.permissions.find? (·.name == permission)) with
    | some object, some definition =>
      object.owner == actor || definition.relations.any (fun relation =>
        state.relations.any (fun fact => fact.resource == resource && fact.id == id &&
          fact.relation == relation && match fact.subject with
          | .actor did => did == actor
          | .object .. => false
          | .entitySet target targetId targetRelation =>
            allowedIn definitions fuel state actor target targetId targetRelation)) ||
      definition.parents.any (fun (relation, inherited) => state.relations.any (fun fact =>
        fact.resource == resource && fact.id == id && fact.relation == relation &&
        match fact.subject with
        | .actor _ => false
        | .entitySet .. => false
        | .object parentResource parentId =>
          allowedIn definitions fuel state actor parentResource parentId inherited))
    | some _, none =>
      ((definitions.find? (·.name == resource)).map (fun definition =>
        definition.relations.any (·.name == permission))).getD false &&
        state.relations.any (fun fact => fact.resource == resource && fact.id == id &&
          fact.relation == permission && match fact.subject with
          | .actor did => did == actor
          | .object .. => false
          | .entitySet target targetId targetRelation =>
            allowedIn definitions fuel state actor target targetId targetRelation)
    | _, _ => false

def allowed (fuel : Nat) (state : State) (actor resource id permission : String) : Bool :=
  allowedIn policy fuel state actor resource id permission

/-- An Agent-created Agent is owned by the creating Agent. The Node parent
separately gives its user owner inherited control; an ungranted sibling has no
access merely because it belongs to the same runtime. -/
def agentCreatedAgentFixture : State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"agents", "child", "creator"⟩],
    relations := [⟨"agents", "child", "parent", .object "nodes" "node"⟩],
    enrolled := [] }

theorem agent_creator_and_node_owner_control_child :
    [allowed 5 agentCreatedAgentFixture "creator" "agents" "child" "read",
     allowed 5 agentCreatedAgentFixture "creator" "agents" "child" "update",
     allowed 5 agentCreatedAgentFixture "user" "agents" "child" "read",
     allowed 5 agentCreatedAgentFixture "user" "agents" "child" "update",
     allowed 5 agentCreatedAgentFixture "sibling" "agents" "child" "read",
     allowed 5 agentCreatedAgentFixture "sibling" "agents" "child" "update"] =
    [true, true, true, true, false, false] := by native_decide

/-- After an Agent is admitted, each effect is authorized as that Agent by
DefraDB. An invoker does not narrow the Agent's principal or intersect its
document permissions. Granting `invoke` therefore makes the Agent's configured
authority reachable to its invokers; ordinary native ACP still decides each
effect. -/
def agentEffectAllowed (state : State) (agent : DID) (resource id permission : String) : Bool :=
  allowed 5 state agent resource id permission

theorem agent_effect_is_its_native_document_authority (state : State) (agent : DID)
    (resource id permission : String) :
    agentEffectAllowed state agent resource id permission =
      allowed 5 state agent resource id permission := rfl

/-- Unattended work uses the Agent's standing, user-granted native authority;
it does not require an online requester. This is a configuration premise for
the work, not a separate document authorization mechanism. -/
def unattendedEffectAllowed (standingUserGrant : Bool) (state : State) (agent : DID)
    (resource id permission : String) : Bool :=
  standingUserGrant && agentEffectAllowed state agent resource id permission

/-- Configured subagent targets and pack grants must expose only authority that
each of the parent Agent's invokers may reach. This is an obligation on the
person configuring those grants, not an additional runtime authorization layer. -/
def configuredAuthorityPremise (invokers resources : List String)
    (configured : String → Bool) (mayReach : String → String → Bool) : Prop :=
  ∀ invoker, invoker ∈ invokers → ∀ resource, resource ∈ resources →
    configured resource = true → mayReach invoker resource = true

theorem configured_authority_is_reachable (invokers resources : List String)
    (configured : String → Bool) (mayReach : String → String → Bool)
    (premise : configuredAuthorityPremise invokers resources configured mayReach)
    (invoker resource : String) (invokerMember : invoker ∈ invokers)
    (resourceMember : resource ∈ resources) (enabled : configured resource = true) :
    mayReach invoker resource = true :=
  premise invoker invokerMember resource resourceMember enabled

/-- A subagent result becomes available through the parent when it has been
delivered as a child of that readable parent session. This does not imply a
direct grant to the subagent's session or documents. -/
def returnedResultReadable (state : State) (reader result : String) : Bool :=
  allowed 5 state reader "children" result "read"

theorem parent_owner_reads_returned_result :
    returnedResultReadable
      { objects := [⟨"sessions", "parent", "user"⟩, ⟨"children", "result", "agent"⟩],
        relations := [⟨"children", "result", "parent", .object "sessions" "parent"⟩],
        enrolled := [] } "user" "result" = true := by native_decide

theorem enrollment_grants_no_document_access_in (definitions : List ResourceDefinition)
    (fuel : Nat) (state : State) (enrolled : List DID) (actor resource id permission : String) :
    allowedIn definitions fuel { state with enrolled := enrolled } actor resource id permission =
      allowedIn definitions fuel state actor resource id permission := by
  induction fuel generalizing actor resource id permission with
  | zero => rfl
  | succ fuel ih => simp only [allowedIn]; simp_all

theorem enrollment_grants_no_document_access (fuel : Nat) (state : State)
    (enrolled : List DID) (actor resource id permission : String) :
    allowed fuel { state with enrolled := enrolled } actor resource id permission =
      allowed fuel state actor resource id permission :=
  enrollment_grants_no_document_access_in policy fuel state enrolled actor resource id permission

/-- ACP1 grants relationship management to the document owner and actors with
an explicitly declared direct manager relation on that same document. Inherited
read, update and delete permissions do not manage relationships. -/
def mayManage (state : State) (actor : DID) (relation : Relation) : Bool :=
  match policy.find? (·.name == relation.resource),
      state.objects.find? (fun object => object.resource == relation.resource && object.id == relation.id) with
  | some definition, some object =>
      relation.relation != "owner" &&
        (object.owner == actor || definition.relations.any (fun manager =>
          manager.manages.contains relation.relation && state.relations.any (fun fact =>
            fact.resource == relation.resource && fact.id == relation.id &&
            fact.relation == manager.name && fact.subject == .actor actor)))
  | _, _ => false

inductive Action where
  | grant (relation : Relation)
  | revoke (relation : Relation)
  | grantAs (actor : DID) (relation : Relation)
  | revokeAs (actor : DID) (relation : Relation)
  | check (actor : DID) (resource id permission : String)
  deriving Repr

def apply (state : State) : Action → State
  | .grant relation => { state with relations := relation :: state.relations }
  | .revoke relation => { state with relations := state.relations.filter (· != relation) }
  | .grantAs actor relation => if mayManage state actor relation then
      { state with relations := relation :: state.relations } else state
  | .revokeAs actor relation => if mayManage state actor relation then
      { state with relations := state.relations.filter (· != relation) } else state
  | .check .. => state

/-- ACP1 owners can manage their own document relationships. A parent tuple
does not prove that the owner was authorized to use the target; that must be
established by another trusted boundary or by a native create constraint. -/
def trialBase : State :=
  { objects := [⟨"sessions", "victim-session", "victim"⟩,
      ⟨"goal_controls", "command", "victim"⟩],
    relations := [], enrolled := [] }

def forgedChild : State :=
  apply (directCreate trialBase "attacker" "children" "forged" true true)
    (.grantAs "attacker" ⟨"children", "forged", "parent", .object "sessions" "victim-session"⟩)

theorem owner_can_attach_unverified_parent :
    allowed 5 forgedChild "victim" "children" "forged" "read" = true := by native_decide

/-- The receipt creator and the reader grant are separate ACP1 operations. A
crash or observer between them can see the document before its intended reader
is granted access; an atomic combined transaction is an additional premise. -/
def receiptBeforeGrant : State :=
  directCreate trialBase "agent" "goal_receipts" "receipt" true true

def receiptAfterGrant : State :=
  apply receiptBeforeGrant
    (.grantAs "agent" ⟨"goal_receipts", "receipt", "parent", .object "goal_controls" "command"⟩)

theorem receipt_grant_window :
    (allowed 5 receiptBeforeGrant "agent" "goal_receipts" "receipt" "read",
     allowed 5 receiptBeforeGrant "victim" "goal_receipts" "receipt" "read",
     allowed 5 receiptAfterGrant "victim" "goal_receipts" "receipt" "read") =
      (true, false, true) := by native_decide

/-- Receipt trust requires a valid signature over the exact command-bound
payload under the selected agent's DID. ACP1 ownership and a parent tuple do
not authenticate that signature. The Boolean is the cryptographic verifier's
external result for that payload and DID. -/
def acceptGoalReceipt (selectedAgent signer : DID) (signatureValid : Bool) : Bool :=
  signatureValid && signer == selectedAgent

theorem receipt_with_other_signer_is_rejected (selectedAgent signer : DID)
    (different : signer ≠ selectedAgent) (signatureValid : Bool) :
    acceptGoalReceipt selectedAgent signer signatureValid = false := by
  simp [acceptGoalReceipt, beq_iff_eq, different]

/-- The ACP1 trial assumes authenticated node writers follow the trusted
publication protocol. This is an operating premise, not document isolation
against a malicious enrolled writer, which can use `directCreate` and
owner-managed relationships. A pending marker records unfinished grants; the
marker and its owner are themselves subject to ordinary ACP1 mutations. -/
structure PendingPublication where
  committed : Bool
  pending : Bool
  parentGranted : Bool
  readerGranted : Bool
  deriving Repr

def beginPublication : PendingPublication := ⟨true, true, false, false⟩

/-- Grant success means the native operation completed with the relation
present, including an idempotent already-present result. Failure preserves the
prior state so a sweep can retry it. -/
def grantParent (state : PendingPublication) (succeeded : Bool) : PendingPublication :=
  { state with parentGranted := state.parentGranted || succeeded }

def grantReader (state : PendingPublication) (succeeded : Bool) : PendingPublication :=
  { state with readerGranted := state.readerGranted || succeeded }

def clearPublicationMarker (state : PendingPublication) : PendingPublication :=
  if state.committed && state.parentGranted && state.readerGranted then
    { state with pending := false }
  else state

def sweepPublication (state : PendingPublication) (parentOk readerOk : Bool) : PendingPublication :=
  if state.pending then
    clearPublicationMarker (grantReader (grantParent state parentOk) readerOk)
  else state

theorem failed_grants_remain_pending :
    (sweepPublication beginPublication false false).pending = true := by rfl

theorem subsequent_successful_sweep_completes :
    let failed := sweepPublication beginPublication false false
    let repaired := sweepPublication failed true true
    repaired.pending = false ∧ repaired.parentGranted = true ∧ repaired.readerGranted = true := by decide

theorem single_failed_grant_does_not_clear_marker :
    (sweepPublication beginPublication true false).pending = true ∧
      (sweepPublication beginPublication false true).pending = true := by decide

/-- An owner may revoke a relationship after completion. A cleared marker is
not proof that grants still exist unless their current state is re-observed. -/
def ownerRevokesParent (state : PendingPublication) : PendingPublication :=
  { state with parentGranted := false }

theorem completed_marker_does_not_fence_owner_revocation :
    let completed := sweepPublication beginPublication true true
    let revoked := ownerRevokesParent completed
    revoked.pending = false ∧ revoked.parentGranted = false := by decide

/-- Callback publication records an unlinked document before the owner grants
its parent and marks the link complete. Grant success includes an already-present
result. A retry must preserve a successful grant; visibility and cursor
advancement remain separate observations. -/
structure ParentPublication where
  committed : Bool
  parentGranted : Bool
  parentLinked : Bool
  deriving Repr, DecidableEq

def beginParentPublication : ParentPublication := ⟨true, false, false⟩

def repairParent (state : ParentPublication) (grantSucceeded markSucceeded : Bool) : ParentPublication :=
  let granted := state.parentGranted || grantSucceeded
  { state with
    parentGranted := granted
    parentLinked := state.parentLinked || (state.committed && granted && markSucceeded) }

theorem failed_parent_grant_cannot_mark_linked :
    (repairParent beginParentPublication false true).parentLinked = false := by rfl

theorem grant_survives_failed_marker_and_retry_completes :
    let interrupted := repairParent beginParentPublication true false
    let retried := repairParent interrupted false true
    interrupted.parentGranted = true ∧ interrupted.parentLinked = false ∧
      retried.parentGranted = true ∧ retried.parentLinked = true := by decide

/-- Recovery may authenticate a creator independently of writable row metadata
and compare it with native ownership. The check binds context; actual relation
management remains a native ACP1 operation. Missing or conflicting evidence
cannot select a node or another agent as a substitute actor. -/
def recoverParent (state : ParentPublication)
    (nativeOwner claimedCreator authenticatedCreator : Option DID)
    (grantSucceeded markSucceeded : Bool) : ParentPublication :=
  if nativeOwner.isSome && nativeOwner == claimedCreator && nativeOwner == authenticatedCreator then
    repairParent state grantSucceeded markSucceeded
  else state

theorem forged_creator_metadata_cannot_select_recovery_actor :
    recoverParent beginParentPublication (some "agent") (some "other") (some "agent") true true =
      beginParentPublication := by decide

theorem missing_authenticated_creator_cannot_select_recovery_actor :
    recoverParent beginParentPublication (some "agent") (some "agent") none true true =
      beginParentPublication := by decide

theorem authenticated_parent_recovery_is_idempotent :
    let once := recoverParent beginParentPublication (some "agent") (some "agent")
      (some "agent") true true
    recoverParent once (some "agent") (some "agent") (some "agent") true true = once := by decide

def ownerRevokesLinkedParent (state : ParentPublication) : ParentPublication :=
  { state with parentGranted := false }

theorem linked_marker_does_not_fence_later_revocation :
    let linked := repairParent beginParentPublication true true
    let revoked := ownerRevokesLinkedParent linked
    revoked.parentLinked = true ∧ revoked.parentGranted = false := by decide

/-- A worker that already observed an incomplete marker can finish a detached
grant after a concurrent owner completes and then revokes the relationship.
ACP1 supplies no commit fence for that stale observation. -/
def finishStaleParentGrant (state : ParentPublication)
    (observedIncomplete grantSucceeded : Bool) : ParentPublication :=
  if observedIncomplete && grantSucceeded then { state with parentGranted := true } else state

theorem stale_repair_can_restore_revoked_parent :
    let linked := repairParent beginParentPublication true true
    let revoked := ownerRevokesLinkedParent linked
    let stale := finishStaleParentGrant revoked true true
    stale.parentLinked = true ∧ stale.parentGranted = true := by decide

theorem completed_marker_does_not_schedule_new_repair :
    let linked := repairParent beginParentPublication true true
    let revoked := ownerRevokesLinkedParent linked
    finishStaleParentGrant revoked false true = revoked := by rfl

/-- An actor-scoped lookup precedes a separate ACP1 relationship operation.
An absent or hidden Agent has the same observation. A later native manager
decision does not establish that the lookup still names a live document of the
same generation. -/
structure SelfConfigAttempt where
  present : Bool
  visible : Bool
  sameGeneration : Bool
  nativeAllows : Bool
  deriving Repr

def selfConfigOutcome (attempt : SelfConfigAttempt) : String :=
  if !attempt.present || !attempt.visible then "unavailable"
  else if attempt.nativeAllows then "applied" else "denied"

theorem self_config_absent_and_hidden_are_indistinguishable :
    selfConfigOutcome ⟨false, false, false, false⟩ =
      selfConfigOutcome ⟨true, false, true, true⟩ := by rfl

theorem self_config_lookup_does_not_fence_generation :
    selfConfigOutcome ⟨true, true, false, true⟩ = "applied" := by rfl

/-- Desired-state creation commits before ACP1 relationship operations. The
creator receives the remaining grants after a reported failure, but a process
crash loses that in-memory result. Replaying the desired configuration cannot
infer which missing relations were intentionally revoked after publication. -/
structure ConfigPublication where
  committed : Bool
  grantsComplete : Bool
  incompleteReported : Bool
  deriving Repr, DecidableEq

def finishConfigPublication (grantOk crashed : Bool) : ConfigPublication :=
  ⟨true, grantOk, !grantOk && !crashed⟩

theorem config_grant_failure_reports_committed_incomplete :
    finishConfigPublication false false = ⟨true, false, true⟩ := by rfl

theorem config_crash_loses_incomplete_report :
    finishConfigPublication false true = ⟨true, false, false⟩ := by rfl

/-- An ambiguous commit response cannot establish whether any document exists;
blind transaction replay would treat a committed creation as an update and lose
its grants. The caller must retain the intended grants and resolve the outcome. -/
def configMayReplay (commitConfirmedAbsent hasGrants : Bool) : Bool :=
  commitConfirmedAbsent || !hasGrants

theorem ambiguous_config_commit_with_grants_cannot_replay :
    configMayReplay false true = false := by rfl

theorem config_revocation_is_indistinguishable_from_missing_grant :
    (finishConfigPublication false true).grantsComplete =
      ({ finishConfigPublication true false with grantsComplete := false } : ConfigPublication).grantsComplete := by rfl

/-- A node-level permission is an independent gate. Giving two authenticated
actors the same node permission says nothing about which Agent documents they
may access. -/
def nodeGateAllows (nodePermission : Bool) (_actor _selectedAgent : DID) : Bool :=
  nodePermission

theorem node_gate_does_not_distinguish_agents (first second agent : DID) :
    nodeGateAllows true first agent = nodeGateAllows true second agent := by rfl

def twoAgentFixture : State :=
  { objects := [⟨"agents", "first-agent", "first"⟩,
      ⟨"agents", "second-agent", "second"⟩], relations := [], enrolled := [] }

theorem node_gate_does_not_grant_other_agent_document_access :
    nodeGateAllows true "second" "first-agent" = true ∧
      allowed 5 twoAgentFixture "second" "agents" "first-agent" "update" = false := by native_decide

/-- DefraDB's current ACP1 document check treats a policy-bound existing object
without an owner registration as readable. Enabling a policy on legacy rows
therefore needs a native fail-closed premise or verified registration first. -/
def nativeReadForExisting (registered permissionGranted : Bool) : Bool :=
  !registered || permissionGranted

theorem unregistered_existing_object_is_readable :
    nativeReadForExisting false false = true := by rfl

structure QueuedAuthority where
  observed : Bool
  current : Bool
  executed : Bool
  deriving Repr

def revokeQueued (state : QueuedAuthority) : QueuedAuthority :=
  { state with current := false }

/-- A permission observed before enqueue does not authenticate a later
execution. The fresh observation and its effect must share a conflict-validated
native transaction for revocation before execution to be decisive. -/
def executeWithCachedPermission (state : QueuedAuthority) : QueuedAuthority :=
  { state with executed := state.observed }

def executeWithCurrentPermission (state : QueuedAuthority) : QueuedAuthority :=
  { state with executed := state.current }

theorem queued_revocation_needs_current_permission :
    let revoked := revokeQueued ⟨true, true, false⟩
    (executeWithCachedPermission revoked).executed = true ∧
      (executeWithCurrentPermission revoked).executed = false := by decide

def check (state : State) : Action → Option Bool
  | .check actor resource id permission => some (allowed 5 state actor resource id permission)
  | _ => none

open Lean

def policyYamlOf (definitions : List ResourceDefinition) : String :=
  "name: Gents identity ownership model\nresources:\n" ++ String.join (definitions.map fun resource =>
    "  - name: " ++ resource.name ++ "\n" ++
    (if resource.relations.isEmpty then "    relations: []\n" else "    relations:\n") ++
    String.join (resource.relations.map fun relation =>
      "      - name: " ++ relation.name ++ "\n" ++
      (if relation.types.isEmpty then "" else
        "        types: [" ++ String.intercalate ", " relation.types ++ "]\n") ++
      (if relation.manages.isEmpty then "" else
        "        manages: [" ++ String.intercalate ", " relation.manages ++ "]\n")) ++
    "    permissions:\n" ++ String.join (resource.permissions.map fun permission =>
      "      - name: " ++ permission.name ++ "\n" ++
      (let expression := String.intercalate " + " (permission.relations ++
          permission.parents.map (fun (relation, inherited) => relation ++ "->" ++ inherited))
       if expression.isEmpty then "" else "        expr: " ++ expression ++ "\n")))

def policyYaml : String := policyYamlOf policy

/-- The Go-compatible ID input deliberately excludes relation types and
management metadata. Equality of this projection is sufficient for a collision
when the policy name and registration counter are unchanged. -/
def policyIdInput (definitions : List ResourceDefinition) :
    List (String × List String × List (String × List String × List (String × String))) :=
  definitions.map fun resource => (resource.name, resource.relations.map (·.name),
    resource.permissions.map fun permission => (permission.name, permission.relations, permission.parents))

def policyWithDifferentManagement : List ResourceDefinition := policy.map fun resource =>
  if resource.name == "agents" then
    { resource with relations := resource.relations.map fun relation =>
        if relation.name == "administrator" then { relation with manages := ["reader"] } else relation }
  else resource

def policyWithDifferentTypes : List ResourceDefinition := policy.map fun resource =>
  if resource.name == "nodes" then
    { resource with relations := resource.relations.map fun relation =>
        if relation.name == "runtime_reader" then { relation with types := [] } else relation }
  else resource

theorem management_change_collides :
    (policyIdInput policy == policyIdInput policyWithDifferentManagement) = true := by native_decide

theorem type_change_collides :
    (policyIdInput policy == policyIdInput policyWithDifferentTypes) = true := by native_decide

def objectJson (object : Object) : Json := Json.mkObj
  [("resource", toJson object.resource), ("id", toJson object.id), ("owner", toJson object.owner)]

def relationJson (relation : Relation) : Json := Json.mkObj
  [("resource", toJson relation.resource), ("id", toJson relation.id),
   ("relation", toJson relation.relation),
   ("subject", match relation.subject with
     | .actor did => Json.mkObj [("actor", toJson did)]
     | .object resource id => Json.mkObj [("resource", toJson resource), ("id", toJson id)]
     | .entitySet resource id targetRelation => Json.mkObj
       [("resource", toJson resource), ("id", toJson id), ("target_relation", toJson targetRelation)])]

def actionJson (state : State) (action : Action) : Json :=
  match action with
  | .grant relation => Json.mkObj [("action", toJson "grant"), ("relation", relationJson relation)]
  | .revoke relation => Json.mkObj [("action", toJson "revoke"), ("relation", relationJson relation)]
  | .grantAs actor relation => Json.mkObj [("action", toJson "grant_as"), ("actor", toJson actor),
      ("relation", relationJson relation), ("expected", toJson (mayManage state actor relation))]
  | .revokeAs actor relation => Json.mkObj [("action", toJson "revoke_as"), ("actor", toJson actor),
      ("relation", relationJson relation), ("expected", toJson (mayManage state actor relation))]
  | .check actor resource id permission => Json.mkObj
      [("action", toJson "check"), ("actor", toJson actor), ("resource", toJson resource),
       ("id", toJson id), ("permission", toJson permission),
       ("expected", toJson (allowed 5 state actor resource id permission))]

def traceJson (state : State) : List Action → List Json
  | [] => []
  | action :: rest => actionJson state action :: traceJson (apply state action) rest

end Identity.Household
