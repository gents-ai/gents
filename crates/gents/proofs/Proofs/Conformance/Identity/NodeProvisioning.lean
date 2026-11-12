import Proofs.Store.Identity.NodeProvisioning
import Proofs.Store.Identity.NodeProvisioningConformance
import Proofs.Conformance.Family

namespace Conformance.NodeProvisioning

open Lean Identity Identity.NodeProvisioning

structure ExposureInput where
  name : String
  nodeCategory : Bool
  agentCategory : Bool
  deriving ToJson

@[conformance_family]
def agentCreationExposure : Family ExposureInput Bool where
  inputs :=
    [("agent-category", false, true), ("node-category", true, false),
      ("both-categories", true, true), ("no-category", false, false)].map
      fun (name, node, agent) =>
        ({ name, nodeCategory := node, agentCategory := agent } : ExposureInput)
  model := fun i => agentCommandExposed i.nodeCategory i.agentCategory

structure CreatorInput where
  name : String
  runtimeMatches : Bool
  signerHeld : Bool
  signerMatches : Bool
  nativeCreate : Bool
  nativeRelation : Bool
  deriving ToJson

def CreatorInput.observation (i : CreatorInput) : AgentCreatorObservation :=
  ⟨i.runtimeMatches, i.signerHeld, i.signerMatches, i.nativeCreate, i.nativeRelation⟩

private def creatorInput (c : String × AgentCreatorObservation) : CreatorInput :=
  { name := c.1, runtimeMatches := c.2.runtimeMatches, signerHeld := c.2.signerHeld
    signerMatches := c.2.signerMatches, nativeCreate := c.2.nativeCreate
    nativeRelation := c.2.nativeRelation }

/-- The served creation scenario holds a node whose native document ACP admits
the creator, so it can vary the runtime and the signer but not the native
create and relation decisions. -/
private def nativeAdmits (c : String × AgentCreatorObservation) : Bool :=
  c.2.nativeCreate && c.2.nativeRelation

@[conformance_family]
def agentCreator : Family CreatorInput Bool where
  inputs := (NodeProvisioningConformance.agentCreatorCases.filter nativeAdmits).map creatorInput
  model := fun i => agentCreatorMayPublish i.observation

/-- model-production-gap: no served creation scenario denies the native Agent
create or its parent relation to an otherwise authorized creator yet. -/
@[conformance_family]
def agentCreatorUnreached : Family CreatorInput Bool where
  inputs :=
    (NodeProvisioningConformance.agentCreatorCases.filter (!nativeAdmits ·)).map creatorInput
  model := fun i => agentCreatorMayPublish i.observation

end Conformance.NodeProvisioning
