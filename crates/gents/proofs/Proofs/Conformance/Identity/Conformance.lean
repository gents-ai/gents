import Proofs.Store.Identity.State
import Proofs.Store.Identity.Permission
import Proofs.Store.Identity.Properties
import Proofs.Store.Identity.Household
import Proofs.Store.Identity.Automation
import Proofs.Store.Identity.UnattendedReceipt
import Proofs.Tools.Identity.Configuration
import Proofs.Tools.Identity.TaskTarget
import Proofs.Store.Identity.McpService
import Proofs.Store.Identity.Operations
import Proofs.Documents.Configuration.Affected
import Proofs.Conformance.ContractCases
import Proofs.Tools.ToolPolicy.Cases
import Proofs.Tools.SelfConfig.Auth

namespace Identity.Conformance
open Lean

def userDid : String := "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK"
def runtimeDid : String := "did:key:z6MkfXG2FkNy3u7Eg3jm8e2YQpGz7Z1JqWgHDAP1hLk9r2bR"
def otherRuntimeDid : String := "did:key:z6MkpTHR8VNsBxYAAWHut2Geadd9jSwuBV8xRoAnwWsdvktH"
def agentDid : String := "did:key:z6MkeXBLjYiSvqnhFb6D7sHm8yKm4jV45wwBFRaatf1cfZ76"
def siblingDid : String := "did:key:z6Mkeb6dsrBTX95vPgLiZAcgRr6XJthFm1czoqFEx34DQtRo"
def otherAgentDid : String := "did:key:z6Mkef1w29eU7SP9XmbDzTwbiisHZ3uTS5JpNEuu1R6pADkW"
def readerDid : String := "did:key:z6MkeiwEAT7UhjgNfrqjRmGX1be3oD7f78zdveaZ4o9QuZ5D"
def strangerDid : String := "did:key:z6MkenrXJkaVJ2ybox6Es4bSJUQp3NKrnCgTV4FD8BC1etPv"

def creationA : String := String.mk (List.replicate 64 'a')
def creationB : String := String.mk (List.replicate 64 'b')

structure NodeCase where
  did : String
  enabled : Bool
  deriving Repr, ToJson

structure AgentCase where
  label : String
  did : String
  creationId : String
  node : String
  enabled : Bool
  deriving Repr

def agentCaseToAgent (c : AgentCase) : Agent :=
  { label := c.label, did := c.did, creationId := c.creationId, node := c.node,
    displayName := none, enabled := c.enabled }

def localNode : NodeCase := ⟨runtimeDid, true⟩
def otherNode : NodeCase := ⟨otherRuntimeDid, true⟩
def selectedAgent : AgentCase := ⟨"general", agentDid, creationA, runtimeDid, true⟩
def siblingAgent : AgentCase := ⟨"general", siblingDid, creationB, runtimeDid, true⟩
def remoteAgent : AgentCase := ⟨"general", otherAgentDid, creationA, otherRuntimeDid, true⟩

structure IdentityStructuralCase where
  name : String
  nodes : List NodeCase
  agents : List AgentCase
  deriving Repr

def IdentityStructuralCase.world (c : IdentityStructuralCase) : World :=
  { nodes := (c.nodes.map (fun p =>
      ({ did := p.did, displayName := none, enabled := p.enabled } : Node))).toFinset,
    agents := (c.agents.map agentCaseToAgent).toFinset }

def IdentityStructuralCase.wellFormed (c : IdentityStructuralCase) : Bool :=
  decide c.world.WellFormed

def structuralCases : List IdentityStructuralCase :=
  [ ⟨"same-label-distinct-creations-allowed", [localNode], [selectedAgent, siblingAgent]⟩
  , ⟨"same-label-across-nodes-allowed", [localNode, otherNode], [selectedAgent, remoteAgent]⟩
  , ⟨"dangling-node-denied", [localNode], [remoteAgent]⟩
  , ⟨"duplicate-creation-denied", [localNode],
      [selectedAgent, { selectedAgent with enabled := false }]⟩
  , ⟨"renamed-label-preserves-identity", [localNode], [{ selectedAgent with label := "renamed" }]⟩ ]

theorem structural_scope_cases_pinned : structuralCases.map (·.wellFormed) =
    [true, true, false, false, true] := by native_decide

structure PermissionGrantCase where
  actor : String
  permission : String
  deriving Repr, ToJson

structure IdentityPermissionCase where
  name : String
  nodes : List NodeCase
  agents : List AgentCase
  grants : List PermissionGrantCase
  permission : String
  rowOwner : String
  actorNode : String
  actorDid : String
  peerNode : String
  peerDid : String
  deriving Repr

def grantStoreFromCases (grants : List PermissionGrantCase) : GrantStore String :=
  { granted := fun actor permission =>
      grants.any (fun grant => grant.actor == actor && grant.permission == permission) }

def IdentityPermissionCase.actor (c : IdentityPermissionCase) : Option Agent :=
  findAgent? (c.agents.map agentCaseToAgent) c.actorNode c.actorDid

def IdentityPermissionCase.peer (c : IdentityPermissionCase) : Option Agent :=
  findAgent? (c.agents.map agentCaseToAgent) c.peerNode c.peerDid

def IdentityPermissionCase.allowed (c : IdentityPermissionCase) (agent : Option Agent) : Bool :=
  selectedDecide (grantStoreFromCases c.grants) agent c.permission

def rowRead : String := "row:" ++ userDid ++ ":memory.read"

def permissionCase (name : String) (agents : List AgentCase) (grants : List String)
    (actorNode actorDid peerNode peerDid : String) : IdentityPermissionCase :=
  ⟨name, [localNode, otherNode], agents, grants.map (fun actor => ⟨actor, rowRead⟩),
    rowRead, userDid, actorNode, actorDid, peerNode, peerDid⟩

