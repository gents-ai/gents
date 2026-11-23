import Proofs.Conformance.Identity.Conformance
import Proofs.Conformance.Family

namespace Conformance.AgentIdentity

open Lean Identity.Conformance

deriving instance ToJson for Identity.Conformance.DerivationInput

structure DerivationCase where
  name : String
  left : DerivationInput
  right : DerivationInput
  deriving ToJson

structure DerivationOutput where
  sameDid : Bool
  deriving ToJson

@[conformance_family]
def derivation : Family DerivationCase DerivationOutput where
  inputs := derivationCases.map fun (name, left, right) => { name, left, right }
  model := fun i => { sameDid := sameDerivedDid i.left i.right }

structure AgentWire where
  label : String
  did : String
  creationNonce : String
  node : String
  enabled : Bool
  deriving ToJson

def AgentWire.ofCase (c : AgentCase) : AgentWire :=
  { label := c.label, did := c.did, creationNonce := c.creationId, node := c.node,
    enabled := c.enabled }

def AgentWire.toCase (w : AgentWire) : AgentCase :=
  ⟨w.label, w.did, w.creationNonce, w.node, w.enabled⟩

structure PermissionInput where
  name : String
  nodes : List NodeCase
  agents : List AgentWire
  grants : List PermissionGrantCase
  permission : String
  rowOwner : String
  actorNode : String
  requestedActorDid : String
  peerNode : String
  requestedPeerDid : String
  deriving ToJson

def PermissionInput.ofCase (c : IdentityPermissionCase) : PermissionInput :=
  { name := c.name, nodes := c.nodes, agents := c.agents.map AgentWire.ofCase,
    grants := c.grants, permission := c.permission, rowOwner := c.rowOwner,
    actorNode := c.actorNode, requestedActorDid := c.actorDid, peerNode := c.peerNode,
    requestedPeerDid := c.peerDid }

def PermissionInput.toCase (i : PermissionInput) : IdentityPermissionCase :=
  ⟨i.name, i.nodes, i.agents.map AgentWire.toCase, i.grants, i.permission, i.rowOwner,
    i.actorNode, i.requestedActorDid, i.peerNode, i.requestedPeerDid⟩

def resolvedSelection (c : IdentityPermissionCase) : Bool :=
  c.actor.isSome && c.peer.isSome

structure DecisionOutput where
  actorAllowed : Bool
  peerAllowed : Bool
  decisionsEqual : Bool
  deriving ToJson

/-- `/identity/decide` answers for an actor DID its caller already selected. Only the
cases whose actor and peer both select one node-local Agent can be asked of it; the
selected DID is then the requested DID. -/
@[conformance_family]
def permissionDecide : Family PermissionInput DecisionOutput where
  inputs := (identityPermissionCases.filter resolvedSelection).map PermissionInput.ofCase
  model := fun i =>
    let c := i.toCase
    { actorAllowed := c.allowed c.actor, peerAllowed := c.allowed c.peer,
      decisionsEqual := c.allowed c.actor == c.allowed c.peer }

/-- The node-scoped selector `findAgent?` is the caller's: `/identity/decide`
receives an already selected DID. A foreign-node, unknown or duplicate DID
selects no Agent, and an unselected Agent is denied. -/
theorem unselected_agent_is_denied :
    (identityPermissionCases.filter (!resolvedSelection ·)).all (fun c =>
      (c.actor.isSome || !c.allowed c.actor) && (c.peer.isSome || !c.allowed c.peer)) ∧
    (identityPermissionCases.filter (!resolvedSelection ·)).length = 3 := by
  native_decide

structure CollectionInput where
  collection : String
  deriving ToJson

structure LocalityOutput where
  replicates : Bool
  deriving ToJson

/-- The protocol allowlist, the representative node-local families, and one application
collection no schema declares. -/
def localityCollections : List String :=
  Identity.replicatedProtocolCollections ++ Identity.nodeLocalCollections ++ ["PackPrivateData"]

example : Identity.schemaActor { did := runtimeDid, displayName := none, enabled := true }
    .createSchema = runtimeDid := by rfl

@[conformance_family]
def collectionLocality : Family CollectionInput LocalityOutput where
  inputs := localityCollections.map fun collection => { collection }
  model := fun i => { replicates := Identity.collectionReplicates i.collection }

@[conformance_family]
def replicationAdmission : Family CollectionInput LocalityOutput where
  inputs := localityCollections.map fun collection => { collection }
  model := fun i => { replicates := Identity.collectionReplicates i.collection }

end Conformance.AgentIdentity
