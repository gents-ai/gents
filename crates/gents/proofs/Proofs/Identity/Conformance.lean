import Proofs.Identity.State
import Proofs.Identity.Permission
import Proofs.Identity.Properties
import Proofs.Identity.Household
import Proofs.Identity.Automation
import Proofs.Identity.UnattendedReceipt
import Proofs.Identity.Configuration
import Proofs.Identity.TaskTarget
import Proofs.Identity.McpService
import Proofs.Identity.Operations
import Proofs.Configuration.Affected
import Proofs.Conformance.ContractCases
import Proofs.ToolPolicy.Cases
import Proofs.SelfConfig.Auth

namespace Identity.Conformance
open Lean

def taskSessionPublicationCasesJson : String :=
  let empty : Automation.TaskSessionPublication := ⟨false, false, false, false⟩
  let revoked : Automation.TaskSessionPublication := ⟨true, true, true, false⟩
  let cases := [("aborted", empty, true, true, false),
    ("created", empty, true, true, true),
    ("duplicate-after-revocation", revoked, false, false, true)]
  (toJson (cases.map fun (name, before, fresh, newSession, committed) =>
    let result := Automation.publishTaskSession before fresh newSession committed
    Json.mkObj [("name", toJson name), ("fresh", toJson fresh),
      ("new_session", toJson newSession), ("committed", toJson committed),
      ("request", toJson result.request), ("session", toJson result.session),
      ("parent", toJson result.parent), ("working_agent", toJson result.workingAgent)])).compress

def userDid : String := "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK"
def runtimeDid : String := "did:key:z6MkfXG2FkNy3u7Eg3jm8e2YQpGz7Z1JqWgHDAP1hLk9r2bR"
def otherRuntimeDid : String := "did:key:z6MkpTHR8VNsBxYAAWHut2Geadd9jSwuBV8xRoAnwWsdvktH"
def agentDid : String := "did:key:z6MkeXBLjYiSvqnhFb6D7sHm8yKm4jV45wwBFRaatf1cfZ76"
def siblingDid : String := "did:key:z6Mkeb6dsrBTX95vPgLiZAcgRr6XJthFm1czoqFEx34DQtRo"
def otherAgentDid : String := "did:key:z6Mkef1w29eU7SP9XmbDzTwbiisHZ3uTS5JpNEuu1R6pADkW"
def readerDid : String := "did:key:z6MkeiwEAT7UhjgNfrqjRmGX1be3oD7f78zdveaZ4o9QuZ5D"
def strangerDid : String := "did:key:z6MkenrXJkaVJ2ybox6Es4bSJUQp3NKrnCgTV4FD8BC1etPv"

def clientSessionReservationCasesJson : String :=
  let empty : Household.State := ⟨[], [], []⟩
  let parent : Household.Relation := ⟨"sessions", "session", "parent", .object "agents" "agent"⟩
  let worker : Household.Relation := ⟨"sessions", "session", "working_agent", .actor agentDid⟩
  let revoked : Household.State := ⟨[⟨"sessions", "session", userDid⟩], [parent], []⟩
  let cases := [("aborted", empty, true, false), ("created", empty, true, true),
    ("duplicate-after-revocation", revoked, false, true)]
  (toJson (cases.map fun (name, before, fresh, commit) =>
    let result := Household.reserveUserSession before "session" userDid agentDid "agent" fresh commit
    Json.mkObj [("name", toJson name), ("fresh", toJson fresh), ("commit", toJson commit),
      ("session", toJson (result.objects.any (fun o => o.resource == "sessions" && o.id == "session"))),
      ("parent", toJson (result.relations.contains parent)),
      ("working_agent", toJson (result.relations.contains worker))])).compress
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

def policyRegistrations : List Json :=
  [("identical-policy", policy), ("management-metadata-collision", policyWithDifferentManagement),
    ("relation-type-collision", policyWithDifferentTypes)].map fun (name, candidate) =>
      let candidateYaml := policyYamlOf candidate
      Json.mkObj [("name", toJson name), ("candidate_yaml", toJson candidateYaml),
        ("same_id", toJson (policyIdInput policy == policyIdInput candidate)),
        ("accepted", toJson ((registerPolicy (some policyYaml) candidateYaml).isSome))]

def taskCheckpointCases : List Json :=
  [(runtimeDid, 0, 1, true), (agentDid, 0, 1, true), (agentDid, 1, 1, true),
    (agentDid, 1, 2, false), (agentDid, 1, 2, true)].map fun (actor, cursor, through, receipt) =>
    Json.mkObj [("actor", toJson actor), ("cursor", toJson cursor), ("through", toJson through), ("receipt", toJson receipt),
      ("expected", toJson (Identity.Automation.checkpointTaskArrival initial actor "automated-request"
        receipt true true cursor through))]

