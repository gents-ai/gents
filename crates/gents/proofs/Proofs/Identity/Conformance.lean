import Proofs.Identity.State
import Proofs.Identity.Permission
import Proofs.Identity.Properties
import Proofs.Conformance.ContractCases

namespace Identity.Conformance

def amyDid : String :=
  "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK"

def ruminationDid : String :=
  "did:key:z6MkfXG2FkNy3u7Eg3jm8e2YQpGz7Z1JqWgHDAP1hLk9r2bR"

def ghostDid : String :=
  "did:key:z6MkpTHR8VNsBxYAAWHut2Geadd9jSwuBV8xRoAnwWsdvktH"

structure NodeCase where
  did     : String
  enabled : Bool
  deriving Repr

structure AgentCase where
  id        : String
  node      : String
  enabled   : Bool
  deriving Repr

structure PermissionGrantCase where
  node       : String
  permission : String
  deriving Repr

structure IdentityStructuralCase where
  name        : String
  nodes       : List NodeCase
  agents      : List AgentCase
  deriving Repr

def agentCaseToAgent (c : AgentCase) : Agent :=
  { id := c.id, node := c.node, displayName := none, enabled := c.enabled }

def IdentityStructuralCase.world (c : IdentityStructuralCase) : World :=
  { nodes := (c.nodes.map (fun p =>
      ({ did := p.did, displayName := none, enabled := p.enabled } : Node))).toFinset,
    agents := (c.agents.map agentCaseToAgent).toFinset }

def IdentityStructuralCase.wellFormed (c : IdentityStructuralCase) : Bool :=
  decide c.world.WellFormed

def structuralCases : List IdentityStructuralCase :=
  [ { name        := "amy_general_and_amy_code_share_node"
    , nodes       := [{ did := amyDid, enabled := true }]
    , agents      :=
        [ { id := "amy-general", node := amyDid, enabled := true }
        , { id := "amy-code",    node := amyDid, enabled := true } ]
    }
  , { name        := "amy_rumination_separate_node"
    , nodes       :=
        [ { did := amyDid,        enabled := true }
        , { did := ruminationDid, enabled := true } ]
    , agents      :=
        [ { id := "amy-general",     node := amyDid,        enabled := true }
        , { id := "amy-rumination",  node := ruminationDid, enabled := true } ]
    }
  , { name        := "dangling_agent_fk_violates"
    , nodes       := [{ did := amyDid, enabled := true }]
    , agents      :=
        [ { id := "orphan", node := ghostDid, enabled := true } ]
    }
  , { name        := "duplicate_agent_id_violates"
    , nodes       := [{ did := amyDid, enabled := true }]
    , agents      :=
        [ { id := "amy-general", node := amyDid, enabled := true }
        , { id := "amy-general", node := amyDid, enabled := false } ]
    }
  , { name := "same_agent_label_across_nodes_allowed"
    , nodes := [{ did := amyDid, enabled := true }, { did := ruminationDid, enabled := true }]
    , agents := [{ id := "coding", node := amyDid, enabled := true },
                    { id := "coding", node := ruminationDid, enabled := true }] }


  ]

structure IdentityPermissionCase where
  name                     : String
  nodes                    : List NodeCase
  agents                   : List AgentCase
  grants                   : List PermissionGrantCase
  permission               : String
  rowOwner                 : String
  actorNode                : String
  actorAgent               : String
  peerNode                 : String
  peerAgent                : String
  expectedActorNode        : Option String
  expectedPeerNode         : Option String
  expectedActorAllowed     : Bool
  expectedPeerAllowed      : Bool
  sameNode                 : Bool
  expectedDecisionsEqual   : Bool
  deriving Repr


def grantStoreFromCases (grants : List PermissionGrantCase) : GrantStore String :=
  { granted := fun node permission =>
      grants.any (fun grant =>
        grant.node == node && grant.permission == permission) }

def permissionDecideFromGrants (grants : List PermissionGrantCase) :
    Decide String :=
  canonicalDecide (grantStoreFromCases grants)

theorem permissionDecideFromGrants_respectsNode
    (grants : List PermissionGrantCase) :
    RespectsNode (permissionDecideFromGrants grants) :=
  canonicalDecide_respectsNode (grantStoreFromCases grants)