def identityPermissionCases : List IdentityPermissionCase :=
  [ permissionCase "node-grant-never-authorizes-agents" [selectedAgent, siblingAgent]
      [runtimeDid] runtimeDid agentDid runtimeDid siblingDid
  , permissionCase "same-node-agents-isolated" [selectedAgent, siblingAgent]
      [agentDid] runtimeDid agentDid runtimeDid siblingDid
  , permissionCase "explicit-grant-shares-data" [selectedAgent, siblingAgent]
      [agentDid, siblingDid] runtimeDid agentDid runtimeDid siblingDid
  , permissionCase "did-selects-among-identical-labels" [siblingAgent, selectedAgent]
      [agentDid] runtimeDid agentDid runtimeDid siblingDid
  , permissionCase "renamed-label-preserves-selected-actor"
      [{ selectedAgent with label := "renamed" }, siblingAgent]
      [agentDid] runtimeDid agentDid runtimeDid siblingDid
  , permissionCase "foreign-node-does-not-select-local-agent" [selectedAgent, remoteAgent]
      [agentDid] otherRuntimeDid agentDid runtimeDid agentDid
  , permissionCase "unknown-did-does-not-select-by-label" [selectedAgent, siblingAgent]
      [agentDid] runtimeDid strangerDid runtimeDid agentDid
  , permissionCase "ambiguous-did-does-not-select-first-row"
      [selectedAgent, { selectedAgent with enabled := false }]
      [agentDid] runtimeDid agentDid runtimeDid agentDid ]

theorem same_node_isolation_and_no_fallback_pinned :
    (identityPermissionCases.take 2).map (fun c => (c.allowed c.actor, c.allowed c.peer)) =
    [(false, false), (true, false)] := by native_decide

structure DerivationInput where
  nodeKey : String
  creationNonce : String
  label : String
  deriving Repr

def DerivationInput.key (input : DerivationInput) : NodeKey × CreationId :=
  (input.nodeKey, input.creationNonce)

/-- Equality in the injective key derivation model is equality of the secret-root and
creation identifier pair. Display labels are excluded from the derivation input. -/
def sameDerivedDid (left right : DerivationInput) : Bool := left.key == right.key

theorem sameDerivedDid_matches_derivation (derivation : KeyDerivation)
    (left right : DerivationInput) : sameDerivedDid left right = true ↔
      derivation.derive left.nodeKey left.creationNonce =
        derivation.derive right.nodeKey right.creationNonce := by
  simp only [sameDerivedDid, beq_iff_eq, DerivationInput.key]
  exact ⟨fun h => congrArg (fun input : NodeKey × CreationId =>
    derivation.derive input.1 input.2) h,
    fun h => @derivation.injective (left.nodeKey, left.creationNonce)
      (right.nodeKey, right.creationNonce) h⟩

def derivationCases : List (String × DerivationInput × DerivationInput) :=
  [ ("rename-preserves-did", ⟨"runtime-a", creationA, "before"⟩, ⟨"runtime-a", creationA, "after"⟩)
  , ("same-label-recreation-has-new-did", ⟨"runtime-a", creationA, "general"⟩,
      ⟨"runtime-a", creationB, "general"⟩)
  , ("different-root-has-new-did", ⟨"runtime-a", creationA, "general"⟩,
      ⟨"runtime-b", creationA, "general"⟩)
  , ("deliberate-restoration-preserves-did", ⟨"runtime-a", creationA, "restored-label"⟩,
      ⟨"runtime-a", creationA, "original-label"⟩)
  , ("reopen-preserves-did", ⟨"runtime-a", creationA, "general"⟩,
      ⟨"runtime-a", creationA, "general"⟩) ]

/-- A claimed Agent DID names an independent key or a derivation input. -/
inductive ClaimedDid where
  | key (name : String)
  | derived (nodeKey creation : String)

def ClaimedDid.render : ClaimedDid → String
  | .key name => "key:" ++ name
  | .derived nodeKey creation => "derived:" ++ nodeKey ++ ":" ++ creation

def ClaimedDid.json : ClaimedDid → Json
  | .key name => Json.mkObj [("key", toJson name)]
  | .derived nodeKey creation => Json.mkObj [("derived", Json.mkObj
      [("node_key", toJson nodeKey), ("creation_nonce", toJson creation)])]

def renderedDerivation (nodeKey creation : String) : String :=
  (ClaimedDid.derived nodeKey creation).render

def managedToolIdentityCases : List (Option ClaimedDid × String × Option String) :=
  [ (some (.derived "runtime" creationA), "runtime", some creationA)
  , (some (.key "user"), "runtime", some creationA)
  , (some (.derived "runtime" creationB), "runtime", some creationA)
  , (some (.derived "other-runtime" creationA), "runtime", some creationA)
  , (some (.key "missing"), "runtime", none)
  , (some (.key "malformed"), "runtime", some "not-a-nonce")
  , (none, "runtime", some creationA) ]


namespace HouseholdFixture
open Household

def actorRelation (resource id relation actor : String) : Relation :=
  ⟨resource, id, relation, .actor actor⟩

