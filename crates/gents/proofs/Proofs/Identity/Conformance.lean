import Proofs.Identity.State
import Proofs.Identity.Permission
import Proofs.Identity.Properties
import Proofs.Identity.Household
import Proofs.Identity.Operations
import Proofs.Conformance.ContractCases

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

def agentCaseJson (c : AgentCase) : Json := Json.mkObj
  [("label", toJson c.label), ("did", toJson c.did), ("creation_nonce", toJson c.creationId),
   ("node", toJson c.node), ("enabled", toJson c.enabled)]

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

def identityStructuralCaseJson (c : IdentityStructuralCase) : Json := Json.mkObj
  [("name", toJson c.name), ("nodes", toJson c.nodes),
   ("agents", toJson (c.agents.map agentCaseJson)), ("well_formed", toJson c.wellFormed)]

def structuralCasesJson : String := (toJson (structuralCases.map identityStructuralCaseJson)).compress

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

def identityPermissionCaseJson (c : IdentityPermissionCase) : Json := Json.mkObj
  [("name", toJson c.name), ("nodes", toJson c.nodes), ("agents", toJson (c.agents.map agentCaseJson)),
   ("grants", toJson c.grants), ("permission", toJson c.permission), ("row_owner", toJson c.rowOwner),
   ("actor_node", toJson c.actorNode), ("actor_did", toJson c.actorDid),
   ("peer_node", toJson c.peerNode), ("peer_did", toJson c.peerDid),
   ("expected_actor_did", toJson (c.actor.map (·.did))),
   ("expected_peer_did", toJson (c.peer.map (·.did))),
   ("expected_actor_allowed", toJson (c.allowed c.actor)),
   ("expected_peer_allowed", toJson (c.allowed c.peer)),
   ("same_node", toJson (c.actor.any fun a => c.peer.any (fun b => a.node == b.node))),
   ("expected_decisions_equal", toJson (c.allowed c.actor == c.allowed c.peer))]

def identityPermissionCasesJson : String :=
  (toJson (identityPermissionCases.map identityPermissionCaseJson)).compress

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

def derivationInputJson (input : DerivationInput) : Json := Json.mkObj
  [("node_key", toJson input.nodeKey), ("creation_nonce", toJson input.creationNonce),
   ("label", toJson input.label)]

def derivationCasesJson : String := (toJson (derivationCases.map fun (name, left, right) =>
  Json.mkObj [("name", toJson name), ("left", derivationInputJson left),
    ("right", derivationInputJson right), ("expected_same_did", toJson (sameDerivedDid left right))])).compress

namespace HouseholdFixture
open Household

def actorRelation (resource id relation actor : String) : Relation :=
  ⟨resource, id, relation, .actor actor⟩

def initial : State :=
  commitUserSession
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
    enrolled := [userDid, readerDid, strangerDid] } "general-session" userDid agentDid "general" true

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

def selfConfigLookupCaseJson (name : String) (attempt : SelfConfigAttempt) : Json := Json.mkObj
  [("name", toJson name), ("present", toJson attempt.present), ("visible", toJson attempt.visible),
   ("same_generation", toJson attempt.sameGeneration), ("native_allows", toJson attempt.nativeAllows),
   ("expected", toJson (selfConfigOutcome attempt))]

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
    [true, true, false, false, true, false, true, false, false, true, true, false,
     true, true, true, false, true, false, false] := by native_decide

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

def policyRegistrations : List Json :=
  [("identical-policy", policy), ("management-metadata-collision", policyWithDifferentManagement),
    ("relation-type-collision", policyWithDifferentTypes)].map fun (name, candidate) =>
      let candidateYaml := policyYamlOf candidate
      Json.mkObj [("name", toJson name), ("candidate_yaml", toJson candidateYaml),
        ("same_id", toJson (policyIdInput policy == policyIdInput candidate)),
        ("accepted", toJson ((registerPolicy (some policyYaml) candidateYaml).isSome))]

def json : Json := Json.mkObj
  [("name", toJson "user-node-agents-mixed-session-ownership-policy-followup"), ("policy_yaml", toJson policyYaml),
   ("objects", toJson (initial.objects.map objectJson)),
   ("relations", toJson (initial.relations.map relationJson)), ("enrolled", toJson initial.enrolled),
   ("policy_registrations", toJson policyRegistrations),
   ("self_config_actions", toJson (traceJson initial selfConfigChanges)),
   ("self_config_lookup_cases", toJson (selfConfigLookupCases.map fun (name, attempt) =>
     selfConfigLookupCaseJson name attempt)),
   ("actions", toJson (traceJson initial (nodeAdministratorChanges ++ runtimeReaderChanges ++ invokeChanges ++ requestSessionChanges ++ receiptAuthorChanges ++ matrix ++ managementChanges ++ selfConfigChanges ++ changes)))]
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
  , .check agentDid "operations" "readiness" "read" ]

theorem detached_parent_grant_trace :
    HouseholdFixture.observed initial actions = [false, false, true, true, false, true, false] := by
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

def householdCasesJson : String := (toJson [HouseholdFixture.json, OperationsFixture.json]).compress

def identityContractsJson : String := (toJson [Json.mkObj
  [("name", toJson "identity.agent_owned_data"),
   ("statement", toJson "User keys stay on clients. Node keys serve P2P and own node-local operational data not caused by agents; operational data inherits user read access through Node. Agent work never falls back to runtime authority. Agent DIDs derive from immutable creation nonces and select all agent work. Users own Node configuration and transitively administer Agents. User-created sessions are user-owned; unattended sessions are agent-owned with configured user access and deletion. Agents own session children with parent-session user access and control. Full policy wiring remains follow-up."),
   ("node_local_collections", toJson nodeLocalCollections),
   ("replicated_protocol_collections", toJson replicatedProtocolCollections),
   ("schema_actor", toJson (schemaActor { did := runtimeDid, displayName := none, enabled := true } .createSchema)),
   ("runtime_actor", toJson runtimeDid),
   ("enforced", toJson false), ("tracked_by", toJson "#3485")]]).compress

end Identity.Conformance
