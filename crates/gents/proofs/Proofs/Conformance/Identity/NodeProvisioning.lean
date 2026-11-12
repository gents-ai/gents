import Proofs.Store.Identity.NodeProvisioning
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

deriving instance ToJson for NacStatus

structure NacInput where
  status : NacStatus
  deriving ToJson

structure NacOutput where
  mayStart : Bool
  deriving ToJson

private def nacModel (i : NacInput) : NacOutput := { mayStart := runtimeMayStart i.status }

/-- A runtime start opens its node with NAC enabled, and an admin can disable
it on the running node, which native persists across a restart: these are the
statuses a production start reaches. -/
@[conformance_family]
def runtimeNac : Family NacInput NacOutput where
  inputs := [{ status := .enabled }, { status := .disabledTemporarily }]
  model := nacModel

/-- model-production-gap: every production node open enables NAC, so no
production path observes a node whose NAC was never configured. -/
@[conformance_family]
def runtimeNacUnreached : Family NacInput NacOutput where
  inputs := [{ status := .notConfigured }]
  model := nacModel

end Conformance.NodeProvisioning