def initial : State :=
  Identity.Automation.commitTaskRequest (commitUserSession
  { objects := [⟨"nodes", "home", userDid⟩, ⟨"operations", "readiness", runtimeDid⟩, ⟨"agents", "general", userDid⟩, ⟨"agents", "sibling", userDid⟩,
      ⟨"agents", "wife", readerDid⟩, ⟨"sessions", "wife-session", otherAgentDid⟩,
      ⟨"sessions", "sibling-session", userDid⟩,
      ⟨"children", "general-message", agentDid⟩, ⟨"children", "sibling-message", siblingDid⟩,
      ⟨"agent_children", "callback", agentDid⟩,
      ⟨"agent_children", "callback-result", agentDid⟩,
      ⟨"delivered_requests", "remote-request", agentDid⟩,
      ⟨"goal_controls", "goal-command", userDid⟩,
      ⟨"goal_receipts", "goal-receipt", agentDid⟩,
      ⟨"sessions", "automated-session", agentDid⟩, ⟨"children", "automated-message", agentDid⟩,
      ⟨"sessions", "nested-session", agentDid⟩, ⟨"children", "nested-message", siblingDid⟩]
    relations := [⟨"operations", "readiness", "parent", .object "nodes" "home"⟩,
      actorRelation "nodes" "home" "runtime_reader" runtimeDid,
      ⟨"agents", "general", "parent", .object "nodes" "home"⟩,
      ⟨"agents", "sibling", "parent", .object "nodes" "home"⟩,
      ⟨"agents", "wife", "parent", .object "nodes" "home"⟩,
      ⟨"agent_children", "callback", "parent", .object "agents" "general"⟩,
      ⟨"agent_children", "callback-result", "parent", .object "agents" "general"⟩,
      ⟨"sessions", "wife-session", "parent", .object "agents" "wife"⟩,
      ⟨"sessions", "sibling-session", "parent", .object "agents" "sibling"⟩,
      ⟨"sessions", "automated-session", "parent", .object "agents" "general"⟩,
      ⟨"sessions", "nested-session", "parent", .object "agents" "sibling"⟩,
      actorRelation "sessions" "nested-session" "working_agent" siblingDid,
      ⟨"children", "nested-message", "parent", .object "sessions" "nested-session"⟩,
      ⟨"children", "automated-message", "parent", .object "sessions" "automated-session"⟩,
      actorRelation "agents" "general" "reader" readerDid,
      actorRelation "agents" "general" "executor" agentDid,
      actorRelation "agents" "sibling" "executor" siblingDid,
      actorRelation "agents" "wife" "executor" otherAgentDid,
      actorRelation "sessions" "sibling-session" "working_agent" siblingDid,
      actorRelation "delivered_requests" "remote-request" "working_agent" otherAgentDid,
      ⟨"delivered_requests", "remote-request", "parent", .object "agents" "wife"⟩,
      ⟨"goal_controls", "goal-command", "parent", .object "sessions" "general-session"⟩,
      actorRelation "goal_controls" "goal-command" "working_agent" agentDid,
      ⟨"goal_receipts", "goal-receipt", "parent", .object "goal_controls" "goal-command"⟩,
      actorRelation "goal_receipts" "goal-receipt" "reader" strangerDid,
      actorRelation "sessions" "general-session" "reader" readerDid,
      ⟨"children", "general-message", "parent", .object "sessions" "general-session"⟩,
      ⟨"children", "sibling-message", "parent", .object "sessions" "sibling-session"⟩]
    enrolled := [userDid, readerDid, strangerDid] } "general-session" userDid agentDid "general" true)
    "automated-request" agentDid agentDid "general" "automated-session" true true true

def nodeAdministratorChanges : List Action :=
  let grant := actorRelation "nodes" "home" "administrator" strangerDid
  [ .check strangerDid "nodes" "home" "update"
  , .grantAs runtimeDid grant
  , .check strangerDid "nodes" "home" "update"
  , .grantAs userDid grant
  , .check strangerDid "nodes" "home" "update"
  , .revokeAs userDid grant
  , .check strangerDid "nodes" "home" "update" ]

def managementChanges : List Action :=
  let agentReader := actorRelation "agents" "wife" "reader" strangerDid
  let sessionReader := actorRelation "sessions" "wife-session" "reader" strangerDid
  [ .grantAs strangerDid agentReader
  , .check strangerDid "agents" "wife" "read"
  , .grantAs userDid agentReader
  , .check strangerDid "agents" "wife" "read"
  , .grantAs readerDid (actorRelation "agents" "wife" "administrator" userDid)
  , .grantAs userDid agentReader
  , .check strangerDid "agents" "wife" "read"
  , .revokeAs userDid agentReader
  , .check strangerDid "agents" "wife" "read"
  , .grantAs strangerDid sessionReader
  , .check strangerDid "sessions" "wife-session" "read"
  , .grantAs userDid sessionReader
  , .check strangerDid "sessions" "wife-session" "read"
  , .grantAs otherAgentDid sessionReader
  , .check strangerDid "sessions" "wife-session" "read"
  , .revokeAs otherAgentDid sessionReader
  , .check strangerDid "sessions" "wife-session" "read" ]

/-- Enabling a self-config tool is not an ACP grant. The Agent's owner or
directly granted administrator explicitly grants and revokes its config_writer
relationship; the agent cannot grant or retain that relationship itself. -/
def selfConfigChanges : List Action :=
  let grant := actorRelation "agents" "general" "config_writer" agentDid
  [ .check agentDid "agents" "general" "update"
  , .grantAs agentDid grant
  , .check agentDid "agents" "general" "update"
  , .grantAs userDid grant
  , .check agentDid "agents" "general" "update"
  , .check siblingDid "agents" "general" "update"
  , .revokeAs agentDid grant
  , .check agentDid "agents" "general" "update"
  , .revokeAs userDid grant
  , .check agentDid "agents" "general" "update" ]

def runtimeReaderChanges : List Action :=
  let grant := actorRelation "nodes" "home" "runtime_reader" runtimeDid
  [ .check runtimeDid "nodes" "home" "read"
  , .check runtimeDid "agents" "general" "read"
  , .check runtimeDid "sessions" "general-session" "read"
  , .revokeAs userDid grant
  , .check runtimeDid "nodes" "home" "read"
  , .check runtimeDid "agents" "general" "read"
  , .grantAs userDid grant
  , .check runtimeDid "nodes" "home" "read"
  , .check runtimeDid "agents" "general" "read" ]

def selfConfigLookupCases : List (String × SelfConfigAttempt) :=
  [("absent", ⟨false, false, false, false⟩),
   ("hidden", ⟨true, false, true, true⟩),
   ("manager_denied", ⟨true, true, true, false⟩),
   ("manager_allowed", ⟨true, true, true, true⟩),
   ("stale_generation", ⟨true, true, false, true⟩)]

def matrix : List Action :=
  [userDid, runtimeDid, agentDid, siblingDid, otherAgentDid, readerDid, strangerDid].flatMap
    (fun actor => initial.objects.flatMap (fun object => ["read", "update", "delete"].map
      (fun permission => .check actor object.resource object.id permission)))

def invokeChanges : List Action :=
  [ .check readerDid "agents" "general" "invoke"
  , .check agentDid "agents" "general" "invoke"
  , .check userDid "agents" "general" "invoke"
  , .grant (actorRelation "agents" "general" "session_writer" readerDid)
  , .check readerDid "agents" "general" "invoke"
  , .revoke (actorRelation "agents" "general" "session_writer" readerDid)
  , .check readerDid "agents" "general" "invoke" ]