def json : Json := Json.mkObj
  [("name", toJson "user-node-agents-mixed-session-ownership-policy-followup"), ("policy_yaml", toJson policyYaml),
   ("objects", toJson (initial.objects.map objectJson)),
   ("relations", toJson (initial.relations.map relationJson)), ("enrolled", toJson initial.enrolled),
   ("policy_registrations", toJson policyRegistrations),
   ("task_checkpoint_cases", toJson taskCheckpointCases),
   ("self_config_actions", toJson (traceJson initial selfConfigChanges)),
   ("invoke_session_actions", toJson (traceJson initial invokeOnlySessionActions)),
   ("self_config_lookup_cases", toJson (selfConfigLookupCases.map fun (name, attempt) =>
     selfConfigLookupCaseJson name attempt)),
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

def atomicCaseJson (commit : Bool) : Json :=
  let state := Configuration.publishSelectedAtomic atomicInitial userDid "atomic" "home" agentDid commit
  Json.mkObj [("commit", toJson commit),
    ("owner_read", toJson (Household.allowed 5 state userDid "configuration" "atomic" "read")),
    ("agent_read", toJson (Household.allowed 5 state agentDid "configuration" "atomic" "read")),
    ("sibling_read", toJson (Household.allowed 5 state siblingDid "configuration" "atomic" "read"))]

def bindingCaseJson (sameNode hasDoc visible : Bool) : Json :=
  let observed := if sameNode then "node" else "other"
  let doc := if hasDoc then "doc" else ""
  Json.mkObj [("same_node", toJson sameNode), ("has_doc", toJson hasDoc),
    ("visible", toJson visible),
    ("expected", toJson (Configuration.usableNodeBinding "node" observed doc "node" visible))]

def providerInitial : State :=
  { objects := [⟨"agents", "agent-doc", userDid⟩,
      ⟨"configuration", "context-doc", userDid⟩,
      ⟨"configuration", "tools-doc", userDid⟩],
    relations := [⟨"agents", "agent-doc", "executor", .actor agentDid⟩,
      ⟨"configuration", "context-doc", "reader", .actor agentDid⟩,
      ⟨"configuration", "tools-doc", "reader", .actor agentDid⟩], enrolled := [] }

def providerCaseJson (name actor purpose : String) (revokeTools missingContext : Bool) : Json :=
  let state := if revokeTools then apply providerInitial
    (.revoke ⟨"configuration", "tools-doc", "reader", .actor agentDid⟩) else providerInitial
  let documents := [("agents", "agent-doc"),
    ("configuration", if missingContext then "replaced-context" else "context-doc"),
    ("configuration", "tools-doc")]
  Json.mkObj [("name", toJson name), ("actor", toJson actor), ("purpose", toJson purpose),
    ("revoke_tools", toJson revokeTools), ("missing_context", toJson missingContext),
    ("expected", toJson (Configuration.providerAssemblyAllowed state actor userDid documents))]

def eventConfigurationState : State :=
  { objects := [⟨"nodes", "home", userDid⟩,
      ⟨"configuration", "trigger", userDid⟩,
      ⟨"configuration", "source", userDid⟩],
    relations := [⟨"nodes", "home", "runtime_reader", .actor runtimeDid⟩,
      ⟨"configuration", "trigger", "parent", .object "nodes" "home"⟩,
      ⟨"configuration", "source", "parent", .object "nodes" "home"⟩], enrolled := [] }

def eventConfigurationCaseJson (actor resource operation : String) (revoked : Bool) : Json :=
  let granted := Automation.publishEventConfiguration eventConfigurationState userDid "trigger" "source" runtimeDid agentDid true
  let state := if revoked then apply granted
    (.revoke ⟨"configuration", "source", "reader", .actor agentDid⟩) else granted
  Json.mkObj [("actor", toJson actor), ("id", toJson resource),
    ("permission", toJson operation), ("revoked", toJson revoked), ("rebound", toJson false),
    ("expected", toJson (allowed 5 state actor "configuration" resource operation)),
    ("claim_allowed", toJson (Automation.eventClaimAllowed state actor agentDid "trigger" "source" true true))]

def eventReboundCaseJson (actor resource : String) : Json :=
  let granted := Automation.publishEventConfiguration eventConfigurationState userDid "trigger" "source" runtimeDid agentDid true
  let revoked := apply granted (.revoke ⟨"configuration", "source", "reader", .actor agentDid⟩)
  let rebound := Automation.rebindEventConfiguration revoked userDid "trigger" "source" agentDid siblingDid
  Json.mkObj [("actor", toJson actor), ("id", toJson resource), ("permission", toJson "read"),
    ("revoked", toJson true), ("rebound", toJson true),
    ("expected", toJson (allowed 5 rebound actor "configuration" resource "read")),
    ("claim_allowed", toJson (Automation.eventClaimAllowed rebound actor siblingDid "trigger" "source" true true))]

def eventReboundRevokedJson : Json :=
  let granted := Automation.publishEventConfiguration eventConfigurationState userDid "trigger" "source" runtimeDid agentDid true
  let rebound := Automation.rebindEventConfiguration granted userDid "trigger" "source" agentDid siblingDid
  let revoked := apply rebound (.revoke ⟨"configuration", "source", "reader", .actor siblingDid⟩)
  let unchanged := Automation.publishEventConfiguration revoked userDid "trigger" "source" runtimeDid siblingDid false
  Json.mkObj [("actor", toJson siblingDid), ("id", toJson "source"), ("permission", toJson "read"),
    ("revoked", toJson true), ("rebound", toJson true), ("post_revoke", toJson true),
    ("expected", toJson (allowed 5 unchanged siblingDid "configuration" "source" "read")),
    ("claim_allowed", toJson (Automation.eventClaimAllowed unchanged siblingDid siblingDid "trigger" "source" true true))]

def eventStaleClaimJson : Json :=
  let granted := Automation.publishEventConfiguration eventConfigurationState userDid "trigger" "source" runtimeDid agentDid true
  let rebound := Automation.rebindEventConfiguration granted userDid "trigger" "source" agentDid siblingDid
  Json.mkObj [("actor", toJson agentDid), ("selected", toJson siblingDid),
    ("source_read", toJson (allowed 5 rebound agentDid "configuration" "source" "read")),
    ("claim_allowed", toJson (Automation.eventClaimAllowed rebound agentDid siblingDid "trigger" "source" true true))]

def eventReboundCheckpointJson (actor : String) : Json :=
  let state : State :=
    { objects := [⟨"nodes", "home", userDid⟩,
        ⟨"agents", "old-agent", userDid⟩, ⟨"sessions", "old-session", agentDid⟩,
        ⟨"delivered_requests", "old-request", agentDid⟩],
      relations := [⟨"agents", "old-agent", "parent", .object "nodes" "home"⟩,
        ⟨"agents", "old-agent", "executor", .actor agentDid⟩,
        ⟨"sessions", "old-session", "parent", .object "agents" "old-agent"⟩,
        ⟨"sessions", "old-session", "working_agent", .actor agentDid⟩,
        ⟨"delivered_requests", "old-request", "parent", .object "agents" "old-agent"⟩,
        ⟨"delivered_requests", "old-request", "session", .object "sessions" "old-session"⟩,
        ⟨"delivered_requests", "old-request", "working_agent", .actor agentDid⟩], enrolled := [] }
  Json.mkObj [("actor", toJson actor),
    ("request_read", toJson (allowed 5 state actor "delivered_requests" "old-request" "read")),
    ("cursor", toJson (Automation.checkpointTaskArrival state actor "old-request" true true true 0 1))]

def callbackReboundVisibilityJson (actor : String) : Json :=
  let initial : State :=
    { objects := [⟨"nodes", "home", userDid⟩, ⟨"agents", "old-agent", userDid⟩],
      relations := [⟨"agents", "old-agent", "parent", .object "nodes" "home"⟩,
        ⟨"agents", "old-agent", "executor", .actor agentDid⟩], enrolled := [] }
  let state := Household.commitAgentChild initial "old-invocation" agentDid "old-agent" true
  Json.mkObj [("actor", toJson actor),
    ("invocation_read", toJson (allowed 5 state actor "agent_children" "old-invocation" "read"))]

def unattendedAgentJson (name : String) (actorMatches enabled : Bool)
    (invoke : Automation.InvokeObservation) : Json :=
  Json.mkObj [("name", toJson name), ("actor_matches", toJson actorMatches),
    ("enabled", toJson enabled), ("allowed", toJson (Automation.unattendedAgentAdmission actorMatches enabled invoke))]

def callbackModuleReaderJson (actor : String) (changed revoked : Bool) : Json :=
  let initial : State :=
    { objects := [⟨"configuration", "module", userDid⟩], relations := [], enrolled := [] }
  let granted := Automation.publishCallbackModuleReader initial userDid "module" agentDid true
  let revokedState := if revoked then apply granted (.revoke ⟨"configuration", "module", "reader", .actor agentDid⟩) else granted
  let state := Automation.publishCallbackModuleReader revokedState userDid "module" siblingDid changed
  Json.mkObj [("actor", toJson actor), ("changed", toJson changed), ("revoked", toJson revoked),
    ("expected", toJson (allowed 5 state actor "configuration" "module" "read"))]

def eventGenerationJson (generation : Automation.EventSourceGeneration) : Json := Json.mkObj
  [("collection", toJson generation.collection), ("event_kind", toJson generation.eventKind),
   ("filter", toJson generation.filter), ("correlation", toJson generation.correlation),
   ("grouped", toJson generation.grouped), ("expected_count", toJson generation.expectedCount),
   ("expected_field", toJson generation.expectedField), ("timeout", toJson generation.timeout),
   ("minimum", toJson generation.minimum), ("workspace_authority", toJson generation.workspaceAuthority)]

def eventGenerationCases : List Json :=
  let original : Automation.EventSourceGeneration :=
    ⟨"Records", "created", some "{eligible: {_eq: true}}", some "batch", true,
      some 2, none, some 30, 1, none⟩
  let changed := [("same", original),
    ("filter", { original with filter := some "{eligible: {_eq: false}}" }),
    ("correlation", { original with correlation := some "other_batch" }),
    ("collection", { original with collection := "Other" }),
    ("group", { original with grouped := false, expectedCount := none, timeout := none }),
    ("count", { original with expectedCount := some 3 }),
    ("timeout", { original with timeout := some 60 }),
    ("minimum", { original with minimum := 2 }),
    ("workspace", { original with workspaceAuthority := some "readOnly" })]
  changed.map fun (name, current) => Json.mkObj
    [("name", toJson name), ("observed", eventGenerationJson original),
     ("current", eventGenerationJson current),
     ("allowed", toJson (Automation.eventGenerationAllowed original current)),
     ("receipt", toJson (Automation.eventGenerationReceipt original current true))]

def eventExecutionJson (value : Automation.EventExecutionGeneration) : Json := Json.mkObj
  [("task_id", toJson value.taskId), ("agent", toJson value.agent), ("name", toJson value.name),
   ("prompt", toJson value.prompt), ("objective", toJson value.objective),
   ("budget", toJson value.budget), ("output", toJson value.output), ("hooks", toJson value.hooks),
   ("emit", toJson value.emit), ("source_id", toJson value.sourceId),
   ("trigger_doc", toJson value.triggerDoc), ("source_doc", toJson value.sourceDoc),
   ("task_doc", toJson value.taskDoc),
   ("session_template", toJson value.sessionTemplate), ("concurrency", toJson value.concurrency)]

def eventExecutionCases : List Json :=
  let original : Automation.EventExecutionGeneration :=
    ⟨"task", "agent", none, "work", none, none, none, "[]", false, "source",
      "trigger-doc", "source-doc", "task-doc", none, "parallel"⟩
  let changed := [("same", original), ("prompt", { original with prompt := "changed" }),
    ("source", { original with sourceId := "other" }),
    ("trigger_recreated", { original with triggerDoc := "new-trigger" }),
    ("source_recreated", { original with sourceDoc := "new-source" }),
    ("task_recreated", { original with taskDoc := "new-task" }),
    ("agent", { original with agent := "other" }),
    ("session", { original with sessionTemplate := some "session" }),
    ("concurrency", { original with concurrency := "serial" }),
    ("objective", { original with objective := some "objective" }),
    ("budget", { original with objective := some "objective", budget := some 25 }),
    ("output", { original with output := some "schema" }),
    ("hooks", { original with hooks := "[{\"hook_id\":\"hook\",\"phase\":\"before\",\"command\":[\"true\"]}]" }),
    ("emit", { original with emit := true })]
  changed.map fun (name, current) => Json.mkObj
    [("name", toJson name), ("observed", eventExecutionJson original),
     ("current", eventExecutionJson current),
     ("allowed", toJson (Automation.eventExecutionAllowed original current))]

def eventCursorCaseJson (actor operation : String) (revoked : Bool) : Json :=
  let initial : State :=
    { objects := [⟨"nodes", "home", userDid⟩],
      relations := [⟨"nodes", "home", "runtime_reader", .actor runtimeDid⟩], enrolled := [] }
  let created := Automation.publishEventCursor initial userDid "cursor" "home" agentDid true true
  let state := if revoked then apply created
    (.revoke ⟨"configuration", "cursor", "writer", .actor agentDid⟩) else created
  Json.mkObj [("actor", toJson actor), ("permission", toJson operation), ("revoked", toJson revoked),
    ("expected", toJson (allowed 5 state actor "configuration" "cursor" operation))]

def eventCursorRebindCaseJson (actor : String) (revoked : Bool) : Json :=
  let initial : State :=
    { objects := [⟨"nodes", "home", userDid⟩], relations := [], enrolled := [] }
  let created := Automation.publishEventCursor initial userDid "cursor" "home" agentDid true true
  let rebound := Automation.rebindEventCursor created userDid "cursor" agentDid siblingDid
  let state := if revoked then apply rebound
    (.revoke ⟨"configuration", "cursor", "writer", .actor siblingDid⟩) else rebound
  Json.mkObj [("actor", toJson actor), ("revoked", toJson revoked),
    ("expected", toJson (allowed 5 state actor "configuration" "cursor" "update"))]

def eventCursorMoveCaseJson (cursor : String) : Json :=
  let initial : State :=
    { objects := [⟨"nodes", "home", userDid⟩], relations := [], enrolled := [] }
  let created := Automation.publishEventCursor initial userDid "old" "home" agentDid true true
  let moved := Automation.moveEventCursor created userDid "old" "new" "home" agentDid
  Json.mkObj [("cursor", toJson cursor),
    ("expected", toJson (allowed 5 moved agentDid "configuration" cursor "update"))]

def eventGroupCaseJson (actor : String) : Json :=
  let initial : State :=
    { objects := [⟨"nodes", "home", userDid⟩, ⟨"agents", "agent-doc", userDid⟩],
      relations := [⟨"nodes", "home", "runtime_reader", .actor runtimeDid⟩,
        ⟨"agents", "agent-doc", "parent", .object "nodes" "home"⟩,
        ⟨"agents", "agent-doc", "executor", .actor agentDid⟩], enrolled := [] }
  let state := commitAgentChild initial "group" agentDid "agent-doc" true
  Json.mkObj [("actor", toJson actor),
    ("expected", toJson (allowed 5 state actor "agent_children" "group" "read"))]

def taskReceiptCaseJson (creator actor operation : String) (producerReader : Bool) : Json :=
  let initial : State :=
    { objects := [⟨"nodes", "home", userDid⟩, ⟨"agents", "agent-doc", userDid⟩,
        ⟨"sessions", "session", userDid⟩],
      relations := [⟨"agents", "agent-doc", "parent", .object "nodes" "home"⟩,
        ⟨"sessions", "session", "parent", .object "agents" "agent-doc"⟩,
        ⟨"sessions", "session", "working_agent", .actor agentDid⟩], enrolled := [] }
  let shared := if producerReader then apply initial
    (.grantAs userDid ⟨"agents", "agent-doc", "reader", .actor siblingDid⟩) else initial
  let state := Automation.publishTaskReceipt shared creator "receipt" "session" agentDid true true
  Json.mkObj [("creator", toJson creator), ("actor", toJson actor),
    ("permission", toJson operation), ("producer_reader", toJson producerReader),
    ("expected", toJson (allowed 5 state actor "children" "receipt" operation))]

def scheduleCaseJson (name actor : String) (revokeParent missing : Bool) : Json :=
  let initial : State :=
    { objects := [⟨"nodes", "home", userDid⟩],
      relations := [⟨"nodes", "home", "runtime_reader", .actor runtimeDid⟩], enrolled := [] }
  let published := Automation.publishSchedule initial userDid "schedule" "home" true
  let state := if revokeParent then apply published
    (.revoke ⟨"configuration", "schedule", "parent", .object "nodes" "home"⟩) else published
  let selected := if missing then "replaced" else "schedule"
  Json.mkObj [("name", toJson name), ("actor", toJson actor),
    ("revoke_parent", toJson revokeParent), ("missing", toJson missing),
    ("expected", toJson (Automation.scheduleVisible state runtimeDid actor selected))]

def scheduleWriteCaseJson (name : String) (grant revoke : Bool) : Json :=
  let initial : State :=
    { objects := [⟨"nodes", "home", userDid⟩], relations := [], enrolled := [] }
  let published := Automation.publishSchedule initial userDid "schedule" "home" true
  let writer : Relation := ⟨"configuration", "schedule", "writer", .actor agentDid⟩
  let granted := if grant then apply published (.grantAs userDid writer) else published
  let state := if revoke then apply granted (.revokeAs userDid writer) else granted
  Json.mkObj [("name", toJson name), ("grant", toJson grant), ("revoke", toJson revoke),
    ("expected", toJson (Household.allowed 5 state agentDid "configuration" "schedule" "update"))]

def inferenceProviderCaseJson (name actor revoked : String) : Json :=
  let state := if revoked.isEmpty then Configuration.inferenceProviderFixture else
    apply Configuration.inferenceProviderFixture
      (.revoke ⟨"configuration", revoked, "selected_by", .entitySet "agents" "agent-doc" "executor"⟩)
  Json.mkObj [("name", toJson name), ("actor", toJson actor), ("revoked", toJson revoked),
    ("documents", toJson (Configuration.inferenceProviderDocuments.map Prod.snd)),
    ("expected", toJson (Configuration.providerConfigurationVisible state actor
      Configuration.inferenceProviderDocuments))]

def inferenceUsersetCaseJson (name actor revoked : String) (direct : Bool) : Json :=
  let withDirect := if direct then apply Configuration.inferenceUsersetFixture
    (.grant ⟨"configuration", "sampling", "reader", .actor "agent"⟩)
    else Configuration.inferenceUsersetFixture
  let state := if revoked == "profile" then apply withDirect (.revoke
      ⟨"configuration", "profile", "selected_by", .entitySet "agents" "agent-doc" "executor"⟩)
    else if revoked == "sampling" then apply withDirect (.revoke
      ⟨"configuration", "sampling", "selected_by", .entitySet "configuration" "profile" "selected_by"⟩)
    else if revoked == "executor" then apply withDirect (.revoke
      ⟨"agents", "agent-doc", "executor", .actor "agent"⟩)
    else withDirect
  Json.mkObj [("name", toJson name), ("actor", toJson actor), ("revoked", toJson revoked),
    ("document", toJson "sampling"), ("direct", toJson direct), ("expected", toJson
      (Household.allowed 8 state actor "configuration" "sampling" "read"))]

def retryUsersetCaseJson (name actor : String) (revokeExecution : Bool) : Json :=
  let state := if revokeExecution then apply Configuration.inferenceUsersetFixture (.revoke
      ⟨"configuration", "execution", "selected_by", .entitySet "configuration" "profile" "selected_by"⟩)
    else Configuration.inferenceUsersetFixture
  Json.mkObj [("name", toJson name), ("actor", toJson actor),
    ("revoked", toJson (if revokeExecution then "execution" else "")),
    ("document", toJson "retry"), ("direct", toJson false),
    ("expected", toJson (Household.allowed 8 state actor "configuration" "retry" "read"))]

def backendMetadataSelectionCaseJson (name actor : String) (revoked direct : Bool) : Json :=
  let withDirect := if direct then apply Configuration.inferenceUsersetFixture
    (.grant ⟨"configuration", "backend-metadata", "reader", .actor "agent"⟩)
    else Configuration.inferenceUsersetFixture
  let state := if revoked then apply withDirect (.revoke
    ⟨"configuration", "backend-metadata", "selected_by", .entitySet "configuration" "profile" "selected_by"⟩)
    else withDirect
  Json.mkObj [("name", toJson name), ("actor", toJson actor),
    ("revoked", toJson revoked), ("direct", toJson direct),
    ("read", toJson (Household.allowed 8 state actor "configuration" "backend-metadata" "read")),
    ("update", toJson (Household.allowed 8 state actor "configuration" "backend-metadata" "update"))]

def skillUsersetCaseJson (name actor : String) (revoked : Bool) : Json :=
  let state := if revoked then apply Configuration.skillSelectionFixture (.revoke
      ⟨"configuration", "skill", "selected_by", .entitySet "configuration" "context" "selected_by"⟩)
    else Configuration.skillSelectionFixture
  Json.mkObj [("name", toJson name), ("actor", toJson actor), ("revoked", toJson revoked),
    ("expected", toJson (Household.allowed 8 state actor "configuration" "skill" "read")),
    ("tool_expected", toJson (Configuration.skillToolMayReturn state actor "skill"))]

def compactionUsersetCaseJson (phase actor document : String) : Json :=
  let initial := Configuration.compactionSelectionFixture
  let edgeA : Relation := ⟨"configuration", "compaction-a", "selected_by", .entitySet "configuration" "context" "selected_by"⟩
  let edgeB : Relation := ⟨"configuration", "compaction-b", "selected_by", .entitySet "configuration" "context" "selected_by"⟩
  let rebound := apply (apply initial (.revoke edgeA)) (.grant edgeB)
  let state := if phase == "revoked" || phase == "unchanged" then apply initial (.revoke edgeA)
    else if phase == "rebound" then rebound
    else if phase == "direct-rebound" then apply rebound (.grant
      ⟨"configuration", "compaction-a", "reader", .actor "selected"⟩)
    else if phase == "context-revoked" then apply rebound (.revoke
      ⟨"configuration", "context", "selected_by", .entitySet "agents" "agent" "executor"⟩)
    else if phase == "executor-revoked" then apply rebound (.revoke
      ⟨"agents", "agent", "executor", .actor "selected"⟩)
    else initial
  let visible := Household.allowed 8 state actor "configuration" document "read"
  let guard := Household.allowed 8 state actor "agents" "agent" "read" &&
    Household.allowed 8 state actor "configuration" "context" "read" && visible
  Json.mkObj [("phase", toJson phase), ("actor", toJson actor), ("document", toJson document),
    ("expected", toJson visible), ("guard_expected", toJson guard),
    ("update_expected", toJson (Household.allowed 8 state actor "configuration" document "update")),
    ("owner_update_expected", toJson (Household.allowed 8 state "user" "configuration" document "update"))]

def toolsUsersetCaseJson (name actor : String) (revoked : Bool) : Json :=
  let state := if revoked then apply Configuration.toolsSelectionFixture (.revoke
      ⟨"configuration", "context", "selected_by", .entitySet "agents" "agent" "executor"⟩)
    else Configuration.toolsSelectionFixture
  Json.mkObj [("name", toJson name), ("actor", toJson actor), ("revoked", toJson revoked),
    ("expected", toJson (Household.allowed 8 state actor "configuration" "tools" "read"))]

def taskTargetCaseJson (name actor document transition : String) : Json :=
  let state := if transition == "rebound" then TaskTarget.rebindTask TaskTarget.fixture "user" "a" "b" true
    else if transition == "shared-rebound" then
      TaskTarget.rebindTask (apply TaskTarget.fixture (.grantAs "user"
        ⟨"configuration", "task", "reader", .actor "agent-a"⟩)) "user" "a" "b" true
    else if transition == "task-revoked" then
      let rebound := TaskTarget.rebindTask TaskTarget.fixture "user" "a" "b" true
      let revoked := apply rebound (.revoke (TaskTarget.taskEdge "b"))
      TaskTarget.rebindTask revoked "user" "b" "b" true
    else if transition == "target-revoked" then apply TaskTarget.fixture (.revoke
      ⟨"configuration", "target", "selected_by", .entitySet "configuration" "tools" "selected_by"⟩)
    else TaskTarget.fixture
  Json.mkObj [("name", toJson name), ("actor", toJson actor), ("document", toJson document),
    ("transition", toJson transition), ("expected", toJson (Household.allowed 8 state actor "configuration" document "read"))]

def toolReferencesCaseJson (name : String) (references : Configuration.ToolSelectionReferences) : Json :=
  Json.mkObj [("name", toJson name), ("agents_enabled", toJson references.agentsEnabled),
    ("targets", toJson references.targets), ("services", toJson references.services),
    ("surfaces", toJson references.surfaces),
    ("expected", Json.mkObj ((Configuration.toolSelectionReferences references).map
      fun (collection, ids) => (collection, toJson ids)))]

def pluginProfileCaseJson (name : String) (actor : Option String) (revoked : Bool) : Json :=
  let state := if revoked then apply Configuration.inferenceUsersetFixture (.revoke
      ⟨"configuration", "profile", "selected_by", .entitySet "agents" "agent-doc" "executor"⟩)
    else Configuration.inferenceUsersetFixture
  Json.mkObj [("name", toJson name), ("actor", toJson actor), ("revoked", toJson revoked),
    ("expected", toJson (Configuration.pluginModelMayUse state actor "profile"))]

def selectionManagerCaseJson (name : String) (granted revoked : Bool) : Json :=
  let relation : Relation := ⟨"configuration", "skill", "selection_manager", .actor "delegate"⟩
  let grantedState := if granted then apply Configuration.skillSelectionFixture (.grantAs "user" relation)
    else Configuration.skillSelectionFixture
  let state := if revoked then apply grantedState (.revokeAs "user" relation) else grantedState
  Json.mkObj [("name", toJson name), ("grant", toJson granted), ("revoke", toJson revoked),
    ("selection", toJson (mayManage state "delegate"
      ⟨"configuration", "skill", "selected_by", .entitySet "configuration" "context" "selected_by"⟩)),
    ("reader", toJson (mayManage state "delegate" ⟨"configuration", "skill", "reader", .actor "delegate"⟩)),
    ("update", toJson (allowed 8 state "delegate" "configuration" "skill" "update"))]

def nodeManagerCaseJson (name actor resource id relation : String) (revokeAdmin : Bool) : Json :=
  let state := if revokeAdmin then apply nodeManagerFixture
    (.revoke ⟨"nodes", "node", "administrator", .actor "admin"⟩) else nodeManagerFixture
  Json.mkObj [("name", toJson name), ("actor", toJson actor), ("resource", toJson resource),
    ("relation", toJson relation), ("revoke_admin", toJson revokeAdmin),
    ("expected", toJson (mayManage state actor ⟨resource, id, relation, .actor "selected-agent"⟩))]

def nodeManagerLinkRevocationCaseJson : Json :=
  let state := apply nodeManagerFixture (.revoke
    ⟨"configuration", "config", "selection_manager", .entitySet "nodes" "node" "administrator"⟩)
  Json.mkObj [("admin", toJson (mayManage state "admin"
      ⟨"configuration", "config", "selected_by", .entitySet "agents" "agent" "executor"⟩)),
    ("user", toJson (mayManage state "user"
      ⟨"configuration", "config", "selected_by", .entitySet "agents" "agent" "executor"⟩))]

def agentInvokeManagerCaseJson : Json :=
  let grant : Relation := ⟨"agents", "agent", "session_writer", .actor "reader"⟩
  let granted := apply nodeManagerFixture (.grantAs "user" grant)
  let revoked := apply granted (.revokeAs "user" grant)
  Json.mkObj [("before", toJson (allowed 8 granted "reader" "agents" "agent" "invoke")),
    ("after", toJson (allowed 8 revoked "reader" "agents" "agent" "invoke"))]

def agentManagerLinkRevocationCaseJson : Json :=
  let state := apply nodeManagerFixture (.revoke
    ⟨"agents", "agent", "administrator", .entitySet "nodes" "node" "administrator"⟩)
  Json.mkObj [("admin", toJson (mayManage state "admin" ⟨"agents", "agent", "reader", .actor "reader"⟩)),
    ("user", toJson (mayManage state "user" ⟨"agents", "agent", "reader", .actor "reader"⟩))]
def packInstallationCaseJson (name installer : String) (commit revoke retryFresh direct : Bool) : Json :=
  let initial : State :=
    { objects := [⟨"nodes", "home", userDid⟩],
      relations := [⟨"nodes", "home", "runtime_reader", .actor runtimeDid⟩], enrolled := [] }
  let published := if direct then directCreate initial installer "configuration" "pack" true true
    else Configuration.publishPackInstallation initial installer "pack" "home" true commit
  let state := if revoke then apply published
    (.revoke ⟨"configuration", "pack", "parent", .object "nodes" "home"⟩) else published
  let updated := Configuration.publishPackInstallation state installer "pack" "home" retryFresh true
  let allowed := fun actor permission => Household.allowed 5 updated actor "configuration" "pack" permission
  Json.mkObj [("name", toJson name), ("installer", toJson installer),
    ("commit", toJson commit), ("revoke_parent", toJson revoke), ("retry_fresh", toJson retryFresh),
    ("direct", toJson direct),
    ("installer_read", toJson (allowed installer "read")),
    ("installer_update", toJson (allowed installer "update")),
    ("user_read", toJson (allowed userDid "read")),
    ("user_update", toJson (allowed userDid "update")),
    ("user_delete", toJson (allowed userDid "delete")),
    ("runtime_read", toJson (allowed runtimeDid "read")),
    ("other_read", toJson (allowed siblingDid "read"))]

def packRemovalCaseJson (name actor : String) (writer matchReturned : Bool) : Json :=
  let initial : State :=
    { objects := [⟨"nodes", "home", userDid⟩],
      relations := [⟨"nodes", "home", "runtime_reader", .actor runtimeDid⟩], enrolled := [] }
  let published := Configuration.publishPackInstallation initial agentDid "pack" "home" true true
  let state := if writer then apply published
    (.grantAs agentDid ⟨"configuration", "pack", "writer", .actor siblingDid⟩) else published
  let returned := if matchReturned then "pack" else "other"
  Json.mkObj [("name", toJson name), ("actor", toJson actor),
    ("writer", toJson writer), ("returned_matches", toJson matchReturned),
    ("expected", toJson (Configuration.packRemovalConfirmed state actor "pack" returned))]

def packCleanupCaseJson (name : String) (visible : List Bool) : Json :=
  Json.mkObj [("name", toJson name), ("visible", toJson visible),
    ("expected", toJson (Configuration.packCleanupInputsVisible visible))]

def datastoreToolUseCaseJson (name : String)
    (visible enabled selected declarationMatches : Bool) : Json :=
  Json.mkObj [("name", toJson name), ("visible", toJson visible),
    ("enabled", toJson enabled), ("selected", toJson selected),
    ("declaration_matches", toJson declarationMatches),
    ("expected", toJson (Configuration.cachedDatastoreToolUsable
      visible enabled selected declarationMatches))]

def datastoreWriteCaseJson (name : String) (sourceValid responseConfirmed commit : Bool) : Json :=
  Json.mkObj [("name", toJson name), ("source_valid", toJson sourceValid),
    ("response_confirmed", toJson responseConfirmed), ("commit", toJson commit),
    ("expected", toJson (Configuration.cachedDatastoreWriteCommitted sourceValid responseConfirmed commit))]

def uxDatastoreCaseJson (name : String) (visible enabled declared unchanged : Bool) : Json :=
  Json.mkObj [("name", toJson name), ("visible", toJson visible), ("enabled", toJson enabled),
    ("declared", toJson declared), ("unchanged", toJson unchanged),
    ("expected", toJson (Configuration.cachedUxDatastoreToolUsable visible enabled declared unchanged))]

def uxCreateCaseJson (name : String) (remote signed : Bool) : Json :=
  Json.mkObj [("name", toJson name), ("remote", toJson remote), ("signed_graph", toJson signed),
    ("expected", toJson (Configuration.uxCreateTransportSupported remote signed))]

def affectedCaseJson (name : String) (documents : List _root_.Configuration.Affected.Document)
    (changed : List String) : Json :=
  Json.mkObj [("name", toJson name),
    ("documents", toJson (documents.map fun document => document.key)),
    ("changed", toJson changed),
    ("expected", toJson (_root_.Configuration.Affected.accepted documents changed))]

def affectedCases : List Json :=
  let docs := [_root_.Configuration.Affected.missingSchedule, _root_.Configuration.Affected.task,
    _root_.Configuration.Affected.agent, _root_.Configuration.Affected.tools]
  let collision : _root_.Configuration.Affected.Document :=
    ⟨"tools-collision", ["surface-a", "surface-b"],
      _root_.Configuration.Affected.uniqueToolNames ["duplicate", "duplicate"]⟩
  let surfaceA : _root_.Configuration.Affected.Document := ⟨"surface-a", [], true⟩
  let surfaceB : _root_.Configuration.Affected.Document := ⟨"surface-b", [], true⟩
  let registry : _root_.Configuration.Registry :=
    { tasks := fun _ _ => none,
      agents := fun scope id => if scope == "owner" && id == "disabled-agent" then
        some ⟨"owner", ⟨none, "profile", false⟩⟩ else none,
      contexts := fun _ _ => none, profiles := fun _ _ => none, backends := fun _ _ => none }
  let defaultNode : _root_.Configuration.Affected.Document :=
    ⟨"default-node", ["disabled-agent"],
      _root_.Configuration.defaultAgentPublishable registry "owner" (some "disabled-agent")⟩
  let disabledAgent : _root_.Configuration.Affected.Document := ⟨"disabled-agent", ["profile"], true⟩
  let profile : _root_.Configuration.Affected.Document := ⟨"profile", ["backend"], true⟩
  [affectedCaseJson "unrelated-edit" docs ["tools"],
   affectedCaseJson "changed-referrer" docs ["trigger"],
   affectedCaseJson "shared-target-edit" docs ["task"],
   affectedCaseJson "removed-target" docs ["schedule"],
   affectedCaseJson "visible-reference" (docs ++ [_root_.Configuration.Affected.schedule]) ["trigger"],
   affectedCaseJson "shared-surface-collision" [collision, surfaceA, surfaceB] ["surface-a"],
   affectedCaseJson "disabled-default" [defaultNode, disabledAgent, profile] ["disabled-agent"]]
def pluginDeleteCase (name : String) (installed selected managed pinnedSame : Bool) : Json :=
  let root : Configuration.PluginDeleteRoot := ⟨"team/worker", "digest-a"⟩
  let selection : Configuration.PluginDeleteSelection :=
    ⟨"tools-doc", "context-doc", "team/worker",
      (if pinnedSame then some "digest-a" else some "digest-b"),
      agentDid, (if managed then agentDid else userDid), selected⟩
  let state : Configuration.PluginDeleteProjection :=
    { roots := (if installed then [root] else []), selections := [selection] }
  Json.mkObj [("name", toJson name), ("installed", toJson installed),
    ("selected", toJson selected), ("managed", toJson managed),
    ("pinned_same", toJson pinnedSame),
    ("source_count", toJson (Configuration.pluginDeleteSources state).length)]

def pluginDeleteCases : List Json :=
  [pluginDeleteCase "selected-installed" true true true true,
   pluginDeleteCase "uninstalled" false true true true,
   pluginDeleteCase "disabled" true false true true,
   pluginDeleteCase "foreign-actor" true true false true,
   pluginDeleteCase "replaced-digest" true true true false]

def toolSource : Configuration.ToolGrantSource := ⟨"tools-doc", agentDid, "schema", "patch-collection"⟩
def otherToolSource : Configuration.ToolGrantSource := ⟨"other-tools-doc", agentDid, "schema", "patch-collection"⟩
def otherAgentSource : Configuration.ToolGrantSource := ⟨"tools-doc", siblingDid, "schema", "patch-collection"⟩
def otherPurposeSource : Configuration.ToolGrantSource := ⟨"tools-doc", agentDid, "other", "patch-collection"⟩

def toolSourceJson (source : Configuration.ToolGrantSource) : Json := Json.mkObj
  [("tools_doc", toJson source.toolsDoc), ("agent", toJson source.agent),
   ("purpose", toJson source.purpose), ("permission", toJson source.permission)]

def toolGrantCaseJson (name : String) (initial : Configuration.ToolGrantProjection)
    (source : Configuration.ToolGrantSource) (enable authorized committed : Bool) : Json :=
  let result := Configuration.changeToolGrant initial source enable authorized committed
  Json.mkObj [("name", toJson name),
    ("initial_enabled", toJson (initial.enabled.map toolSourceJson)),
    ("initial_grants", toJson (initial.grants.map toolSourceJson)),
    ("direct", toJson (initial.direct.map fun (actor, permission) =>
      Json.mkObj [("actor", toJson actor), ("permission", toJson permission)])),
    ("owners", toJson initial.owners), ("admins", toJson initial.admins),
    ("source", toolSourceJson source),
    ("enable", toJson enable), ("authorized", toJson authorized), ("committed", toJson committed),
    ("expected_enabled", toJson (result.enabled.map toolSourceJson)),
    ("expected_grants", toJson (result.grants.map toolSourceJson)),
    ("expected_effective", toJson (Configuration.toolGrantEffective result source.agent source.permission))]

def toolGrantCases : List Json :=
  let selected : Configuration.ToolGrantProjection := { enabled := [toolSource], grants := [toolSource] }
  [toolGrantCaseJson "enable" {} toolSource true true true,
   toolGrantCaseJson "disable" selected toolSource false true true,
   toolGrantCaseJson "other-source-survives" { selected with enabled := [toolSource, otherToolSource], grants := [toolSource, otherToolSource] } toolSource false true true,
   toolGrantCaseJson "other-purpose-survives" { selected with enabled := [toolSource, otherPurposeSource], grants := [toolSource, otherPurposeSource] } toolSource false true true,
   toolGrantCaseJson "other-agent-survives" { selected with enabled := [toolSource, otherAgentSource], grants := [toolSource, otherAgentSource] } toolSource false true true,
   toolGrantCaseJson "direct-survives" { selected with direct := [(agentDid, "patch-collection")] } toolSource false true true,
   toolGrantCaseJson "owner-survives" { selected with owners := [agentDid] } toolSource false true true,
   toolGrantCaseJson "admin-survives" { selected with admins := [agentDid] } toolSource false true true,
   toolGrantCaseJson "unauthorized-enable" {} toolSource true false true,
   toolGrantCaseJson "failed-enable" {} toolSource true true false,
   toolGrantCaseJson "unauthorized-disable" selected toolSource false false true,
   toolGrantCaseJson "failed-disable" selected toolSource false true false,
   toolGrantCaseJson "missing-physical-document" {} { toolSource with toolsDoc := "" } true true true,
   toolGrantCaseJson "idempotent-enable" selected toolSource true true true]

def schemaToolSources : List Configuration.ToolGrantSource :=
  Configuration.schemaToolPermissions.map fun permission =>
    ⟨"tools-doc", agentDid, "schema", permission⟩

def toolGrantGroupCaseJson (name : String) (initial : Configuration.ToolGrantProjection)
    (sources : List Configuration.ToolGrantSource) (enable authorized committed : Bool) : Json :=
  let result := Configuration.changeToolGrantGroup initial sources enable authorized committed
  Json.mkObj [("name", toJson name), ("sources", toJson (sources.map toolSourceJson)),
    ("initial_enabled", toJson (initial.enabled.map toolSourceJson)),
    ("initial_grants", toJson (initial.grants.map toolSourceJson)),
    ("direct", toJson (initial.direct.map fun (actor, permission) =>
      Json.mkObj [("actor", toJson actor), ("permission", toJson permission)])),
    ("owners", toJson initial.owners), ("admins", toJson initial.admins),
    ("enable", toJson enable), ("authorized", toJson authorized), ("committed", toJson committed),
    ("expected_enabled", toJson (result.enabled.map toolSourceJson)),
    ("expected_grants", toJson (result.grants.map toolSourceJson)),
    ("expected_permissions", toJson (Configuration.schemaToolPermissions.map fun permission =>
      Json.mkObj [("permission", toJson permission),
        ("allowed", toJson (Configuration.toolGrantEffective result agentDid permission))]))]

def toolGrantGroupCases : List Json :=
  let initial : Configuration.ToolGrantProjection :=
    { enabled := schemaToolSources ++ [otherPurposeSource],
      grants := schemaToolSources ++ [otherPurposeSource],
      direct := [(agentDid, "get-collection")] }
  [toolGrantGroupCaseJson "schema-enable-all" {} schemaToolSources true true true,
   toolGrantGroupCaseJson "schema-disable-preserves-overlap" initial schemaToolSources false true true,
   toolGrantGroupCaseJson "schema-enable-rejected" {} schemaToolSources true false true,
   toolGrantGroupCaseJson "schema-enable-rollback" {} schemaToolSources true true false,
   toolGrantGroupCaseJson "schema-disable-rollback" initial schemaToolSources false true false,
   toolGrantGroupCaseJson "mixed-actor-rejected" {} [toolSource, otherAgentSource] true true true]

def sharedToolGroup : Configuration.ToolGroup := ⟨"tools-doc", "schema"⟩
def nextToolGroup : Configuration.ToolGroup := ⟨"next-tools-doc", "schema"⟩

def sharedToolHierarchy : Configuration.ToolGrantHierarchy :=
  { permissions := Configuration.schemaToolPermissions.map fun permission => ⟨sharedToolGroup, permission⟩,
    contexts := [⟨sharedToolGroup, "context-doc"⟩],
    agents := [⟨"context-doc", agentDid⟩, ⟨"context-doc", siblingDid⟩] }

def toolGroupJson (group : Configuration.ToolGroup) : Json := Json.mkObj
  [("tools_doc", toJson group.toolsDoc), ("purpose", toJson group.purpose)]

def toolHierarchyJson (state : Configuration.ToolGrantHierarchy) : Json := Json.mkObj
  [("permissions", toJson (state.permissions.map fun edge => Json.mkObj
      [("group", toolGroupJson edge.group), ("permission", toJson edge.permission)])),
   ("contexts", toJson (state.contexts.map fun edge => Json.mkObj
      [("group", toolGroupJson edge.group), ("context_doc", toJson edge.contextDoc)])),
   ("agents", toJson (state.agents.map fun edge => Json.mkObj
      [("context_doc", toJson edge.contextDoc), ("agent", toJson edge.agent)])),
   ("direct", toJson (state.direct.map fun (actor, permission) => Json.mkObj
      [("actor", toJson actor), ("permission", toJson permission)])),
   ("owners", toJson state.owners), ("admins", toJson state.admins)]

def toolHierarchyCaseJson (name action : String) (initial result : Configuration.ToolGrantHierarchy)
    (authorized committed : Bool) : Json :=
  let flattened := Configuration.flattenToolHierarchy result
  Json.mkObj [("name", toJson name), ("action", toJson action),
    ("initial", toolHierarchyJson initial), ("group", toolGroupJson sharedToolGroup),
    ("next_group", toolGroupJson nextToolGroup), ("context_doc", toJson "context-doc"),
    ("agent", toJson agentDid), ("next_context_doc", toJson "next-context-doc"),
    ("permissions", toJson Configuration.schemaToolPermissions),
    ("authorized", toJson authorized), ("committed", toJson committed),
    ("expected", toolHierarchyJson result),
    ("expected_grants", toJson (flattened.grants.map toolSourceJson)),
    ("agent_allowed", toJson (Configuration.toolGrantEffective flattened agentDid "patch-collection")),
    ("sibling_allowed", toJson (Configuration.toolGrantEffective flattened siblingDid "patch-collection"))]

def toolHierarchyCases : List Json :=
  let inactive : Configuration.ToolGrantHierarchy := { sharedToolHierarchy with permissions := [] }
  let set := Configuration.setToolHierarchy
  [toolHierarchyCaseJson "disable-shared-tools" "disable" sharedToolHierarchy
      (set sharedToolHierarchy sharedToolGroup Configuration.schemaToolPermissions false true true) true true,
   toolHierarchyCaseJson "enable-dormant-shared-tools" "enable" inactive
      (set inactive sharedToolGroup Configuration.schemaToolPermissions true true true) true true,
   toolHierarchyCaseJson "disable-keeps-independent-grant" "disable"
      { sharedToolHierarchy with direct := [(agentDid, "patch-collection")] }
      (set { sharedToolHierarchy with direct := [(agentDid, "patch-collection")] }
        sharedToolGroup Configuration.schemaToolPermissions false true true) true true,
   toolHierarchyCaseJson "failed-shared-disable" "disable" sharedToolHierarchy
      (set sharedToolHierarchy sharedToolGroup Configuration.schemaToolPermissions false true false) true false,
   toolHierarchyCaseJson "unauthorized-shared-enable" "enable" inactive
      (set inactive sharedToolGroup Configuration.schemaToolPermissions true false true) false true,
   toolHierarchyCaseJson "move-shared-context" "context" sharedToolHierarchy
      (Configuration.rebindToolContext sharedToolHierarchy "context-doc" nextToolGroup true true) true true,
   toolHierarchyCaseJson "failed-context-move" "context" sharedToolHierarchy
      (Configuration.rebindToolContext sharedToolHierarchy "context-doc" nextToolGroup true false) true false,
   toolHierarchyCaseJson "move-one-agent" "agent" sharedToolHierarchy
      (Configuration.rebindToolAgent sharedToolHierarchy agentDid "next-context-doc" true true) true true,
   toolHierarchyCaseJson "unauthorized-agent-move" "agent" sharedToolHierarchy
      (Configuration.rebindToolAgent sharedToolHierarchy agentDid "next-context-doc" false true) false true]

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

def toolRebindingCaseJson (name : String) (authorized committed : Bool) : Json :=
  let initial : Configuration.ToolGrantProjection :=
    { enabled := [toolSource, otherPurposeSource], grants := [toolSource, otherPurposeSource],
      direct := [(agentDid, "patch-collection")] }
  let result := Configuration.replaceToolGrant initial toolSource otherToolSource authorized committed
  Json.mkObj [("name", toJson name), ("old", toolSourceJson toolSource),
    ("next", toolSourceJson otherToolSource),
    ("initial_enabled", toJson (initial.enabled.map toolSourceJson)),
    ("initial_grants", toJson (initial.grants.map toolSourceJson)),
    ("direct", toJson (initial.direct.map fun (actor, permission) =>
      Json.mkObj [("actor", toJson actor), ("permission", toJson permission)])),
    ("owners", toJson initial.owners), ("admins", toJson initial.admins),
    ("authorized", toJson authorized), ("committed", toJson committed),
    ("expected_enabled", toJson (result.enabled.map toolSourceJson)),
    ("expected_grants", toJson (result.grants.map toolSourceJson)),
    ("expected_effective", toJson (Configuration.toolGrantEffective result agentDid "patch-collection"))]

def sessionToolSource : Configuration.ToolGrantSource :=
  ⟨"tools-doc", agentDid, "session-history", "read-document"⟩

def sessionToolHousehold : State :=
  { objects := [⟨"sessions", "owned", agentDid⟩, ⟨"sessions", "shared", userDid⟩,
      ⟨"sessions", "foreign", siblingDid⟩],
    relations := [⟨"sessions", "shared", "reader", .actor agentDid⟩], enrolled := [] }

def sessionToolCaseJson (session : String) (granted admin nativeBypass : Bool) : Json :=
  let grants : Configuration.ToolGrantProjection :=
    { enabled := (if granted then [sessionToolSource] else []),
      grants := (if granted then [sessionToolSource] else []),
      admins := (if admin then [agentDid] else []) }
  Json.mkObj [("session", toJson session), ("actor", toJson agentDid),
    ("source", toolSourceJson sessionToolSource), ("granted", toJson granted),
    ("admin", toJson admin), ("native_bypass", toJson nativeBypass),
    ("objects", toJson (sessionToolHousehold.objects.map objectJson)),
    ("relations", toJson (sessionToolHousehold.relations.map relationJson)),
    ("expected", toJson (Configuration.toolSessionReadAllowed grants sessionToolHousehold agentDid session nativeBypass))]

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

def toolGrantorCaseJson (grantor : String) (committed : Bool) : Json :=
  let initial : Configuration.ToolGrantProjection := { admins := [userDid] }
  let result := Configuration.configureToolGrantGroup initial toolGrantorHousehold
    schemaToolSources grantor true committed
  Json.mkObj [("grantor", toJson grantor), ("committed", toJson committed),
    ("sources", toJson (schemaToolSources.map toolSourceJson)),
    ("objects", toJson (toolGrantorHousehold.objects.map objectJson)),
    ("relations", toJson (toolGrantorHousehold.relations.map relationJson)),
    ("admins", toJson initial.admins),
    ("expected_enabled", toJson (result.enabled.map toolSourceJson)),
    ("expected_grants", toJson (result.grants.map toolSourceJson))]

def toolOperationCaseJson (name : String) (schema history goal create selfConfig pack : Bool) (category : String) : Json :=
  let surface : ToolPolicy.Surface :=
    { ToolPolicy.ContractCases.secureMinimal with
      schemaManagement := schema
      sessionHistory := history
      goalTools := goal
      goalCreate := create
      selfConfig := selfConfig
      queryTools := .none
      writeTools := .none
      selfConfigCategories := if category == "all" then .all else if category == "none" then .none
        else .only {category} (fun _ => ()) }
  Json.mkObj [("name", toJson name), ("schema", toJson schema), ("history", toJson history),
    ("goal", toJson goal), ("create", toJson create), ("self_config", toJson selfConfig),
    ("category", toJson category), ("pack", toJson pack),
    ("query", toJson false), ("write", toJson false),
    ("sources", toJson ((Configuration.toolOperationSources surface pack).map fun (purpose, permission) =>
      Json.mkObj [("purpose", toJson purpose), ("permission", toJson permission)]))]

def datastoreOperationCaseJson (name : String) (query write : Bool) : Json :=
  let scope : ToolPolicy.EndpointScope (String × String) (Finset String) :=
    .only {("tool", "collection")} (fun _ => ∅)
  let surface : ToolPolicy.Surface :=
    { ToolPolicy.ContractCases.secureMinimal with
      queryTools := if query then scope else .none
      writeTools := if write then scope else .none }
  Json.mkObj [("name", toJson name), ("schema", toJson false), ("history", toJson false),
    ("goal", toJson false), ("create", toJson false), ("self_config", toJson false),
    ("category", toJson "none"), ("pack", toJson false),
    ("query", toJson query), ("write", toJson write),
    ("sources", toJson ((Configuration.toolOperationSources surface false).map fun (purpose, permission) =>
      Json.mkObj [("purpose", toJson purpose), ("permission", toJson permission)]))]

def toolOperationCases : List Json :=
  [toolOperationCaseJson "none" false false false false false false "none",
   toolOperationCaseJson "schema" true false false false false false "none",
   toolOperationCaseJson "session-history" false true false false false false "none",
   toolOperationCaseJson "goal" false false true false false false "none",
   toolOperationCaseJson "goal-create-without-goal" false false false true false false "none",
   toolOperationCaseJson "goal-and-create" false false true true false false "none",
   toolOperationCaseJson "self-config-without-categories" false false false false true true "none",
   toolOperationCaseJson "self-config-all" false false false false true false "all",
   toolOperationCaseJson "self-config-agent" false false false false true false "agent",
   toolOperationCaseJson "self-config-node" false false false false true false "node",
   toolOperationCaseJson "self-config-tools" false false false false true false "tools",
   toolOperationCaseJson "self-config-profile" false false false false true false "profile",
   toolOperationCaseJson "self-config-backend" false false false false true false "backend",
   toolOperationCaseJson "self-config-mcp-service" false false false false true false "mcp_service",
   toolOperationCaseJson "self-config-automation" false false false false true false "automation",
   toolOperationCaseJson "pack-independent" false false false false true true "backend",
   toolOperationCaseJson "schema-and-pack-overlap" true false false false true true "all",
   toolOperationCaseJson "all" true true true true true true "all",
   datastoreOperationCaseJson "datastore-query" true false,
   datastoreOperationCaseJson "datastore-write" false true,
   datastoreOperationCaseJson "datastore-overlap" true true]

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
def callbackState : State :=
  { objects := [⟨"nodes", "home", userDid⟩,
      ⟨"configuration", "binding", userDid⟩,
      ⟨"configuration", "callback", userDid⟩,
      ⟨"configuration", "module", userDid⟩],
    relations := [⟨"nodes", "home", "runtime_reader", .actor runtimeDid⟩,
      ⟨"configuration", "binding", "parent", .object "nodes" "home"⟩,
      ⟨"configuration", "callback", "parent", .object "nodes" "home"⟩,
      ⟨"configuration", "module", "parent", .object "nodes" "home"⟩], enrolled := [] }

def callbackCaseJson (actor id : String) (revoked : Bool) : Json :=
  let granted := Automation.publishCallbackReaders callbackState userDid "binding" "callback" agentDid true
  let state := if revoked then apply granted
    (.revoke ⟨"configuration", "binding", "reader", .actor agentDid⟩) else granted
  Json.mkObj [("actor", toJson actor), ("id", toJson id), ("revoked", toJson revoked),
    ("expected", toJson (allowed 5 state actor "configuration" id "read")),
    ("claim_allowed", toJson (Automation.callbackClaimAllowed state actor agentDid "binding" "callback" true true))]

def callbackGenerationCaseJson (name : String) (sourceVisible : Bool)
    (observed current : Option String) : Json :=
  Json.mkObj [("name", toJson name), ("source_visible", toJson sourceVisible),
    ("observed", toJson observed), ("current", toJson current),
    ("allowed", toJson (Automation.callbackGenerationAllowed sourceVisible observed current))]

def callbackModuleClaimJson (name : String) (visible same prepared committed : Bool) : Json :=
  Json.mkObj [("name", toJson name), ("visible", toJson visible), ("same", toJson same),
    ("prepared", toJson prepared), ("committed", toJson committed),
    ("allowed", toJson (Automation.callbackModuleClaim visible same prepared committed))]

def callbackRecoveryConfigurationJson (claimed : Bool) : Json :=
  Json.mkObj [("claimed", toJson claimed), ("snapshot", toJson "admitted"),
    ("current", toJson "changed"),
    ("expected", toJson (Automation.callbackRecoveryConfiguration claimed "admitted" "changed"))]

def callbackPlacementJson (actor : String) (revoked : Bool) : Json :=
  let initial : State :=
    { objects := [⟨"configuration", "placement", userDid⟩],
      relations := [⟨"configuration", "placement", "reader", .actor agentDid⟩], enrolled := [] }
  let state := if revoked then apply initial (.revoke ⟨"configuration", "placement", "reader", .actor agentDid⟩) else initial
  Json.mkObj [("actor", toJson actor), ("revoked", toJson revoked),
    ("expected", toJson (Automation.callbackPlacementReadable state actor "placement"))]

def callbackChildJson (name : String) (fresh committed revoke : Bool) : Json :=
  let initial : State :=
    { objects := [⟨"nodes", "home", userDid⟩, ⟨"agents", "agent-doc", userDid⟩],
      relations := [⟨"agents", "agent-doc", "parent", .object "nodes" "home"⟩,
        ⟨"agents", "agent-doc", "executor", .actor agentDid⟩], enrolled := [] }
  let published := Automation.publishCallbackChild initial agentDid agentDid "child" "agent-doc" true true
  let before := if revoke then apply published
    (.revoke ⟨"agent_children", "child", "parent", .object "agents" "agent-doc"⟩) else initial
  let result := Automation.publishCallbackChild before agentDid agentDid "child" "agent-doc" fresh committed
  Json.mkObj [("name", toJson name), ("user_read", toJson (allowed 5 result userDid "agent_children" "child" "read")),
    ("agent_read", toJson (allowed 5 result agentDid "agent_children" "child" "read"))]

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

def runtimeDefaultAccountCase (name : String) (access : Configuration.RuntimeDefaultAccountRead)
    (visibleDefault : Option String) : Json :=
  Json.mkObj [("name", toJson name), ("runtime_actor", toJson access.runtimeActor),
    ("same_node", toJson access.sameNode), ("same_snapshot", toJson access.sameSnapshot),
    ("native_read", toJson access.nativeRead), ("visible_default", toJson visibleDefault),
    ("allowed", toJson (Configuration.runtimeMayReadDefaultAccount access))]

def runtimeDefaultAccountCases : List Json := Id.run do
  let allowed : Configuration.RuntimeDefaultAccountRead := ⟨true, true, true, true⟩
  return [runtimeDefaultAccountCase "visible-named" allowed (some "named"),
    runtimeDefaultAccountCase "visible-empty" allowed none,
    runtimeDefaultAccountCase "foreign-node" { allowed with sameNode := false } (some "named"),
    runtimeDefaultAccountCase "agent-actor" { allowed with runtimeActor := false } (some "named"),
    runtimeDefaultAccountCase "other-snapshot" { allowed with sameSnapshot := false } (some "named"),
    runtimeDefaultAccountCase "read-failed" { allowed with nativeRead := false } (some "named")]

def runtimeDefaultChoiceCase (name currentKind nextKind : String)
    (currentRef nextRef visibleDefault : Option String) : Json :=
  let current := some (currentKind, _root_.Configuration.BackendAuth.nodeOAuth currentRef)
  let next := (nextKind, _root_.Configuration.BackendAuth.nodeOAuth nextRef)
  Json.mkObj [("name", toJson name), ("current_kind", toJson currentKind),
    ("current_ref", toJson currentRef), ("next_kind", toJson nextKind),
    ("next_ref", toJson nextRef), ("visible_default", toJson visibleDefault),
    ("accepted", toJson (SelfConfig.backendChoiceAllowed (fun _ => visibleDefault) current next))]

def runtimeDefaultChoiceCases : List Json :=
  [runtimeDefaultChoiceCase "cross-named-default" "ChatGptCodex" "XaiGrokOAuth"
      (some "a1") (some "g2") (some "g2"),
   runtimeDefaultChoiceCase "cross-wrong-default" "ChatGptCodex" "XaiGrokOAuth"
      (some "a1") none (some "g2"),
   runtimeDefaultChoiceCase "cross-empty-default" "ChatGptCodex" "XaiGrokOAuth"
      (some "a1") none none,
   runtimeDefaultChoiceCase "same-provider-cannot-switch" "XaiGrokOAuth" "XaiGrokOAuth"
      (some "g2") none none]

def backendCredentialNativeCase (revoked : Bool) : Json :=
  let initial := Configuration.backendCredentialFixture
  let state := if revoked then Household.apply initial
    (.revoke ⟨"configuration", "backend", "reader", .actor "agent"⟩) else initial
  let checks := [("agent", "configuration", "backend", "read"),
    ("agent", "configuration", "backend", "update"),
    ("agent", "secret_configuration", "credential", "read"),
    ("runtime", "secret_configuration", "credential", "read"),
    ("user", "secret_configuration", "credential", "update")]
  Json.mkObj [("revoked", toJson revoked),
    ("objects", toJson (state.objects.map objectJson)),
    ("relations", toJson (state.relations.map relationJson)),
    ("checks", toJson (checks.map fun (actor, resource, doc, permission) =>
      Json.mkObj [("actor", toJson actor), ("resource", toJson resource),
        ("doc", toJson doc), ("permission", toJson permission),
        ("allowed", toJson (Household.allowed 5 state actor resource doc permission))]))]

def backendCredentialPublicationCase (metadata secret : String) (authorized committed : Bool) : Json :=
  let prior : Configuration.BackendCredentialPublication := ⟨"old", "old"⟩
  let result := Configuration.publishBackendCredential prior metadata secret authorized committed
  Json.mkObj [("metadata", toJson metadata), ("secret", toJson secret),
    ("authorized", toJson authorized), ("committed", toJson committed),
    ("expected_metadata", toJson result.metadataVersion),
    ("expected_secret", toJson result.secretVersion)]

def pluginBackendCase (name : String) (actor : Option String) (revoked : String) : Json :=
  let state := if revoked == "" then Configuration.pluginBackendFixture else
    Household.apply Configuration.pluginBackendFixture
      (.revoke ⟨"configuration", revoked, "reader", .actor "agent"⟩)
  Json.mkObj [("name", toJson name), ("actor", toJson actor), ("revoked", toJson revoked),
    ("expected", toJson (Configuration.pluginBackendMayUse state actor))]

def inferenceSelfConfigCase (phase actor document : String) : Json :=
  let state := if phase == "before" then Configuration.inferenceUsersetFixture
    else if phase == "revoked" then Household.apply Configuration.inferenceSelfConfigFixture
      (.revoke ⟨"agents", "agent-doc", "config_writer", .actor "agent"⟩)
    else if phase == "direct-after-revoke" then Household.apply
      (Household.apply Configuration.inferenceSelfConfigFixture
        (.grant ⟨"configuration", "execution", "writer", .actor "agent"⟩))
      (.revoke ⟨"agents", "agent-doc", "config_writer", .actor "agent"⟩)
    else Configuration.inferenceSelfConfigFixture
  Json.mkObj [("phase", toJson phase), ("actor", toJson actor), ("document", toJson document),
    ("manager_expected", toJson (Household.mayManage state actor
      ⟨"configuration", document, "selected_by", .entitySet "configuration" "profile" "selected_by"⟩)),
    ("expected", toJson (Household.allowed 8 state actor "configuration" document "update"))]

def json : Json := Json.mkObj
  [("name", toJson "configuration-explicit-grants"),
   ("managed_client_handoff_cases", toJson (([false, true] : List Bool).flatMap fun request =>
     ([false, true] : List Bool).flatMap fun owner =>
     ([false, true] : List Bool).map fun node =>
       Json.mkObj [("request", toJson request), ("owner", toJson owner),
         ("node", toJson node),
         ("approve", toJson (Configuration.managedClientMayApprove request owner node))])),
   ("desktop_reset_cases", toJson (([false, true] : List Bool).flatMap fun full =>
     ([("cache", Configuration.DesktopResetAsset.cache),
       ("binding", .binding), ("upgrade", .upgrade), ("user_key", .userKey),
       ("user_metadata", .userMetadata), ("p2p_key", .p2pKey),
       ("custody", .custody), ("peers", .peers)]).map fun (name, asset) =>
       Json.mkObj [("full", toJson full), ("asset", toJson name),
         ("retired", toJson (Configuration.desktopResetRetires full asset))])),
   ("backend_credential_cases", toJson backendCredentialCases),
   ("runtime_default_account_cases", toJson runtimeDefaultAccountCases),
   ("runtime_default_choice_cases", toJson runtimeDefaultChoiceCases),
   ("configuration_plugin_backend_cases", toJson [
      pluginBackendCase "selected" (some "agent") "",
      pluginBackendCase "sibling" (some "sibling") "",
      pluginBackendCase "absent" none "",
      pluginBackendCase "profile_revoked" (some "agent") "profile",
      pluginBackendCase "backend_revoked" (some "agent") "backend"]),
   ("backend_observation_authority_cases", toJson ([
      ("runtime", "backend-metadata", "update"),
      ("runtime", "backend-observation", "update"),
      ("backend-owner", "backend-metadata", "update"),
      ("agent", "backend-observation", "read"),
      ("sibling", "backend-observation", "read")].map fun (actor, object, permission) =>
      Json.mkObj [("actor", toJson actor), ("object", toJson object), ("permission", toJson permission),
        ("allowed", toJson (Household.allowed 10 Configuration.backendObservationFixture actor "configuration" object permission))])),
   ("backend_discovery_cases", toJson ([(true, true, true, true), (false, true, true, true),
       (true, false, true, true), (true, true, false, true), (true, true, true, false)].map
      fun (authorized, doc, connection, committed) => Json.mkObj
        [("authorized", toJson authorized), ("same_doc", toJson doc),
         ("same_connection", toJson connection), ("committed", toJson committed),
         ("published", toJson (Configuration.backendDiscoveryMayPublish authorized doc connection committed))])),
   ("backend_probe_cases", toJson ([(true, true, true), (false, true, true), (true, false, true), (true, true, false)].map fun (doc, same, committed) =>
      Json.mkObj [("same_doc", toJson doc), ("same_connection", toJson same), ("committed", toJson committed),
        ("published", toJson (Configuration.backendProbeMayPublish doc same committed))])),
   ("backend_removal_cases", toJson ([(true, true, true), (false, true, true),
       (true, false, true), (true, true, false)].map fun (authorized, matching, committed) =>
      let (metadata, observation, credential) := Configuration.removeBackendRecords authorized matching committed
      Json.mkObj [("authorized", toJson authorized), ("matching", toJson matching),
        ("committed", toJson committed), ("metadata", toJson metadata),
        ("observation", toJson observation), ("credential", toJson credential)])),
   ("backend_genesis_distinct", toJson (Configuration.backendGenesis "backend" "credential" "nonce-a" !=
      Configuration.backendGenesis "backend" "credential" "nonce-b")),
   ("legacy_backend_lockdown_unavailable", toJson (Configuration.legacyBackendAdmissionAllowed false)),
   ("fresh_backend_admission_cases", toJson ([(true, true, true), (false, true, true),
       (true, false, true), (true, true, false)].map fun (origin, binding, intact) =>
      Json.mkObj [("origin", toJson origin), ("binding", toJson binding),
        ("intact", toJson intact),
        ("allowed", toJson (Configuration.freshBackendAdmissionAllowed origin binding intact))])),
   ("fresh_backend_origin_cases", toJson ([(true, true, true, true), (false, true, true, true),
       (true, false, true, true), (true, true, false, true), (true, true, true, false)].map
      fun (locked, empty, unexposed, committed) => Json.mkObj
        [("locked", toJson locked), ("empty", toJson empty), ("unexposed", toJson unexposed),
         ("committed", toJson committed),
         ("origin", toJson (Configuration.freshBackendOrigin locked empty unexposed committed))])),
   ("fresh_backend_reopen_cases", toJson ([(true, true, true), (false, true, true), (false, true, false),
       (true, false, true), (true, true, false)].map fun (marker, store, binding) =>
      Json.mkObj [("marker", toJson marker), ("store", toJson store), ("binding", toJson binding),
        ("allowed", toJson (Configuration.freshBackendReopenAllowed marker store binding))])),
   ("backend_credential_publication_cases", toJson [
     backendCredentialPublicationCase "new" "new" true true,
     backendCredentialPublicationCase "new" "other" true true,
     backendCredentialPublicationCase "new" "new" false true,
     backendCredentialPublicationCase "new" "new" true false]),
   ("backend_credential_native_cases", toJson ([backendCredentialNativeCase false,
      backendCredentialNativeCase true])),
   ("plugin_delete_cases", toJson pluginDeleteCases),
   ("tool_grant_projection_cases", toJson toolGrantCases),
   ("tool_grant_group_cases", toJson toolGrantGroupCases),
   ("tool_grant_hierarchy_cases", toJson toolHierarchyCases),
   ("tool_grant_rebinding_cases", toJson ([toolRebindingCaseJson "rebind" true true,
      toolRebindingCaseJson "unauthorized-rebind" false true,
      toolRebindingCaseJson "failed-rebind" true false])),
   ("session_tool_access_cases", toJson ([sessionToolCaseJson "owned" true false false,
      sessionToolCaseJson "shared" true false false, sessionToolCaseJson "foreign" true false false,
      sessionToolCaseJson "owned" false false false, sessionToolCaseJson "foreign" false true true,
      sessionToolCaseJson "foreign" false true false])),
   ("tool_grantor_cases", toJson ([toolGrantorCaseJson userDid true,
      toolGrantorCaseJson agentDid true, toolGrantorCaseJson userDid false])),
   ("tool_operation_cases", toJson toolOperationCases),
   ("managed_tool_identity_cases", toJson ([("matched", "agent", "agent"),
      ("foreign", "user", "agent"), ("empty", "", "agent")].map fun (name, claimed, derived) =>
      Json.mkObj [("name", toJson name),
        ("allowed", toJson (Configuration.managedToolSourceMatches claimed derived))])),
   ("policy_yaml", toJson policyYaml),
   ("objects", toJson (initial.objects.map objectJson)),
   ("relations", toJson (initial.relations.map relationJson)), ("enrolled", toJson ([] : List String)),
   ("policy_registrations", toJson ([] : List Json)),
   ("self_config_actions", toJson ([] : List Json)),
   ("self_config_lookup_cases", toJson ([] : List Json)),
   ("configuration_signed_create_collections", toJson (["Node", "Agent", "AgentContext", "CompactionConfig", "Skill", "DatastoreToolSurface", "Tools", "AgentTarget", "InferenceBackend", "InferenceBackendMetadata", "InferenceProfile", "InferenceSampling", "InferenceExecution", "InferenceRetryPolicy", "ToolServiceRegistry", "ProjectionAcpBinding", "Task", "Schedule", "EventSource", "Trigger", "Callback", "CallbackBinding", "CallbackModule", "RepositoryPlacement", "EvalDefinition", "NotBundled"].map fun collection =>
      Json.mkObj [("collection", toJson collection),
        ("supported", toJson (Configuration.clientSignedCreateCollection collection)),
        ("update_supported", toJson (Configuration.clientSignedUpdateSupported collection))])),
   ("configuration_signed_delete_collections", toJson (["Node", "Agent", "AgentContext", "Tools", "Schedule", "PackInstallation", "Goal", "GoalCreationClaim", "InferenceBackend", "InferenceBackendMetadata", "InferenceBackendObservation", "BackendCredential", "AgentSession", "NotBundled"].map fun collection =>
      Json.mkObj [("collection", toJson collection), ("supported", toJson (Configuration.clientSignedDeleteCollection collection))])),
   ("ordinary_client_mutation_cases", toJson ([(false, false), (false, true), (true, false), (true, true)].map fun (intent, mutation) =>
      Json.mkObj [("intent", toJson intent), ("mutation", toJson mutation),
        ("allowed", toJson (Configuration.ordinaryMutationAllowed intent mutation))])),
   ("client_goal_transaction_actor_cases", toJson ([(some "user", "user"), (some "agent", "user"), (none, "user"), (some "", "")].map fun (actor, requester) =>
      Json.mkObj [("actor", toJson actor), ("requester", toJson requester),
        ("allowed", toJson (Configuration.clientGoalTransactionActorMatches actor requester))])),
   ("client_goal_graph_handoff_cases", toJson ([(true, true, true, true, false, false),
      (true, true, true, true, true, true), (false, true, true, true, true, true),
      (true, false, true, true, true, true), (true, true, false, true, true, true),
      (true, true, true, false, true, true), (true, true, true, true, true, false)].map fun (actor, command, create, authorized, replacing, delete) =>
      Json.mkObj [("actor", toJson actor), ("command", toJson command), ("create", toJson create),
        ("authorized", toJson authorized), ("replacing", toJson replacing), ("delete", toJson delete),
        ("allowed", toJson (Configuration.clientGoalGraphHandoffAllowed actor command create authorized replacing delete))])),
   ("held_client_signer_cases", toJson ([("user-a", "user-a", "user-a", true),
      ("user-a", "user-a", "user-b", true), ("user-a", "user-a", "user-a", false),
      ("user-a", "user-b", "user-a", true)].map fun (actor, bearer, signer, hasPrivate) =>
        Json.mkObj [("actor", toJson actor), ("bearer", toJson bearer),
          ("signer", toJson signer), ("has_private", toJson hasPrivate),
          ("allowed", toJson (Configuration.heldClientSignerAuthorizes actor bearer signer hasPrivate))])),
   ("configuration_immutable_cases", toJson ([("before", "before"), ("before", "after")].map fun (observed, proposed) =>
      Json.mkObj [("observed", toJson observed), ("proposed", toJson proposed),
        ("allowed", toJson (Configuration.immutableReplacementAllowed observed proposed))])),
   ("configuration_signed_write_retry_cases", toJson ([false, true].map fun dispatched =>
      Json.mkObj [("dispatched", toJson dispatched),
        ("may_retry", toJson (Configuration.clientSignedWriteMayRetry dispatched))])),
   ("configuration_atomic_cases", toJson ([false, true].map atomicCaseJson)),
   ("configuration_node_binding_cases", toJson ([(true, true, true), (false, true, true),
      (true, false, true), (true, true, false)].map fun (sameNode, hasDoc, visible) =>
        bindingCaseJson sameNode hasDoc visible)),
   ("configuration_provider_cases", toJson
      [providerCaseJson "normal" agentDid "normal" false false,
       providerCaseJson "auxiliary" agentDid "auxiliary" false false,
       providerCaseJson "sibling" siblingDid "normal" false false,
       providerCaseJson "revoked-tools" agentDid "normal" true false,
       providerCaseJson "replaced-context" agentDid "auxiliary" false true]),
   ("configuration_inference_provider_cases", toJson
      ([inferenceProviderCaseJson "selected" "agent" "",
        inferenceProviderCaseJson "sibling" "sibling" ""] ++
       ["profile", "sampling", "execution", "retry", "compaction", "summary-profile"].map
        fun id => inferenceProviderCaseJson ("revoked-" ++ id) "agent" id)),
   ("configuration_backend_metadata_cases", toJson
      [backendMetadataSelectionCaseJson "selected" "agent" false false,
       backendMetadataSelectionCaseJson "sibling" "sibling" false false,
       backendMetadataSelectionCaseJson "revoked" "agent" true false,
       backendMetadataSelectionCaseJson "direct-survives" "agent" true true]),
   ("configuration_userset_cases", toJson
      [inferenceUsersetCaseJson "selected" "agent" "" false,
       inferenceUsersetCaseJson "sibling" "sibling" "" false,
       inferenceUsersetCaseJson "parent-owner" "profile-owner" "" false,
       inferenceUsersetCaseJson "profile-revoked" "agent" "profile" false,
       inferenceUsersetCaseJson "sampling-revoked" "agent" "sampling" false,
       inferenceUsersetCaseJson "executor-revoked" "agent" "executor" false,
       inferenceUsersetCaseJson "direct-reader-survives" "agent" "sampling" true,
       retryUsersetCaseJson "retry-selected" "agent" false,
       retryUsersetCaseJson "retry-sibling" "sibling" false,
       retryUsersetCaseJson "retry-revoked" "agent" true]),
   ("configuration_inference_self_config_cases", toJson
      [inferenceSelfConfigCase "before" "agent" "execution",
       inferenceSelfConfigCase "granted" "agent" "profile",
       inferenceSelfConfigCase "granted" "agent" "context",
       inferenceSelfConfigCase "granted" "agent" "tools",
       inferenceSelfConfigCase "granted" "agent" "execution",
       inferenceSelfConfigCase "granted" "agent" "new-execution",
       inferenceSelfConfigCase "granted" "sibling" "execution",
       inferenceSelfConfigCase "before" "agent" "backend-metadata",
       inferenceSelfConfigCase "granted" "agent" "backend-metadata",
       inferenceSelfConfigCase "granted" "sibling" "backend-metadata",
       inferenceSelfConfigCase "revoked" "agent" "backend-metadata",
       inferenceSelfConfigCase "revoked" "agent" "execution",
       inferenceSelfConfigCase "revoked" "agent" "new-execution",
       inferenceSelfConfigCase "revoked" "agent" "profile",
       inferenceSelfConfigCase "revoked" "agent" "context",
       inferenceSelfConfigCase "revoked" "agent" "tools",
       inferenceSelfConfigCase "direct-after-revoke" "agent" "execution"]),
   ("configuration_skill_cases", toJson
      [skillUsersetCaseJson "selected" "selected" false,
       skillUsersetCaseJson "sibling" "sibling" false,
       skillUsersetCaseJson "revoked" "selected" true]),
   ("configuration_compaction_cases", toJson
      ((["initial", "revoked", "unchanged", "rebound", "direct-rebound", "context-revoked", "executor-revoked"] : List String).flatMap fun phase =>
        (["selected", "sibling"] : List String).flatMap fun actor =>
          (["compaction-a", "compaction-b"] : List String).map fun document =>
            compactionUsersetCaseJson phase actor document)),
   ("configuration_tools_selection_cases", toJson
      [toolsUsersetCaseJson "selected" "selected" false,
       toolsUsersetCaseJson "sibling" "sibling" false,
       toolsUsersetCaseJson "revoked-context" "selected" true]),
   ("configuration_task_target_cases", toJson
      [taskTargetCaseJson "task-owner" "user" "task" "initial",
       taskTargetCaseJson "task-runtime" "runtime" "task" "initial",
       taskTargetCaseJson "task-selected" "agent-a" "task" "initial",
       taskTargetCaseJson "task-unrelated" "agent-b" "task" "initial",
       taskTargetCaseJson "task-old-target" "agent-a" "task" "rebound",
       taskTargetCaseJson "task-new-target" "agent-b" "task" "rebound",
       taskTargetCaseJson "task-explicit-reader" "agent-a" "task" "shared-rebound",
       taskTargetCaseJson "task-revoked-replay" "agent-b" "task" "task-revoked",
       taskTargetCaseJson "target-selected" "agent-a" "target" "initial",
       taskTargetCaseJson "target-unrelated" "agent-b" "target" "initial",
       taskTargetCaseJson "target-revoked" "agent-a" "target" "target-revoked"]),

   ("configuration_tool_reference_cases", toJson
      [toolReferencesCaseJson "empty" ⟨false, [], [], []⟩,
       toolReferencesCaseJson "disabled-target" ⟨false, ["target"], [], []⟩,
       toolReferencesCaseJson "independent-sets" ⟨true, ["target-a", "target-b"],
         ["service-a", "service-b"], ["surface-a", "surface-b"]⟩,
       toolReferencesCaseJson "duplicates" ⟨true, ["target", "target"],
         ["service", "service"], ["surface", "surface"]⟩]),
   ("configuration_plugin_profile_cases", toJson
      [pluginProfileCaseJson "selected" (some "agent") false,
       pluginProfileCaseJson "sibling" (some "sibling") false,
       pluginProfileCaseJson "absent" none false,
       pluginProfileCaseJson "revoked" (some "agent") true]),
   ("configuration_selection_manager_cases", toJson
      [selectionManagerCaseJson "denied" false false,
       selectionManagerCaseJson "granted" true false,
       selectionManagerCaseJson "revoked" true true]),
   ("configuration_node_manager_cases", toJson
      [nodeManagerCaseJson "node-owner" "user" "configuration" "config" "selected_by" false,
       nodeManagerCaseJson "node-admin" "admin" "configuration" "config" "selected_by" false,
       nodeManagerCaseJson "creator" "creator-agent" "configuration" "config" "selected_by" false,
       nodeManagerCaseJson "other-admin" "other-admin" "configuration" "config" "selected_by" false,
       nodeManagerCaseJson "revoked-admin" "admin" "configuration" "config" "selected_by" true,
       nodeManagerCaseJson "admin-cannot-grant-writer" "admin" "configuration" "config" "writer" false,
       nodeManagerCaseJson "root-owner" "user" "workspace_roots" "root" "reader" false,
       nodeManagerCaseJson "root-admin" "admin" "workspace_roots" "root" "reader" false,
       nodeManagerCaseJson "root-other-admin" "other-admin" "workspace_roots" "root" "reader" false,
       nodeManagerCaseJson "root-revoked-admin" "admin" "workspace_roots" "root" "reader" true,
       nodeManagerCaseJson "agent-owner" "user" "agents" "agent" "session_writer" false,
       nodeManagerCaseJson "agent-admin" "admin" "agents" "agent" "session_writer" false,
       nodeManagerCaseJson "agent-creator" "creator-agent" "agents" "agent" "session_writer" false,
       nodeManagerCaseJson "agent-other-admin" "other-admin" "agents" "agent" "session_writer" false,
       nodeManagerCaseJson "agent-revoked-admin" "admin" "agents" "agent" "session_writer" true]),
   ("configuration_node_manager_link_revocation_case", nodeManagerLinkRevocationCaseJson),
   ("agent_node_manager_invoke_case", agentInvokeManagerCaseJson),
   ("agent_node_manager_link_revocation_case", agentManagerLinkRevocationCaseJson),
   ("mcp_service_use_cases", toJson
      ([(true, true, true, true, true, true),
        (false, true, true, true, true, true),
        (true, false, true, true, true, true),
        (true, true, false, true, true, true),
        (true, true, true, false, true, true),
        (true, true, true, true, false, true),
        (true, true, true, true, true, false)].map fun
        (actor, chain, selection, selected, visible, enabled) => Json.mkObj
          [("same_actor", toJson actor), ("same_chain", toJson chain),
           ("same_selection", toJson selection), ("selected", toJson selected),
           ("service_visible", toJson visible), ("enabled", toJson enabled),
           ("allowed", toJson (McpService.mayUse actor chain selection selected visible enabled))])),
   ("configuration_schedule_cases", toJson
      [scheduleCaseJson "runtime" runtimeDid false false,
       scheduleCaseJson "agent" agentDid false false,
       scheduleCaseJson "revoked-parent" runtimeDid true false,
       scheduleCaseJson "replaced" runtimeDid false true]),
   ("configuration_schedule_write_cases", toJson
      [scheduleWriteCaseJson "denied" false false,
       scheduleWriteCaseJson "granted" true false,
       scheduleWriteCaseJson "revoked" true true]),
   ("configuration_pack_installation_cases", toJson
      [packInstallationCaseJson "user-created" userDid true false false false,
       packInstallationCaseJson "agent-created" agentDid true false false false,
       packInstallationCaseJson "agent-parent-revoked" agentDid true true false false,
       packInstallationCaseJson "agent-parent-revoked-claimed-fresh" agentDid true true true false,
       packInstallationCaseJson "aborted" agentDid false false false false,
       packInstallationCaseJson "direct-orphan" agentDid true false false true]),
   ("configuration_pack_removal_cases", toJson
      [packRemovalCaseJson "agent-owner" agentDid false true,
       packRemovalCaseJson "user-parent" userDid false true,
       packRemovalCaseJson "runtime-reader" runtimeDid false true,
       packRemovalCaseJson "agent-writer" siblingDid true true,
       packRemovalCaseJson "wrong-document" agentDid false false]),
   ("configuration_pack_cleanup_cases", toJson
      [packCleanupCaseJson "none" [],
       packCleanupCaseJson "all-visible" [true, true],
       packCleanupCaseJson "unavailable-first" [false, true],
       packCleanupCaseJson "unavailable-later" [true, false]]),
   ("configuration_datastore_tool_use_cases", toJson
      [datastoreToolUseCaseJson "allowed" true true true true,
       datastoreToolUseCaseJson "revoked" false true true true,
       datastoreToolUseCaseJson "disabled" true false true true,
       datastoreToolUseCaseJson "deselected" true true false true,
       datastoreToolUseCaseJson "changed" true true true false]),
   ("configuration_datastore_write_cases", toJson
      [datastoreWriteCaseJson "committed" true true true,
       datastoreWriteCaseJson "mutation-failed" true false false,
       datastoreWriteCaseJson "unconfirmed" true false true,
       datastoreWriteCaseJson "source-revoked" false true true]),
   ("configuration_ux_datastore_cases", toJson
      [uxDatastoreCaseJson "allowed" true true true true,
       uxDatastoreCaseJson "revoked" false true true true,
       uxDatastoreCaseJson "disabled" true false true true,
       uxDatastoreCaseJson "not-declared" true true false true,
       uxDatastoreCaseJson "changed" true true true false]),
   ("configuration_ux_create_cases", toJson
      [uxCreateCaseJson "embedded-held" false false,
       uxCreateCaseJson "remote-unsigned" true false,
       uxCreateCaseJson "remote-approved-graph" true true]),
   ("configuration_affected_cases", toJson affectedCases),
   ("projection_binding_cases", toJson
      ([false, true].flatMap fun revoked =>
        let state := if revoked then apply Configuration.projectionBindingFixture
          (.revoke ⟨"configuration", "binding", "parent", .object "nodes" "node"⟩)
          else Configuration.projectionBindingFixture
        ["user", "creator", "runtime", "agent", "sibling", "outsider"].flatMap fun actor =>
          ["read", "update"].map fun permission => Json.mkObj
            [("revoked", toJson revoked), ("actor", toJson actor),
             ("permission", toJson permission),
             ("expected", toJson (Household.allowed 5 state actor "configuration" "binding" permission))])),
   ("configuration_removal_cases", toJson
      ([(["doc"], "exact"), ([], "empty"), (["other"], "different"),
        (["doc", "doc"], "duplicate")].map fun (returned, name) =>
        Json.mkObj [("name", toJson name), ("target", toJson "doc"), ("returned", toJson returned),
          ("expected", toJson (_root_.Configuration.Affected.removalConfirmed "doc" returned))])),
   ("callback_configuration_cases", toJson
      [callbackCaseJson runtimeDid "binding" false, callbackCaseJson runtimeDid "callback" false,
       callbackCaseJson runtimeDid "module" false, callbackCaseJson agentDid "binding" false,
       callbackCaseJson agentDid "callback" false, callbackCaseJson agentDid "module" false,
       callbackCaseJson siblingDid "binding" false, callbackCaseJson agentDid "binding" true]),
   ("callback_generation_cases", toJson
      [callbackGenerationCaseJson "same" true (some "original") (some "original"),
       callbackGenerationCaseJson "source_revoked" false (some "original") (some "original"),
       callbackGenerationCaseJson "source_filter" true (some "original") (some "source_filter"),
       callbackGenerationCaseJson "binding_input" true (some "original") (some "binding_input"),
       callbackGenerationCaseJson "callback_handler" true (some "original") (some "callback_handler"),
       callbackGenerationCaseJson "binding_recreated" true (some "original") (some "binding_recreated"),
       callbackGenerationCaseJson "source_recreated" true (some "original") (some "source_recreated"),
       callbackGenerationCaseJson "callback_recreated" true (some "original") (some "callback_recreated"),
       callbackGenerationCaseJson "missing" true none (some "original")]),
   ("callback_module_claim_cases", toJson
      [callbackModuleClaimJson "allowed" true true true true,
       callbackModuleClaimJson "revoked" false true true true,
       callbackModuleClaimJson "changed" true false true true,
       callbackModuleClaimJson "recreated" true false true true,
       callbackModuleClaimJson "unprepared" true true false true,
       callbackModuleClaimJson "aborted" true true true false]),
   ("callback_recovery_configuration_cases", toJson
      [callbackRecoveryConfigurationJson true, callbackRecoveryConfigurationJson false]),
   ("callback_placement_cases", toJson [callbackPlacementJson userDid false,
     callbackPlacementJson agentDid false, callbackPlacementJson siblingDid false,
     callbackPlacementJson agentDid true]),
   ("callback_child_cases", toJson [callbackChildJson "commit" true true false,
     callbackChildJson "abort" true false false, callbackChildJson "duplicate" false true true]),
   ("event_configuration_cases", toJson
      [eventConfigurationCaseJson userDid "trigger" "update" false,
       eventConfigurationCaseJson runtimeDid "trigger" "update" false,
       eventConfigurationCaseJson runtimeDid "source" "read" false,
       eventConfigurationCaseJson agentDid "trigger" "read" false,
       eventConfigurationCaseJson agentDid "source" "read" false,
       eventConfigurationCaseJson agentDid "trigger" "update" false,
       eventConfigurationCaseJson siblingDid "source" "read" false,
       eventConfigurationCaseJson agentDid "source" "read" true,
       eventReboundCaseJson siblingDid "trigger", eventReboundCaseJson siblingDid "source",
       eventReboundCaseJson agentDid "source", eventReboundCaseJson agentDid "trigger", eventReboundRevokedJson]),
   ("event_cursor_cases", toJson
      [eventCursorCaseJson userDid "update" false, eventCursorCaseJson runtimeDid "read" false,
       eventCursorCaseJson runtimeDid "update" false, eventCursorCaseJson agentDid "update" false,
       eventCursorCaseJson siblingDid "read" false, eventCursorCaseJson agentDid "update" true]),
   ("event_cursor_rebind_cases", toJson
      [eventCursorRebindCaseJson agentDid false, eventCursorRebindCaseJson siblingDid false,
       eventCursorRebindCaseJson siblingDid true]),
   ("event_cursor_move_cases", toJson [eventCursorMoveCaseJson "old", eventCursorMoveCaseJson "new"]),
   ("event_stale_claim_cases", toJson [eventStaleClaimJson]),
   ("event_rebound_checkpoint_cases", toJson
      [eventReboundCheckpointJson userDid, eventReboundCheckpointJson agentDid, eventReboundCheckpointJson siblingDid]),
   ("callback_rebound_visibility_cases", toJson
      [callbackReboundVisibilityJson userDid, callbackReboundVisibilityJson agentDid, callbackReboundVisibilityJson siblingDid]),
   ("unattended_agent_cases", toJson
      [unattendedAgentJson "allowed" true true .allowed,
       unattendedAgentJson "disabled" true false .allowed,
       unattendedAgentJson "reader_only" true true .denied,
       unattendedAgentJson "unavailable" true true .unavailable,
       unattendedAgentJson "wrong_actor" false true .allowed]),
   ("unattended_receipt_cases", toJson Identity.UnattendedReceipt.cases),
   ("callback_checkpoint_claim_cases", toJson
      (([(false, false), (true, false), (false, true), (true, true)] : List (Bool × Bool)).map fun (time, config) =>
        Json.mkObj [("claim_time", toJson time), ("retained_configuration", toJson config),
          ("expected", toJson (Identity.UnattendedReceipt.visibleCallbackClaim time config))])),
   ("ordinary_receipt_visibility_cases", toJson
      [Json.mkObj [("name", toJson "hidden"), ("expected", toJson (Identity.UnattendedReceipt.ordinaryVisible true false))],
       Json.mkObj [("name", toJson "absent"), ("expected", toJson (Identity.UnattendedReceipt.ordinaryVisible false false))]]),
   ("callback_module_reader_cases", toJson
      [callbackModuleReaderJson agentDid false false, callbackModuleReaderJson siblingDid false false,
       callbackModuleReaderJson agentDid true false, callbackModuleReaderJson siblingDid true false,
       callbackModuleReaderJson agentDid false true]),
   ("event_generation_cases", toJson eventGenerationCases),
   ("event_execution_cases", toJson eventExecutionCases),
   ("event_group_cases", toJson
      [eventGroupCaseJson userDid, eventGroupCaseJson agentDid,
       eventGroupCaseJson runtimeDid, eventGroupCaseJson siblingDid]),
   ("task_receipt_cases", toJson
      [taskReceiptCaseJson userDid userDid "read" false,
       taskReceiptCaseJson userDid agentDid "read" false,
       taskReceiptCaseJson agentDid userDid "read" false,
       taskReceiptCaseJson agentDid agentDid "read" false,
       taskReceiptCaseJson agentDid agentDid "update" false,
       taskReceiptCaseJson agentDid siblingDid "read" false,
       taskReceiptCaseJson agentDid siblingDid "read" true]),
   ("pairing_overlay_cases", toJson
      ([false, true].flatMap fun revoked =>
        let state := if revoked then apply Configuration.agentPairingFixture
          (.revoke ⟨"configuration", "overlay", "parent", .object "nodes" "node"⟩)
          else Configuration.agentPairingFixture
        ["user", "runtime", "agent", "sibling", "outsider"].flatMap fun actor =>
          ["read", "update"].map fun permission => Json.mkObj
            [("revoked", toJson revoked), ("actor", toJson actor), ("permission", toJson permission),
             ("expected", toJson (Household.allowed 5 state actor "configuration" "overlay" permission))])),
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

def identityContractsJson : String := (toJson [Json.mkObj
  [("name", toJson "identity.agent_owned_data"),
   ("statement", toJson "User keys stay on clients. Node keys serve P2P and own node-local operational data not caused by agents; operational data inherits user read access through Node. Agent work never falls back to runtime authority. Agent DIDs derive from immutable creation nonces and select all agent work. Users own Node configuration and transitively administer Agents. User-created sessions are user-owned; unattended sessions are agent-owned with configured user access and deletion. Agents own session children with parent-session user access and control. Full policy wiring remains follow-up."),
   ("node_local_collections", toJson nodeLocalCollections),
   ("replicated_protocol_collections", toJson replicatedProtocolCollections),
   ("schema_actor", toJson (schemaActor { did := runtimeDid, displayName := none, enabled := true } .createSchema)),
   ("runtime_actor", toJson runtimeDid),
   ("enforced", toJson false), ("tracked_by", toJson "#3485")]]).compress

end Identity.Conformance