def mkIdentityPermissionCase
    (name : String)
    (nodes : List NodeCase)
    (agents : List AgentCase)
    (grants : List PermissionGrantCase)
    (permission rowOwner actorNode actorAgent peerNode peerAgent : String) :
    IdentityPermissionCase :=
  let actor := Identity.findAgent? (agents.map agentCaseToAgent) actorNode actorAgent
  let peer := Identity.findAgent? (agents.map agentCaseToAgent) peerNode peerAgent
  let actorAllowed := actor.any (fun agent => permissionDecideFromGrants grants agent permission)
  let peerAllowed := peer.any (fun agent => permissionDecideFromGrants grants agent permission)
  { name := name
  , nodes := nodes
  , agents := agents
  , grants := grants
  , permission := permission
  , rowOwner := rowOwner
  , actorNode := actorNode
  , actorAgent := actorAgent
  , peerNode := peerNode
  , peerAgent := peerAgent
  , expectedActorNode := actor.map (·.node)
  , expectedPeerNode := peer.map (·.node)
  , expectedActorAllowed := actorAllowed
  , expectedPeerAllowed := peerAllowed
  , sameNode := actor.any (fun a => peer.any (fun b => a.node == b.node))
  , expectedDecisionsEqual := actorAllowed == peerAllowed
  }

def amyNode : NodeCase :=
  { did := amyDid, enabled := true }

def ruminationNode : NodeCase :=
  { did := ruminationDid, enabled := true }

def amyGeneralAgent : AgentCase :=
  { id := "amy-general", node := amyDid, enabled := true }

def amyCodeAgent : AgentCase :=
  { id := "amy-code", node := amyDid, enabled := true }

def amyRuminationAgent : AgentCase :=
  { id := "amy-rumination", node := ruminationDid, enabled := true }

def amyRowReadPermission : String :=
  "row:" ++ amyDid ++ ":memory.read"

def ruminationRowReadPermission : String :=
  "row:" ++ ruminationDid ++ ":journal.read"

def grant (node permission : String) : PermissionGrantCase :=
  { node := node, permission := permission }

def identityPermissionCases : List IdentityPermissionCase :=
  [ mkIdentityPermissionCase
      "same_node_row_owner_grant_allows_shared_agents"
      [amyNode]
      [amyGeneralAgent, amyCodeAgent]
      [grant amyDid amyRowReadPermission]
      amyRowReadPermission
      amyDid
      amyDid "amy-general"
      amyDid "amy-code"
  , mkIdentityPermissionCase
      "separate_node_without_grant_blocks_peer"
      [amyNode, ruminationNode]
      [amyGeneralAgent, amyRuminationAgent]
      [grant amyDid amyRowReadPermission]
      amyRowReadPermission
      amyDid
      amyDid "amy-general"
      ruminationDid "amy-rumination"
  , mkIdentityPermissionCase
      "separate_node_with_grant_allows_peer"
      [amyNode, ruminationNode]
      [amyGeneralAgent, amyRuminationAgent]
      [ grant amyDid amyRowReadPermission
      , grant ruminationDid amyRowReadPermission ]
      amyRowReadPermission
      amyDid
      amyDid "amy-general"
      ruminationDid "amy-rumination"
  , mkIdentityPermissionCase
      "scoped_agent_lookup_selects_declared_node"
      [amyNode, ruminationNode]
      [amyGeneralAgent, amyCodeAgent, amyRuminationAgent]
      [grant ruminationDid ruminationRowReadPermission]
      ruminationRowReadPermission
      ruminationDid
      amyDid "amy-code"
      ruminationDid "amy-rumination"
  ]

theorem structural_scope_cases_pinned : structuralCases.map (·.wellFormed) =
    [true, true, false, false, true] := by native_decide

/-- Same label never borrows the other node's ACP grant, regardless of
query ordering. Ambiguous same-owner documents fail resolution. -/
def sharedLabelAgents : List AgentCase :=
  [{ id := "coding", node := amyDid, enabled := true },
   { id := "coding", node := ruminationDid, enabled := true }]

def scopedSelectionCases : List IdentityPermissionCase :=
  [ mkIdentityPermissionCase "same-label-scoped-selection-forward"
      [amyNode, ruminationNode] sharedLabelAgents
      [grant amyDid amyRowReadPermission] amyRowReadPermission amyDid
      amyDid "coding" ruminationDid "coding"
  , mkIdentityPermissionCase "same-label-scoped-selection-reverse"
      [amyNode, ruminationNode] sharedLabelAgents.reverse
      [grant amyDid amyRowReadPermission] amyRowReadPermission amyDid
      amyDid "coding" ruminationDid "coding"
  , mkIdentityPermissionCase "unknown-owner-does-not-select-other-node"
      [amyNode, ruminationNode] sharedLabelAgents
      [grant amyDid amyRowReadPermission] amyRowReadPermission amyDid
      ghostDid "coding" amyDid "coding"
  , mkIdentityPermissionCase "same-owner-collision-does-not-select-first-row"
      [amyNode] [amyGeneralAgent, { amyGeneralAgent with enabled := false }]
      [grant amyDid amyRowReadPermission] amyRowReadPermission amyDid
      amyDid "amy-general" amyDid "amy-general" ]