/-- Invoking a User-owned target does not expose its unrelated User-owned
sessions. The caller still reads the child session it created, and the target
reads its own working session. -/
def invokeOnlySessionActions : List Action :=
  [ .grant (actorRelation "agents" "sibling" "session_writer" agentDid)
  , .check agentDid "agents" "sibling" "read"
  , .check agentDid "agents" "sibling" "invoke"
  , .check agentDid "sessions" "sibling-session" "read"
  , .check agentDid "sessions" "nested-session" "read"
  , .check siblingDid "sessions" "sibling-session" "read"
  , .check userDid "sessions" "sibling-session" "read"
  , .check agentDid "sessions" "sibling-session" "update"
  , .grant (actorRelation "sessions" "sibling-session" "reader" agentDid)
  , .check agentDid "sessions" "sibling-session" "read"
  , .revoke (actorRelation "sessions" "sibling-session" "reader" agentDid)
  , .check agentDid "sessions" "sibling-session" "read"
  , .revoke (actorRelation "agents" "sibling" "session_writer" agentDid) ]

def requestSessionChanges : List Action :=
  [ .check strangerDid "delivered_requests" "remote-request" "read"
  , .grant (actorRelation "sessions" "wife-session" "reader" strangerDid)
  , .grant ⟨"delivered_requests", "remote-request", "session", .object "sessions" "wife-session"⟩
  , .check strangerDid "delivered_requests" "remote-request" "read"
  , .revoke (actorRelation "sessions" "wife-session" "reader" strangerDid)
  , .check strangerDid "delivered_requests" "remote-request" "read" ]

def receiptAuthorChanges : List Action :=
  [ .grant (actorRelation "sessions" "general-session" "reader" strangerDid)
  , .check strangerDid "goal_receipts" "goal-receipt" "read"
  , .revoke (actorRelation "sessions" "general-session" "reader" strangerDid)
  , .check strangerDid "goal_receipts" "goal-receipt" "read"
  , .check strangerDid "goal_receipts" "goal-receipt" "update" ]

def changes : List Action :=
  [ .revoke (actorRelation "sessions" "general-session" "reader" readerDid)
  , .check readerDid "children" "general-message" "read"
  , .grant (actorRelation "sessions" "general-session" "reader" strangerDid)
  , .check strangerDid "children" "general-message" "read"
  , .check strangerDid "children" "general-message" "update"
  , .revoke (actorRelation "sessions" "general-session" "reader" strangerDid)
  , .check strangerDid "children" "general-message" "read"
  , .check readerDid "sessions" "automated-session" "read"
  , .check readerDid "sessions" "automated-session" "update"
  , .grant (actorRelation "agents" "general" "session_writer" readerDid)
  , .check readerDid "sessions" "automated-session" "update"
  , .check readerDid "agents" "general" "update"
  , .check readerDid "agents" "sibling" "read"
  , .grant (actorRelation "agents" "general" "administrator" readerDid)
  , .check readerDid "agents" "general" "update"
  , .check readerDid "children" "automated-message" "update"
  , .revoke (actorRelation "agents" "general" "reader" readerDid)
  , .revoke (actorRelation "agents" "general" "session_writer" readerDid)
  , .revoke (actorRelation "agents" "general" "administrator" readerDid)
  , .check readerDid "children" "automated-message" "read"
  , .grant (actorRelation "nodes" "home" "administrator" strangerDid)
  , .check strangerDid "sessions" "sibling-session" "read"
  , .check strangerDid "delivered_requests" "remote-request" "read"
  , .check strangerDid "agents" "sibling" "update"
  , .check strangerDid "children" "automated-message" "read"
  , .check strangerDid "children" "automated-message" "update"
  , .check agentDid "agents" "general" "update"
  , .grant (actorRelation "agents" "general" "config_writer" agentDid)
  , .check agentDid "agents" "general" "update"
  , .check agentDid "agents" "general" "delete"
  , .check agentDid "agents" "sibling" "update" ]

def observed (state : State) : List Action → List Bool
  | [] => []
  | action :: rest => (check state action).toList ++ observed (apply state action) rest

theorem node_administrator_is_explicit_and_revocable :
    observed initial nodeAdministratorChanges = [false, false, true, false] := by native_decide

theorem runtime_reader_is_explicit_and_revocable :
    observed initial runtimeReaderChanges = [true, true, false, false, false, true, true] := by native_decide

theorem invoke_is_distinct_and_revocable :
    observed initial invokeChanges = [false, true, true, true, false] := by native_decide

theorem invoke_only_preserves_session_isolation :
    observed initial invokeOnlySessionActions =
      [true, true, false, true, true, true, false, true, false] := by native_decide

theorem request_session_reader_is_revocable :
    observed initial requestSessionChanges = [false, true, false] := by native_decide

theorem receipt_author_remains_a_reader_after_session_revocation :
    observed initial receiptAuthorChanges = [true, true, false] := by native_decide

theorem relationship_management_requires_owner_or_direct_manager :
    observed initial managementChanges = [false, false, true, false, false, false, true, false] := by native_decide

theorem self_config_grant_is_explicit_and_revocable :
    observed initial selfConfigChanges = [false, false, true, false, true, false] := by native_decide

theorem household_ownership_and_isolation :
    [allowed 5 initial userDid "agents" "general" "delete",
     allowed 5 initial userDid "sessions" "general-session" "delete",
     allowed 5 initial agentDid "children" "general-message" "delete",
     allowed 5 initial userDid "children" "general-message" "read",
     allowed 5 initial userDid "children" "general-message" "update",
     allowed 5 initial siblingDid "children" "general-message" "read",
     allowed 5 initial runtimeDid "children" "general-message" "read",
     allowed 5 initial strangerDid "children" "general-message" "read",
     allowed 5 initial agentDid "sessions" "general-session" "update",
     allowed 5 initial agentDid "sessions" "general-session" "delete"] =
    [true, true, true, true, true, false, false, false, true, false] := by native_decide

theorem household_revocation : observed initial changes =
    [true, true, false, false, true, false, false, false, false, true, true, false,
     true, true, true, true, true, false, true, false, false] := by native_decide

