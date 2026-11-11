import Proofs.Store.Identity.Operations
import Proofs.Conformance.Family

namespace Conformance.NetworkProtocol

open Lean Identity.Operations

structure ResourceInput where
  collection : String
  deriving ToJson

@[conformance_family]
def resource : Family ResourceInput (Option String) where
  inputs := ["Network", "NetworkEnrollmentRequest", "NetworkEnrollmentDecision",
    "NetworkEnrollmentRouteReceipt", "NetworkAuthorizationRevision",
    "EnrollmentRenewalIntent", "EnrollmentRenewalOffer", "NodeDirectoryEntry",
    "PeerRegistry", "AgentSession", "UnclassifiedCollection"].map fun collection => { collection }
  model := fun i => networkProtocolResource i.collection

end Conformance.NetworkProtocol