theorem scoped_selection_permission_cases_pinned : scopedSelectionCases.map
    (fun c => (c.expectedActorNode, c.expectedPeerNode,
      c.expectedActorAllowed, c.expectedPeerAllowed)) =
    [(some amyDid, some ruminationDid, true, false),
     (some amyDid, some ruminationDid, true, false),
     (none, some amyDid, false, true), (none, none, false, false)] := by native_decide

open Conformance.Contracts
open Conformance.ContractCases (boolString)

def nodeCaseJson (c : NodeCase) : String :=
  "{"
    ++ "\"did\":" ++ jsonString c.did ++ ","
    ++ "\"enabled\":" ++ boolString c.enabled
    ++ "}"

def agentCaseJson (c : AgentCase) : String :=
  "{"
    ++ "\"id\":" ++ jsonString c.id ++ ","
    ++ "\"node\":" ++ jsonString c.node ++ ","
    ++ "\"enabled\":" ++ boolString c.enabled
    ++ "}"

def permissionGrantCaseJson (c : PermissionGrantCase) : String :=
  "{"
    ++ "\"node\":" ++ jsonString c.node ++ ","
    ++ "\"permission\":" ++ jsonString c.permission
    ++ "}"

def identityStructuralCaseJson (c : IdentityStructuralCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"nodes\":" ++ jsonArray (c.nodes.map nodeCaseJson) ++ ","
    ++ "\"agents\":" ++ jsonArray (c.agents.map agentCaseJson) ++ ","
    ++ "\"well_formed\":" ++ boolString c.wellFormed
    ++ "}"

def structuralCasesJson : String :=
  jsonArray (structuralCases.map identityStructuralCaseJson)

def identityPermissionCaseJson (c : IdentityPermissionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"nodes\":" ++ jsonArray (c.nodes.map nodeCaseJson) ++ ","
    ++ "\"agents\":" ++ jsonArray (c.agents.map agentCaseJson) ++ ","
    ++ "\"grants\":" ++ jsonArray (c.grants.map permissionGrantCaseJson) ++ ","
    ++ "\"permission\":" ++ jsonString c.permission ++ ","
    ++ "\"row_owner\":" ++ jsonString c.rowOwner ++ ","
    ++ "\"actor_node\":" ++ jsonString c.actorNode ++ ","
    ++ "\"actor_agent\":" ++ jsonString c.actorAgent ++ ","
    ++ "\"peer_node\":" ++ jsonString c.peerNode ++ ","
    ++ "\"peer_agent\":" ++ jsonString c.peerAgent ++ ","
    ++ "\"expected_actor_node\":"
      ++ (c.expectedActorNode.map jsonString |>.getD "null") ++ ","
    ++ "\"expected_peer_node\":"
      ++ (c.expectedPeerNode.map jsonString |>.getD "null") ++ ","
    ++ "\"expected_actor_allowed\":"
      ++ boolString c.expectedActorAllowed ++ ","
    ++ "\"expected_peer_allowed\":"
      ++ boolString c.expectedPeerAllowed ++ ","
    ++ "\"same_node\":" ++ boolString c.sameNode ++ ","
    ++ "\"expected_decisions_equal\":"
      ++ boolString c.expectedDecisionsEqual
    ++ "}"

def identityPermissionCasesJson : String :=
  jsonArray ((identityPermissionCases ++ scopedSelectionCases).map identityPermissionCaseJson)

structure IdentityContract where
  name      : String
  statement : String
  enforced  : Bool
  trackedBy : String
  deriving Repr

def identityContracts : List IdentityContract :=
  [ { name      := "identity.respects_node_boundary"
    , statement :=
        "Target contract: resolve agents by (node_did, agent_id). " ++
        "For any two Agent rows b1, b2 with " ++
        "b1.node_did == b2.node_did, the runtime supplies the same " ++
        "Identity::Authenticated(did) as the actor for any DefraDB ACP " ++
        "check, so any DID-keyed permission decision returns identical " ++
        "results."
    , enforced  := false
    , trackedBy := "#1436"
    }
  ]

def identityContractJson (c : IdentityContract) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"statement\":" ++ jsonString c.statement ++ ","
    ++ "\"enforced\":" ++ boolString c.enforced ++ ","
    ++ "\"tracked_by\":" ++ jsonString c.trackedBy
    ++ "}"

def identityContractsJson : String :=
  jsonArray (identityContracts.map identityContractJson)

end Identity.Conformance