/-- Isolation between sibling Agents is a property of grants, not of Agents: an
Agent with no grant on a sibling reads none of its sessions or requests
(`invoke_only_preserves_session_isolation`,
`delivered_request_preserves_sender_and_worker`), while a Node `administrator`
reads every Agent's sessions and their requests. The initialised Engineer holds
`administrator` by decision (`SelfConfig.engineerInitRelations`), so it is not a
witness for sibling isolation; two Agents without grants are. -/
theorem node_administrator_reads_sibling_sessions_and_requests :
    [allowed 5 initial siblingDid "sessions" "general-session" "read",
     allowed 5 initial agentDid "sessions" "sibling-session" "read",
     allowed 5 initial siblingDid "delivered_requests" "remote-request" "read",
     allowed 5 (apply initial (.grant (actorRelation "nodes" "home" "administrator" strangerDid)))
       strangerDid "sessions" "sibling-session" "read",
     allowed 5 (apply initial (.grant (actorRelation "nodes" "home" "administrator" strangerDid)))
       strangerDid "delivered_requests" "remote-request" "read"] =
    [false, false, false, true, true] := by native_decide

theorem operations_follow_cause_and_preserve_user_visibility :
    [allowed 5 initial runtimeDid "operations" "readiness" "update",
     allowed 5 initial userDid "operations" "readiness" "read",
     allowed 5 initial userDid "operations" "readiness" "update",
     allowed 5 initial readerDid "operations" "readiness" "read",
     allowed 5 initial agentDid "operations" "readiness" "update"] =
    [true, true, false, false, false] := by native_decide

theorem unattended_sessions_preserve_agent_ownership_and_user_access :
    [allowed 5 initial agentDid "sessions" "automated-session" "delete",
     allowed 5 initial userDid "sessions" "automated-session" "read",
     allowed 5 initial userDid "sessions" "automated-session" "update",
     allowed 5 initial userDid "children" "automated-message" "read",
     allowed 5 initial userDid "children" "automated-message" "update",
     allowed 5 initial userDid "sessions" "automated-session" "delete",
     allowed 5 initial userDid "children" "automated-message" "delete",
     allowed 5 initial runtimeDid "sessions" "automated-session" "read",
     allowed 5 initial siblingDid "sessions" "automated-session" "read"] =
    [true, true, true, true, true, true, true, false, false] := by native_decide

theorem unattended_request_preserves_agent_ownership_and_user_access :
    [allowed 5 initial agentDid "delivered_requests" "automated-request" "read",
     allowed 5 initial userDid "delivered_requests" "automated-request" "read",
     allowed 5 initial readerDid "delivered_requests" "automated-request" "read",
     allowed 5 initial runtimeDid "delivered_requests" "automated-request" "read",
     allowed 5 initial siblingDid "delivered_requests" "automated-request" "read",
     allowed 5 initial strangerDid "delivered_requests" "automated-request" "read"] =
    [true, true, true, false, false, false] := by native_decide

theorem unattended_checkpoint_uses_request_visibility :
    [Identity.Automation.checkpointTaskArrival initial runtimeDid "automated-request" true true true 0 1,
     Identity.Automation.checkpointTaskArrival initial agentDid "automated-request" true true true 0 1,
     Identity.Automation.checkpointTaskArrival initial agentDid "automated-request" true true true 1 1] =
    [0, 1, 1] := by native_decide

theorem agent_children_inherit_only_their_agent_household :
    [allowed 5 initial agentDid "agent_children" "callback" "delete",
     allowed 5 initial userDid "agent_children" "callback" "read",
     allowed 5 initial userDid "agent_children" "callback" "update",
     allowed 5 initial userDid "agent_children" "callback-result" "delete",
     allowed 5 initial readerDid "agent_children" "callback" "read",
     allowed 5 initial readerDid "agent_children" "callback" "update",
     allowed 5 initial siblingDid "agent_children" "callback" "read",
     allowed 5 initial runtimeDid "agent_children" "callback" "read",
     allowed 5 initial strangerDid "agent_children" "callback-result" "read"] =
    [true, true, true, true, true, false, false, false, false] := by native_decide

theorem goal_receipt_writer_and_command_author_are_distinct :
    [allowed 5 initial agentDid "goal_receipts" "goal-receipt" "update",
     allowed 5 initial agentDid "goal_receipts" "goal-receipt" "delete",
     allowed 5 initial userDid "goal_receipts" "goal-receipt" "read",
     allowed 5 initial userDid "goal_receipts" "goal-receipt" "update",
     allowed 5 initial userDid "goal_receipts" "goal-receipt" "delete",
     allowed 5 initial readerDid "goal_receipts" "goal-receipt" "read",
     allowed 5 initial readerDid "goal_receipts" "goal-receipt" "update",
     allowed 5 initial runtimeDid "goal_receipts" "goal-receipt" "read",
     allowed 5 initial siblingDid "goal_receipts" "goal-receipt" "read",
     allowed 5 initial strangerDid "goal_receipts" "goal-receipt" "read"] =
    [true, true, true, false, false, true, false, false, false, true] := by native_decide

theorem nested_sessions_preserve_user_control :
    [allowed 5 initial userDid "sessions" "nested-session" "delete",
     allowed 5 initial userDid "children" "nested-message" "update",
     allowed 5 initial userDid "children" "nested-message" "delete",
     allowed 5 initial siblingDid "sessions" "nested-session" "update",
     allowed 5 initial siblingDid "sessions" "nested-session" "delete"] =
    [true, true, true, true, false] := by native_decide

/-- A delivered request belongs to its signing sender; the receiving agent's
working relationship permits execution without transferring ownership. Native
collection ACP separately controls whether the sender can create it at all. -/
theorem delivered_request_preserves_sender_and_worker :
    [allowed 5 initial agentDid "delivered_requests" "remote-request" "delete",
     allowed 5 initial otherAgentDid "delivered_requests" "remote-request" "update",
     allowed 5 initial otherAgentDid "delivered_requests" "remote-request" "delete",
     allowed 5 initial siblingDid "delivered_requests" "remote-request" "read",
     allowed 5 initial runtimeDid "delivered_requests" "remote-request" "read"] =
    [true, true, false, false, false] := by native_decide

