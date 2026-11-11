import Proofs.Store.Identity.Household
import Proofs.Conformance.Family

namespace Conformance.CreatedAgentAccess

open Lean Identity

structure AccessInput where
  actor : String
  permission : String
  deriving ToJson

@[conformance_family]
def cases : Family AccessInput Bool where
  inputs := [("creator", "read"), ("creator", "update"), ("user", "read"),
    ("user", "update"), ("sibling", "read"), ("sibling", "update")].map fun (actor, permission) =>
      { actor, permission }
  model := fun i =>
    Household.allowed 5 Household.agentCreatedAgentFixture i.actor "agents" "child" i.permission

end Conformance.CreatedAgentAccess
