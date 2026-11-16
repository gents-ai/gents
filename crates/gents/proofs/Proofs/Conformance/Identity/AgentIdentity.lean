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

structure SelectionOutput where
  actorDid : Option String
  peerDid : Option String
  actorAllowed : Bool
  peerAllowed : Bool
  sameNode : Bool
  decisionsEqual : Bool
  deriving ToJson

/-- model-production-gap: production has no node-scoped Agent selector over a registry
of Agent rows to replay; foreign-node, unknown and duplicate DIDs are rejected only by
the model's `findAgent?` before `/identity/decide`, which receives an already selected
DID. -/
@[conformance_family]
def permissionSelectionUnreached : Family PermissionInput SelectionOutput where
  inputs := (identityPermissionCases.filter (!resolvedSelection ·)).map PermissionInput.ofCase
  model := fun i =>
    let c := i.toCase
    { actorDid := c.actor.map (·.did), peerDid := c.peer.map (·.did),
      actorAllowed := c.allowed c.actor, peerAllowed := c.allowed c.peer,
      sameNode := c.actor.any fun a => c.peer.any (fun b => a.node == b.node),
      decisionsEqual := c.allowed c.actor == c.allowed c.peer }

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

@[conformance_family]
def desktopSubscription : Family CollectionInput LocalityOutput where
  inputs := localityCollections.map fun collection => { collection }
  model := fun i => { replicates := Identity.collectionReplicates i.collection }

end Conformance.AgentIdentity