theorem target_household_controls_delivered_request :
    [allowed 5 initial userDid "delivered_requests" "remote-request" "read",
     allowed 5 initial userDid "delivered_requests" "remote-request" "delete",
     allowed 5 initial readerDid "agents" "general" "invoke",
     allowed 5 initial agentDid "agents" "general" "invoke"] =
    [true, true, false, true] := by native_decide

def json : Json := Json.mkObj
  [("name", toJson "user-node-agents-mixed-session-ownership-policy-followup"), ("policy_yaml", toJson policyYaml),
   ("objects", toJson (initial.objects.map objectJson)),
   ("relations", toJson (initial.relations.map relationJson)), ("enrolled", toJson initial.enrolled),
   ("policy_registrations", toJson ([] : List Json)),
   ("self_config_actions", toJson ([] : List Json)),
   ("self_config_lookup_cases", toJson ([] : List Json)),
   ("actions", toJson (traceJson initial (nodeAdministratorChanges ++ runtimeReaderChanges ++ invokeChanges ++ invokeOnlySessionActions ++ requestSessionChanges ++ receiptAuthorChanges ++ matrix ++ managementChanges ++ selfConfigChanges ++ changes)))]
end HouseholdFixture

namespace OperationsFixture
open Household

def initial : State := Operations.beforeParent userDid runtimeDid readerDid

def actions : List Action :=
  [ .check userDid "operations" "readiness" "read"
  , .check readerDid "operations" "readiness" "read"
  , .grantAs runtimeDid ⟨"operations", "readiness", "parent", .object "nodes" "home"⟩
  , .check userDid "operations" "readiness" "read"
  , .check readerDid "operations" "readiness" "read"
  , .check userDid "operations" "readiness" "update"
  , .check runtimeDid "operations" "readiness" "update"
  , .check agentDid "operations" "readiness" "read"
  , .check siblingDid "operations" "readiness" "read"
  , .check otherAgentDid "operations" "readiness" "update"
  , .revokeAs runtimeDid ⟨"operations", "readiness", "parent", .object "nodes" "home"⟩
  , .check userDid "operations" "readiness" "read"
  , .check runtimeDid "operations" "readiness" "update"
  , .check otherAgentDid "operations" "readiness" "read" ]

theorem detached_parent_grant_trace :
    HouseholdFixture.observed initial actions = [false, false, true, true, false, true, false, false, false, false, true, false] := by
  native_decide

def json : Json := Json.mkObj
  [("name", toJson "operations-parent-publication"), ("policy_yaml", toJson policyYaml),
   ("objects", toJson (initial.objects.map objectJson)),
   ("relations", toJson (initial.relations.map relationJson)), ("enrolled", toJson initial.enrolled),
   ("policy_registrations", toJson ([] : List Json)),
   ("self_config_actions", toJson ([] : List Json)),
   ("self_config_lookup_cases", toJson ([] : List Json)),
   ("actions", toJson (traceJson initial actions))]
end OperationsFixture

namespace ConfigurationFixture
open Household

def initial : State :=
  { objects := [⟨"nodes", "home", userDid⟩, ⟨"configuration", "selected", userDid⟩,
      ⟨"configuration", "agent-created", agentDid⟩],
    relations := [⟨"nodes", "home", "runtime_reader", .actor runtimeDid⟩,
      ⟨"configuration", "selected", "parent", .object "nodes" "home"⟩,
      ⟨"configuration", "agent-created", "parent", .object "nodes" "home"⟩],
    enrolled := [] }

def checks (state : State) : List Json :=
  [userDid, agentDid, siblingDid, runtimeDid].flatMap fun actor =>
    ["selected", "agent-created"].flatMap fun id =>
      ["read", "update", "delete"].map fun operation => Json.mkObj
        [("action", toJson "check"), ("actor", toJson actor),
         ("resource", toJson "configuration"), ("id", toJson id),
         ("permission", toJson operation),
         ("expected", toJson (Household.allowed 5 state actor "configuration" id operation))]

def reader : Relation := ⟨"configuration", "selected", "reader", .actor agentDid⟩
def writer : Relation := ⟨"configuration", "selected", "writer", .actor agentDid⟩
def withReader : State := apply initial (.grant reader)
def withWriter : State := apply withReader (.grant writer)
def withoutWriter : State := apply withWriter (.revoke writer)
def withoutReader : State := apply withoutWriter (.revoke reader)

def atomicInitial : State :=
  { objects := [⟨"nodes", "home", userDid⟩], relations := [], enrolled := [] }

def providerInitial : State :=
  { objects := [⟨"agents", "agent-doc", userDid⟩,
      ⟨"configuration", "context-doc", userDid⟩,
      ⟨"configuration", "tools-doc", userDid⟩],
    relations := [⟨"agents", "agent-doc", "executor", .actor agentDid⟩,
      ⟨"configuration", "context-doc", "reader", .actor agentDid⟩,
      ⟨"configuration", "tools-doc", "reader", .actor agentDid⟩], enrolled := [] }

def eventConfigurationState : State :=
  { objects := [⟨"nodes", "home", userDid⟩,
      ⟨"configuration", "trigger", userDid⟩,
      ⟨"configuration", "source", userDid⟩],
    relations := [⟨"nodes", "home", "runtime_reader", .actor runtimeDid⟩,
      ⟨"configuration", "trigger", "parent", .object "nodes" "home"⟩,
      ⟨"configuration", "source", "parent", .object "nodes" "home"⟩], enrolled := [] }





def datastoreToolUseCaseJson (name : String)
    (visible enabled selected declarationMatches : Bool) : Json :=
  Json.mkObj [("name", toJson name), ("visible", toJson visible),
    ("enabled", toJson enabled), ("selected", toJson selected),
    ("declaration_matches", toJson declarationMatches),
    ("expected", toJson (Configuration.cachedDatastoreToolUsable
      visible enabled selected declarationMatches))]


def toolSource : Configuration.ToolGrantSource := ⟨"tools-doc", agentDid, "schema", "patch-collection"⟩
def otherToolSource : Configuration.ToolGrantSource := ⟨"other-tools-doc", agentDid, "schema", "patch-collection"⟩
def otherAgentSource : Configuration.ToolGrantSource := ⟨"tools-doc", siblingDid, "schema", "patch-collection"⟩
def otherPurposeSource : Configuration.ToolGrantSource := ⟨"tools-doc", agentDid, "other", "patch-collection"⟩

