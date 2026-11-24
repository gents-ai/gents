import Proofs.Tools.SessionCustody
import Proofs.Conformance.Family

namespace Conformance.SessionCustody

open Lean Tools.SessionCustody

structure RelationChangeInput where
  collection : String
  revoke : Bool
  deriving ToJson

/-- The policy tool's relationship change on the session collection, the
documents beneath a session, the Agent and Node documents and an application
collection, as a grant and as a removal. -/
@[conformance_family]
def relationChange : Family RelationChangeInput Bool where
  inputs := (["AgentSession", "AgentRequest", "AgentMessage", "AgentSessionGoalControl",
      "Agent", "Node", "Skill", "Shipment"].flatMap fun collection =>
    [false, true].map fun revoke => { collection, revoke })
  model := fun i => relationChangeOffered i.collection i.revoke

end Conformance.SessionCustody