def toolSourceJson (source : Configuration.ToolGrantSource) : Json := Json.mkObj
  [("tools_doc", toJson source.toolsDoc), ("agent", toJson source.agent),
   ("purpose", toJson source.purpose), ("permission", toJson source.permission)]

def schemaToolSources : List Configuration.ToolGrantSource :=
  Configuration.schemaToolPermissions.map fun permission =>
    ⟨"tools-doc", agentDid, "schema", permission⟩

def sharedToolGroup : Configuration.ToolGroup := ⟨"tools-doc", "schema"⟩
def nextToolGroup : Configuration.ToolGroup := ⟨"next-tools-doc", "schema"⟩

def sharedToolHierarchy : Configuration.ToolGrantHierarchy :=
  { permissions := Configuration.schemaToolPermissions.map fun permission => ⟨sharedToolGroup, permission⟩,
    contexts := [⟨sharedToolGroup, "context-doc"⟩],
    agents := [⟨"context-doc", agentDid⟩, ⟨"context-doc", siblingDid⟩] }

theorem shared_tool_edges_revoke_all_members_without_scanning :
    let disabled := Configuration.setToolHierarchy sharedToolHierarchy sharedToolGroup
      Configuration.schemaToolPermissions false true true
    let active := Configuration.flattenToolHierarchy sharedToolHierarchy
    let inactive := Configuration.flattenToolHierarchy disabled
    [Configuration.toolGrantEffective active agentDid "patch-collection",
     Configuration.toolGrantEffective active siblingDid "patch-collection",
     Configuration.toolGrantEffective inactive agentDid "patch-collection",
     Configuration.toolGrantEffective inactive siblingDid "patch-collection"] =
       [true, true, false, false] := by native_decide

theorem shared_context_rebinding_and_individual_rebinding_are_scoped :
    let movedContext := Configuration.flattenToolHierarchy
      (Configuration.rebindToolContext sharedToolHierarchy "context-doc" nextToolGroup true true)
    let movedAgent := Configuration.flattenToolHierarchy
      (Configuration.rebindToolAgent sharedToolHierarchy agentDid "next-context-doc" true true)
    [Configuration.toolGrantEffective movedContext agentDid "patch-collection",
     Configuration.toolGrantEffective movedContext siblingDid "patch-collection",
     Configuration.toolGrantEffective movedAgent agentDid "patch-collection",
     Configuration.toolGrantEffective movedAgent siblingDid "patch-collection"] =
       [false, false, false, true] := by native_decide

def sessionToolSource : Configuration.ToolGrantSource :=
  ⟨"tools-doc", agentDid, "session-history", "read-document"⟩

def sessionToolHousehold : State :=
  { objects := [⟨"sessions", "owned", agentDid⟩, ⟨"sessions", "shared", userDid⟩,
      ⟨"sessions", "foreign", siblingDid⟩],
    relations := [⟨"sessions", "shared", "reader", .actor agentDid⟩], enrolled := [] }

theorem session_tool_still_requires_session_access :
    let grants : Configuration.ToolGrantProjection :=
      { enabled := [sessionToolSource], grants := [sessionToolSource] }
    [Configuration.toolSessionReadAllowed grants sessionToolHousehold agentDid "owned" false,
     Configuration.toolSessionReadAllowed grants sessionToolHousehold agentDid "shared" false,
     Configuration.toolSessionReadAllowed grants sessionToolHousehold agentDid "foreign" false,
     Configuration.toolSessionReadAllowed {} sessionToolHousehold agentDid "owned" false] =
       [true, true, false, false] := by native_decide

def toolGrantorHousehold : State :=
  { objects := [⟨"configuration", "tools-doc", userDid⟩],
    relations := [⟨"configuration", "tools-doc", "writer", .actor agentDid⟩], enrolled := [] }


theorem tools_writer_cannot_grant_itself_schema_authority :
    let initial : Configuration.ToolGrantProjection := { admins := [userDid] }
    (Household.allowed 5 toolGrantorHousehold agentDid "configuration" "tools-doc" "update" ∧
      Configuration.configureToolGrantGroup initial toolGrantorHousehold schemaToolSources agentDid true true = initial) := by
  native_decide

theorem native_admin_can_grant_schema_atomically :
    let initial : Configuration.ToolGrantProjection := { admins := [userDid] }
    let result := Configuration.configureToolGrantGroup initial toolGrantorHousehold schemaToolSources userDid true true
    result.enabled = schemaToolSources ∧ result.grants = schemaToolSources ∧
      Configuration.configureToolGrantGroup initial toolGrantorHousehold schemaToolSources userDid true false = initial := by
  native_decide

/-- The tool sources one Tools edit enables, as `toolOperationSources` derives
them for the configuring Agent. -/
def editToolSources (surface : ToolPolicy.Surface) (pack : Bool) : List Configuration.ToolGrantSource :=
  (Configuration.toolOperationSources surface pack).map fun (purpose, permission) =>
    ⟨"tools-doc", agentDid, purpose, permission⟩

def selfConfigPackSurface : ToolPolicy.Surface :=
  { ToolPolicy.ContractCases.secureMinimal with selfConfig := true, selfConfigCategories := .all }

/-- One edit selects one purpose's tool group; a surface enabling several
purposes is several group edits. -/
def editToolGroups (surface : ToolPolicy.Surface) (pack : Bool) :
    List (List Configuration.ToolGrantSource) :=
  let sources := editToolSources surface pack
  (sources.map (·.purpose)).eraseDups.map fun purpose => sources.filter (·.purpose == purpose)

theorem own_only_grant_cannot_enable_tool_sources :
    let initial : Configuration.ToolGrantProjection :=
      { admins := [userDid],
        direct := GrantSets.GrantSet.agentBaseline.permissions.map (agentDid, ·) }
    [ToolPolicy.ContractCases.agentGoalOnly, selfConfigPackSurface].all fun surface =>
      !(editToolGroups surface true).isEmpty &&
        (editToolGroups surface true).all fun group =>
          Configuration.validToolGrantGroup group &&
            Configuration.configureToolGrantGroup initial toolGrantorHousehold group agentDid true true ==
              initial := by
  native_decide

def callbackState : State :=
  { objects := [⟨"nodes", "home", userDid⟩,
      ⟨"configuration", "binding", userDid⟩,
      ⟨"configuration", "callback", userDid⟩,
      ⟨"configuration", "module", userDid⟩],
    relations := [⟨"nodes", "home", "runtime_reader", .actor runtimeDid⟩,
      ⟨"configuration", "binding", "parent", .object "nodes" "home"⟩,
      ⟨"configuration", "callback", "parent", .object "nodes" "home"⟩,
      ⟨"configuration", "module", "parent", .object "nodes" "home"⟩], enrolled := [] }

def backendCredentialCaseJson (name : String) (access : Configuration.BackendCredentialAccess)
    (currentSecret historicalSecret : Bool) : Json :=
  Json.mkObj [("name", toJson name),
    ("profile_required", toJson access.profileRequired),
    ("profile_visible", toJson access.profileVisible),
    ("backend_visible", toJson access.backendVisible),
    ("metadata_shareable", toJson (Configuration.backendMetadataShareable currentSecret historicalSecret)),
    ("runtime_actor", toJson access.runtimeActor),
    ("secret_readable", toJson access.secretReadable),
    ("same_node", toJson access.sameNode),
    ("selected_backend", toJson access.selectedBackend),
    ("same_credential", toJson access.sameCredential),
    ("destination_approved", toJson access.destinationApproved),
    ("credential_version_matches", toJson access.credentialVersionMatches),
    ("current_secret", toJson currentSecret), ("historical_secret", toJson historicalSecret),
    ("may_share", toJson (Configuration.backendMetadataShareable currentSecret historicalSecret)),
    ("may_use", toJson (Configuration.runtimeMayUseBackendCredential access currentSecret historicalSecret))]

def backendCredentialCases : List Json := Id.run do
  let allowed : Configuration.BackendCredentialAccess := ⟨true, true, true, true, true, true, true, true, true, true⟩
  return [backendCredentialCaseJson "authorized" allowed false false,
    backendCredentialCaseJson "background-discovery" { allowed with profileRequired := false, profileVisible := false } false false,
    backendCredentialCaseJson "profile-hidden" { allowed with profileVisible := false } false false,
    backendCredentialCaseJson "backend-hidden" { allowed with backendVisible := false } false false,
    backendCredentialCaseJson "agent-is-not-runtime" { allowed with runtimeActor := false } false false,
    backendCredentialCaseJson "runtime-secret-revoked" { allowed with secretReadable := false } false false,
    backendCredentialCaseJson "foreign-node" { allowed with sameNode := false } false false,
    backendCredentialCaseJson "wrong-backend" { allowed with selectedBackend := false } false false,
    backendCredentialCaseJson "wrong-credential" { allowed with sameCredential := false } false false,
    backendCredentialCaseJson "unapproved-destination" { allowed with destinationApproved := false } false false,
    backendCredentialCaseJson "rotated-credential" { allowed with credentialVersionMatches := false } false false,
    backendCredentialCaseJson "inline-secret" allowed true true,
    backendCredentialCaseJson "redacted-history" allowed false true]




def json : Json := Json.mkObj
  [("name", toJson "configuration-explicit-grants"),
   ("backend_credential_cases", toJson backendCredentialCases),
   ("policy_yaml", toJson policyYaml),
   ("objects", toJson (initial.objects.map objectJson)),
   ("relations", toJson (initial.relations.map relationJson)), ("enrolled", toJson ([] : List String)),
   ("policy_registrations", toJson ([] : List Json)),
   ("self_config_actions", toJson ([] : List Json)),
   ("self_config_lookup_cases", toJson ([] : List Json)),
   ("configuration_datastore_tool_use_cases", toJson
      [datastoreToolUseCaseJson "allowed" true true true true,
       datastoreToolUseCaseJson "revoked" false true true true,
       datastoreToolUseCaseJson "disabled" true false true true,
       datastoreToolUseCaseJson "deselected" true true false true,
       datastoreToolUseCaseJson "changed" true true true false]),
   ("actions", toJson (checks initial ++ [actionJson initial (.grant reader)] ++
      checks withReader ++ [actionJson withReader (.grant writer)] ++ checks withWriter ++
      [actionJson withWriter (.revoke writer)] ++ checks withoutWriter ++
      [actionJson withoutWriter (.revoke reader)] ++ checks withoutReader))]
end ConfigurationFixture

namespace SecretConfigurationFixture
open Household

def initial : State :=
  { objects := [⟨"nodes", "node", userDid⟩, ⟨"secret_configuration", "secret", userDid⟩],
    relations := [⟨"secret_configuration", "secret", "parent", .object "nodes" "node"⟩,
      ⟨"secret_configuration", "secret", "runtime_writer", .actor runtimeDid⟩], enrolled := [] }

def actions : List Action :=
  [ .check userDid "secret_configuration" "secret" "read"
  , .check runtimeDid "secret_configuration" "secret" "read"
  , .check runtimeDid "secret_configuration" "secret" "update"
  , .check runtimeDid "secret_configuration" "secret" "delete"
  , .check agentDid "secret_configuration" "secret" "read"
  , .check siblingDid "secret_configuration" "secret" "read"
  , .check otherAgentDid "secret_configuration" "secret" "update" ]

theorem access_trace : HouseholdFixture.observed initial actions =
    [true, true, true, false, false, false, false] := by native_decide

def json : Json := Json.mkObj
  [("name", toJson "secret-configuration-publication"), ("policy_yaml", toJson policyYaml),
   ("objects", toJson (initial.objects.map objectJson)),
   ("relations", toJson (initial.relations.map relationJson)), ("enrolled", toJson initial.enrolled),
   ("policy_registrations", toJson ([] : List Json)),
   ("self_config_actions", toJson ([] : List Json)),
   ("self_config_lookup_cases", toJson ([] : List Json)),
   ("actions", toJson (traceJson initial actions))]
end SecretConfigurationFixture

def householdCasesJson : String := (toJson [HouseholdFixture.json, OperationsFixture.json,
  ConfigurationFixture.json, SecretConfigurationFixture.json]).compress

end Identity.Conformance
